//! Panes: `/v1/panes` and `/v1/panes/{id}` (spec §4, §6).

use std::collections::HashSet;
use std::path::PathBuf;

use agentdeck_core::ws::Entity;
use agentdeck_core::{Agent, AgentState, PaneId, PaneKind};
use dropshot::{
    Body, ClientErrorStatusCode, FreeformBody, HttpError, HttpResponseAccepted,
    HttpResponseCreated, HttpResponseDeleted, HttpResponseOk, Path, Query, RequestContext,
    TypedBody, endpoint,
};

use super::{ApiContext, Authorized, IdPath, client_error};
use crate::agents::{self, codex, delivery};
use crate::db;
use crate::model::{
    Comment, InputAccepted, InputWhen, NewPane, Pane, PaneInput, PanePatch, PaneRespawn, PaneRun,
    TailQuery,
};
use crate::terminal::env::default_shell;
use crate::terminal::pane::{Launch, SpawnError, When};
use crate::terminal::tail::TAIL_CAPACITY;

/// What a new pane runs, from its request (spec §4, §6).
#[derive(Debug, PartialEq, Eq)]
enum Command {
    /// A shell, or an `other` agent: this argv.
    Given(Vec<String>),
    /// Claude Code or Codex, which agentdeck launches with a session and a launch prompt.
    Agent(Agent),
}

/// Checks a new pane's kind, agent and argv against each other:
/// - a shell names no agent, and runs `argv`, else the daemon's `$SHELL`, else `/bin/sh`;
/// - an agent pane names its agent;
/// - an `other` agent runs `argv`, which it must have;
/// - `claude` and `codex` take no `argv`: agentdeck builds it.
fn command(new: &NewPane) -> Result<Command, HttpError> {
    let unsupported = |message: &str| {
        client_error(
            ClientErrorStatusCode::BAD_REQUEST,
            "unsupported_kind",
            message.into(),
        )
    };
    let invalid_argv = |message: &str| {
        client_error(
            ClientErrorStatusCode::BAD_REQUEST,
            "invalid_argv",
            message.into(),
        )
    };
    let given = |argv: &Vec<String>| {
        if argv.first().is_none_or(|program| program.is_empty()) {
            Err(invalid_argv("argv must name a program"))
        } else {
            Ok(Command::Given(argv.clone()))
        }
    };
    match (new.kind, new.agent, &new.argv) {
        (PaneKind::Shell, Some(_), _) => Err(unsupported("a shell pane has no agent")),
        (PaneKind::Shell, None, Some(argv)) => given(argv),
        (PaneKind::Shell, None, None) => Ok(Command::Given(vec![default_shell()])),
        (PaneKind::Agent, None, _) => Err(unsupported("an agent pane names its agent")),
        (PaneKind::Agent, Some(Agent::Other), Some(argv)) => given(argv),
        (PaneKind::Agent, Some(Agent::Other), None) => {
            Err(invalid_argv("an `other` agent pane needs its argv"))
        }
        (PaneKind::Agent, Some(_), Some(_)) => Err(invalid_argv(
            "claude and codex panes are launched by agentdeck: leave argv out",
        )),
        (PaneKind::Agent, Some(agent), None) => Ok(Command::Agent(agent)),
    }
}

/// A pane's working directory must be an existing absolute directory.
fn check_cwd(cwd: &str) -> Result<PathBuf, HttpError> {
    let path = PathBuf::from(cwd);
    if path.is_absolute() && path.is_dir() {
        Ok(path)
    } else {
        Err(client_error(
            ClientErrorStatusCode::BAD_REQUEST,
            "invalid_dir",
            format!("{cwd} is not an existing absolute directory"),
        ))
    }
}

fn exited(id: PaneId) -> HttpError {
    client_error(
        ClientErrorStatusCode::CONFLICT,
        "exited",
        format!("pane {id}'s process has exited"),
    )
}

/// The stored pane, with the live title, state, exit code and hosted agent while this daemon
/// holds it (they reach the database a little later).
fn with_live_status(context: &ApiContext, mut pane: Pane) -> Pane {
    if let Some(live) = context.terminals.get(pane.id) {
        let status = live.status();
        pane.title = status.title;
        pane.state = status.state;
        pane.exit_code = status.exit_code;
        pane.hosted_agent = status.hosted;
    }
    pane
}

/// Spawns a pane in a tab and starts its process (spec §6). The caller places the pane in
/// the tab's layout.
///
/// `claude` and `codex` panes get agentdeck's MCP server (`agentdeck mcp`) and start with a
/// launch prompt: the linked task's title, its `Task id:` and how to report back on it with
/// the MCP tools, and its body; then every instruction for the pane's workspace and the task's
/// comments no agent has had yet, which are then marked delivered to the pane. Claude Code
/// starts on a session id agentdeck chooses; a Codex pane's is found once Codex writes its
/// session file, and `session_id` is set then.
#[endpoint { method = POST, path = "/v1/panes" }]
pub(crate) async fn pane_create(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    body: TypedBody<NewPane>,
) -> Result<HttpResponseCreated<Pane>, HttpError> {
    let context = rqctx.context();
    let new = body.into_inner();
    let command = command(&new)?;
    let cwd = check_cwd(&new.cwd)?;
    let claims = context.claims.clone();
    let (pane, workspace, prompted) = context
        .db
        .call(move |c| {
            let (argv, session_id, comments) = match command {
                Command::Given(argv) => (argv, None, Vec::new()),
                Command::Agent(agent) => {
                    let workspace = db::panes::workspace_for(c, &new)?;
                    let task = new.task_id.map(|id| db::tasks::get(c, id)).transpose()?;
                    let instructions = db::instructions::list(c, workspace)?;
                    let comments = match new.task_id {
                        Some(task) => delivery::for_launch_prompt(c, &claims, task)?,
                        None => Vec::new(),
                    };
                    let prompt = agents::launch_prompt(task.as_ref(), &instructions, &comments);
                    let launch = agents::launch(agent, prompt, &agents::cli())
                        .expect("claude and codex launch");
                    (launch.argv, launch.session_id, comments)
                }
            };
            let (pane, workspace) = db::panes::create(c, &new, &argv, session_id.as_deref())?;
            Ok((pane, workspace, comments))
        })
        .await?;

    let launch = Launch::pane(pane.id, workspace, pane.task_id, pane.argv.clone(), cwd);
    let codex_sessions = codex_sessions(pane.agent, &launch);
    let spawned = match pane.agent {
        Some(agent) => context.terminals.spawn(pane.id, launch, agent.as_str()),
        None => context.terminals.spawn_shell(pane.id, launch),
    };
    if let Err(err) = spawned {
        let id = pane.id;
        tracing::warn!("pane {id} couldn't start: {}", err.detail());
        delivery::launch_failed(&context.claims, &prompted);
        context.db.call(move |c| db::panes::delete(c, id)).await?;
        return Err(spawn_error(err));
    }
    context.events.changed(Entity::Pane, pane.id);
    let delivered = prompted.iter().map(|comment| comment.id).collect();
    delivery::mark_delivered(
        &context.db,
        &context.claims,
        &context.events,
        delivered,
        pane.id,
    )
    .await;
    watch_codex(context, pane.id, codex_sessions);
    Ok(HttpResponseCreated(with_live_status(context, pane)))
}

/// For a Codex launch, its sessions dir and the session files already there, to find the one
/// it writes ([`watch_codex`]).
fn codex_sessions(agent: Option<Agent>, launch: &Launch) -> Option<(PathBuf, HashSet<PathBuf>)> {
    (agent == Some(Agent::Codex))
        .then(|| codex::sessions_dir(&launch.env))
        .flatten()
        .map(|dir| {
            let before = codex::session_files(&dir)
                .into_iter()
                .map(|(path, _)| path)
                .collect();
            (dir, before)
        })
}

/// Finds the session a Codex pane just launched writes, and saves it (spec §6).
fn watch_codex(context: &ApiContext, pane: PaneId, sessions: Option<(PathBuf, HashSet<PathBuf>)>) {
    if let Some((dir, before)) = sessions {
        codex::watch(
            context.db.clone(),
            context.terminals.clone(),
            context.events.clone(),
            pane,
            dir,
            before,
        );
    }
}

/// The 400 a pane's program that can't start answers with, or a 500 for a PTY failure.
fn spawn_error(err: SpawnError) -> HttpError {
    match err {
        SpawnError::NotOnPath { .. } | SpawnError::Command(_) => client_error(
            ClientErrorStatusCode::BAD_REQUEST,
            "invalid_argv",
            err.to_string(),
        ),
        SpawnError::Pty(_) => HttpError::for_internal_error(err.to_string()),
    }
}

/// One pane, with its live title and state.
#[endpoint { method = GET, path = "/v1/panes/{id}" }]
pub(crate) async fn pane_get(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<PaneId>>,
) -> Result<HttpResponseOk<Pane>, HttpError> {
    let context = rqctx.context();
    let id = path.into_inner().id;
    let pane = context.db.call(move |c| db::panes::get(c, id)).await?;
    Ok(HttpResponseOk(with_live_status(context, pane)))
}

/// Links the pane to a task in its workspace that isn't done, or unlinks it (`task_id`
/// null). Linking a live agent pane to another task first tells its agent the task itself
/// (its title, id, how to report back on it, and body), then types in the task's comments no
/// agent has had yet, each once the agent is idle or done, as a new comment would be (spec
/// §9). Whatever was still waiting to be typed for the task it leaves is dropped from the
/// pane, and a comment held for it moves on to that task's next live agent pane.
#[endpoint { method = PATCH, path = "/v1/panes/{id}" }]
pub(crate) async fn pane_update(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<PaneId>>,
    body: TypedBody<PanePatch>,
) -> Result<HttpResponseOk<Pane>, HttpError> {
    let context = rqctx.context();
    let id = path.into_inner().id;
    let task = body.into_inner().task_id;
    let (pane, before) = context
        .db
        .call(move |c| db::panes::set_task(c, id, task))
        .await?;
    context.terminals.link(id, task);
    context.events.changed(Entity::Pane, id);
    if before != task {
        for linked in [before, task].into_iter().flatten() {
            context.events.changed(Entity::Task, linked);
        }
        if let Some(task) = task {
            delivery::deliver_on_link(
                &context.db,
                &context.claims,
                &context.terminals,
                &context.events,
                task,
                id,
            )
            .await;
        }
    }
    Ok(HttpResponseOk(with_live_status(context, pane)))
}

/// What a respawn runs in a pane, decided from its row.
enum Respawn {
    /// Its agent's resume command on its session, or an `other` agent's argv: the row's
    /// `argv` and `session_id` stay.
    Again(Agent, Vec<String>),
    /// Claude Code or Codex with no known session: a fresh launch, as a new pane's.
    Fresh(Agent),
    /// A shell, which the pane becomes.
    Shell,
}

/// Starts an exited pane again in place, keeping its id, its place in the tab's layout, its
/// `cwd` and its task link (spec §4). `run: "agent"`, for an agent pane only
/// (`unsupported_kind`), runs its agent again: claude and codex resume their session when it
/// is known, else start fresh with a new launch prompt; an `other` agent runs its argv again.
/// `run: "shell"` turns the pane into a shell pane running `$SHELL`, with hosting on. The
/// state starts over. `running` (409) while its process hasn't exited; `invalid_dir` when its
/// `cwd` is gone; `invalid_argv` when the program can't start, and the pane stays exited.
#[endpoint { method = POST, path = "/v1/panes/{id}/respawn" }]
pub(crate) async fn pane_respawn(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<PaneId>>,
    body: TypedBody<PaneRespawn>,
) -> Result<HttpResponseOk<Pane>, HttpError> {
    let context = rqctx.context();
    let id = path.into_inner().id;
    let run = body.into_inner().run;
    let row = context.db.call(move |c| db::panes::get(c, id)).await?;
    let state = context
        .terminals
        .get(id)
        .map_or(row.state, |live| live.status().state);
    if state != AgentState::Exited {
        return Err(client_error(
            ClientErrorStatusCode::CONFLICT,
            "running",
            format!("pane {id}'s process is still running"),
        ));
    }
    let cwd = check_cwd(&row.cwd)?;
    let respawn = match (run, row.kind, row.agent) {
        (PaneRun::Shell, ..) => Respawn::Shell,
        (PaneRun::Agent, PaneKind::Agent, Some(agent)) => {
            match agents::resume_argv(Some(agent), row.session_id.as_deref(), &agents::cli()) {
                Some(argv) => Respawn::Again(agent, argv),
                None if agent == Agent::Other => Respawn::Again(agent, row.argv.clone()),
                None => Respawn::Fresh(agent),
            }
        }
        (PaneRun::Agent, ..) => {
            return Err(client_error(
                ClientErrorStatusCode::BAD_REQUEST,
                "unsupported_kind",
                "only an agent pane runs its agent again".into(),
            ));
        }
    };
    let claims = context.claims.clone();
    let fresh = matches!(respawn, Respawn::Fresh(_));
    let (pane, workspace, agent, argv, prompted) = context
        .db
        .call(move |c| {
            let workspace = db::panes::workspace_of(c, id)?;
            let (agent, argv, prompted): (_, _, Vec<Comment>) = match respawn {
                Respawn::Shell => {
                    let shell = vec![default_shell()];
                    db::panes::become_shell(c, id, &shell)?;
                    db::panes::respawned(c, id, None)?;
                    (None, shell, Vec::new())
                }
                Respawn::Fresh(agent) => {
                    let row = db::panes::get(c, id)?;
                    let task = row
                        .task_id
                        .map(|task| db::tasks::get(c, task))
                        .transpose()?;
                    let instructions = db::instructions::list(c, workspace)?;
                    let comments = match row.task_id {
                        Some(task) => delivery::for_launch_prompt(c, &claims, task)?,
                        None => Vec::new(),
                    };
                    let prompt = agents::launch_prompt(task.as_ref(), &instructions, &comments);
                    let launch = agents::launch(agent, prompt, &agents::cli())
                        .expect("claude and codex launch");
                    let started = (launch.argv.as_slice(), launch.session_id.as_deref());
                    db::panes::respawned(c, id, Some(started))?;
                    (Some(agent), launch.argv, comments)
                }
                Respawn::Again(agent, argv) => {
                    db::panes::respawned(c, id, None)?;
                    (Some(agent), argv, Vec::new())
                }
            };
            Ok((db::panes::get(c, id)?, workspace, agent, argv, prompted))
        })
        .await?;

    let launch = Launch::pane(id, workspace, pane.task_id, argv, cwd);
    let codex_sessions = codex_sessions(agent.filter(|_| fresh), &launch);
    let spawned = match agent {
        Some(agent) => context.terminals.spawn(id, launch, agent.as_str()),
        None => context.terminals.spawn_shell(id, launch),
    };
    if let Err(err) = spawned {
        tracing::warn!("pane {id} couldn't start again: {}", err.detail());
        delivery::launch_failed(&context.claims, &prompted);
        let title = pane.title.clone();
        context
            .db
            .call(move |c| db::panes::set_exited(c, id, None, &title))
            .await?;
        context.events.changed(Entity::Pane, id);
        return Err(spawn_error(err));
    }
    tracing::info!("respawned pane {id}");
    context.events.changed(Entity::Pane, id);
    let delivered = prompted.iter().map(|comment| comment.id).collect();
    delivery::mark_delivered(&context.db, &context.claims, &context.events, delivered, id).await;
    watch_codex(context, id, codex_sessions);
    Ok(HttpResponseOk(with_live_status(context, pane)))
}

/// Kills a pane's process (SIGHUP, then SIGKILL after 3 s) and deletes the pane. The caller
/// takes it out of the tab's layout.
#[endpoint { method = DELETE, path = "/v1/panes/{id}" }]
pub(crate) async fn pane_delete(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<PaneId>>,
) -> Result<HttpResponseDeleted, HttpError> {
    let context = rqctx.context();
    let id = path.into_inner().id;
    context.db.call(move |c| db::panes::delete(c, id)).await?;
    context.terminals.kill(id);
    context.events.changed(Entity::Pane, id);
    Ok(HttpResponseDeleted())
}

/// Types text into the pane, as is or submitted (`submit`), now or once it is idle (`when`).
#[endpoint { method = POST, path = "/v1/panes/{id}/input" }]
pub(crate) async fn pane_input(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<PaneId>>,
    body: TypedBody<PaneInput>,
) -> Result<HttpResponseAccepted<InputAccepted>, HttpError> {
    let context = rqctx.context();
    let id = path.into_inner().id;
    let input = body.into_inner();
    let Some(pane) = context.terminals.get(id) else {
        context.db.call(move |c| db::panes::get(c, id)).await?;
        return Err(exited(id));
    };
    let when = match input.when {
        InputWhen::Now => When::Now,
        InputWhen::Idle => When::Idle,
    };
    let queued = pane
        .input(input.text.as_bytes(), input.submit, when)
        .map_err(|_| exited(id))?;
    Ok(HttpResponseAccepted(InputAccepted { queued }))
}

/// The pane's raw output: the last `bytes` bytes of the 256 KiB it keeps. Empty for a pane
/// whose process isn't running in this daemon.
#[endpoint { method = GET, path = "/v1/panes/{id}/tail" }]
pub(crate) async fn pane_tail(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<PaneId>>,
    query: Query<TailQuery>,
) -> Result<HttpResponseOk<FreeformBody>, HttpError> {
    let context = rqctx.context();
    let id = path.into_inner().id;
    let bytes = query
        .into_inner()
        .bytes
        .map_or(TAIL_CAPACITY, |n| usize::try_from(n).unwrap_or(usize::MAX));
    let tail = match context.terminals.get(id) {
        Some(pane) => pane.tail(bytes),
        None => {
            context.db.call(move |c| db::panes::get(c, id)).await?;
            Vec::new()
        }
    };
    Ok(HttpResponseOk(Body::from(tail).into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    mod command {
        use super::*;

        fn new(kind: PaneKind, agent: Option<Agent>, argv: Option<&[&str]>) -> NewPane {
            NewPane {
                tab_id: uuid::Uuid::now_v7().into(),
                kind,
                agent,
                argv: argv.map(|argv| argv.iter().map(|arg| (*arg).to_owned()).collect()),
                cwd: "/".into(),
                task_id: None,
            }
        }

        fn code(result: Result<Command, HttpError>) -> String {
            result
                .expect_err("a rejected pane")
                .error_code
                .expect("an error code")
        }

        #[test]
        fn should_keep_a_shells_argv() {
            let new = new(PaneKind::Shell, None, Some(&["sh", "-c", "env"]));
            assert_eq!(
                command(&new).unwrap(),
                Command::Given(vec!["sh".into(), "-c".into(), "env".into()])
            );
        }

        #[test]
        fn should_reject_an_argv_without_a_program() {
            assert_eq!(
                code(command(&new(PaneKind::Shell, None, Some(&[])))),
                "invalid_argv"
            );
            assert_eq!(
                code(command(&new(PaneKind::Shell, None, Some(&[""])))),
                "invalid_argv"
            );
        }

        #[test]
        fn should_reject_a_shell_with_an_agent() {
            let new = new(PaneKind::Shell, Some(Agent::Claude), None);
            assert_eq!(code(command(&new)), "unsupported_kind");
        }

        #[test]
        fn should_reject_an_agent_pane_without_its_agent() {
            let new = new(PaneKind::Agent, None, None);
            assert_eq!(code(command(&new)), "unsupported_kind");
        }

        #[test]
        fn should_run_an_other_agents_argv() {
            let new = new(PaneKind::Agent, Some(Agent::Other), Some(&["aider"]));
            assert_eq!(command(&new).unwrap(), Command::Given(vec!["aider".into()]));
        }

        #[test]
        fn should_reject_an_other_agent_without_argv() {
            let new = new(PaneKind::Agent, Some(Agent::Other), None);
            assert_eq!(code(command(&new)), "invalid_argv");
        }

        #[test]
        fn should_launch_claude_and_codex_itself() {
            for agent in [Agent::Claude, Agent::Codex] {
                let new = new(PaneKind::Agent, Some(agent), None);
                assert_eq!(command(&new).unwrap(), Command::Agent(agent));
            }
        }

        #[test]
        fn should_reject_an_argv_for_claude_or_codex() {
            for agent in [Agent::Claude, Agent::Codex] {
                let new = new(PaneKind::Agent, Some(agent), Some(&["claude", "--yolo"]));
                assert_eq!(code(command(&new)), "invalid_argv");
            }
        }
    }
}
