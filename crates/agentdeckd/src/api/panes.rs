//! Panes: `/v1/panes` and `/v1/panes/{id}` (spec §4, §6).

use std::collections::HashSet;
use std::path::{Path as FsPath, PathBuf};

use agentdeck_core::ws::Entity;
use agentdeck_core::{Agent, AgentState, PaneId, PaneKind};
use dropshot::{
    Body, ClientErrorStatusCode, FreeformBody, HttpError, HttpResponseAccepted,
    HttpResponseCreated, HttpResponseDeleted, HttpResponseOk, HttpResponseUpdatedNoContent, Path,
    Query, RequestContext, TypedBody, endpoint,
};

use super::{ApiContext, Authorized, IdPath, client_error};
use crate::agents::restart::{self, Respawn, RestartError, Restarter};
use crate::agents::{self, codex};
use crate::db;
use crate::model::{
    InputAccepted, InputWhen, NewPane, Pane, PaneInput, PaneRespawn, PaneRun, TailQuery,
};
use crate::terminal::env::default_shell;
use crate::terminal::pane::{Launch, NothingToStop, SpawnError, When};
use crate::terminal::tail::TAIL_CAPACITY;

/// What a new pane runs, from its request (spec §4, §6).
#[derive(Debug, PartialEq, Eq)]
enum Command {
    /// A shell, or an `other` agent: this argv.
    Given(Vec<String>),
    /// Claude Code or Codex, which agentdeck launches with a session and no prompt.
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

/// The stored pane, with the live title, state, exit code, hosted agent and folder while this
/// daemon holds it (they reach the database a little later).
fn with_live_status(context: &ApiContext, mut pane: Pane) -> Pane {
    if let Some(live) = context.terminals.get(pane.id) {
        let status = live.status();
        pane.title = status.title;
        pane.state = status.state;
        pane.exit_code = status.exit_code;
        pane.hosted_agent = status.hosted;
        pane.folder = live.folder().to_string_lossy().into_owned();
    }
    pane
}

/// Spawns a pane in a tab and starts its process (spec §6). The caller places the pane in
/// the tab's layout.
///
/// `claude` and `codex` panes start with no prompt: an agent reads its own CLAUDE.md or
/// AGENTS.md, and work reaches it by pasting or `input`. Claude Code starts on a session id
/// agentdeck chooses; a Codex pane's is found once Codex writes its session file, and
/// `session_id` is set then.
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
    let (pane, workspace) = context
        .db
        .call(move |c| {
            let (argv, session_id) = match command {
                Command::Given(argv) => (argv, None),
                Command::Agent(agent) => {
                    let launch = agents::launch(agent).expect("claude and codex launch");
                    (launch.argv, launch.session_id)
                }
            };
            db::panes::create(c, &new, &argv, session_id.as_deref())
        })
        .await?;

    let launch = Launch::pane(pane.id, workspace, pane.argv.clone(), cwd);
    let codex_sessions = codex::sessions_before(pane.agent, &launch);
    let spawned = match pane.agent {
        Some(agent) => context.terminals.spawn(pane.id, launch, agent.as_str()),
        None => context.terminals.spawn_shell(pane.id, launch),
    };
    if let Err(err) = spawned {
        let id = pane.id;
        tracing::warn!("pane {id} couldn't start: {}", err.detail());
        context.db.call(move |c| db::panes::delete(c, id)).await?;
        return Err(spawn_error(err));
    }
    context.events.changed(Entity::Pane, pane.id);
    watch_codex(context, pane.id, codex_sessions);
    Ok(HttpResponseCreated(with_live_status(context, pane)))
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

/// One pane, with its live title, state and current folder.
///
/// The folder is read now: the folder its shell last reported with OSC 7, else its process's
/// working directory, else the one last known. A pane whose folder moved is saved there, and
/// `changed` goes out for it.
#[endpoint { method = GET, path = "/v1/panes/{id}" }]
pub(crate) async fn pane_get(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<PaneId>>,
) -> Result<HttpResponseOk<Pane>, HttpError> {
    let context = rqctx.context();
    let id = path.into_inner().id;
    context.terminals.refresh_folder(id).await;
    let pane = context.db.call(move |c| db::panes::get(c, id)).await?;
    Ok(HttpResponseOk(with_live_status(context, pane)))
}

/// Starts an exited pane again in place, keeping its id and its place in the layout.
///
/// `run: "agent"`, for an agent pane only (`unsupported_kind`), runs its agent
/// again: claude and codex resume their session when it is known, else start fresh with a new
/// session; an `other` agent runs its argv again. A resume that exits non-zero within 10 s,
/// as one of a session the agent can no longer find does, is followed by a fresh start, once.
/// `run: "shell"` turns the pane into a shell pane running `$SHELL`, with hosting on (spec §4).
///
/// A resume runs in the pane's `cwd`, the folder its session started in, which Claude Code
/// and Codex need; once that folder is gone, the agent starts fresh instead. Anything else
/// starts in the pane's last known folder, else its `cwd`, else `~`. The folder it starts in
/// becomes its `cwd` and its `folder`, and the state starts over. `running` (409) while its
/// process hasn't exited; `invalid_dir` when none of those folders is there; `invalid_argv`
/// when the program can't start, and the pane stays exited.
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
    let row = with_live_status(context, row);
    if row.state != AgentState::Exited {
        return Err(client_error(
            ClientErrorStatusCode::CONFLICT,
            "running",
            format!("pane {id}'s process is still running"),
        ));
    }
    let respawn = match (run, row.kind, row.agent) {
        (PaneRun::Shell, ..) => Respawn::Shell,
        (PaneRun::Agent, PaneKind::Agent, Some(agent)) => {
            match agents::resume_argv(Some(agent), row.session_id.as_deref()) {
                Some(argv) => Respawn::Resume(agent, argv),
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
    let resume = matches!(respawn, Respawn::Resume(..));
    let home = restart::home();
    let (cwd, folder) = (FsPath::new(&row.cwd), FsPath::new(&row.folder));
    let Some((respawn, start_in)) = restart::place(respawn, cwd, folder, home.as_deref()) else {
        return Err(client_error(
            ClientErrorStatusCode::BAD_REQUEST,
            "invalid_dir",
            format!(
                "none of {}, {} and ~ is an existing directory",
                row.folder, row.cwd
            ),
        ));
    };
    if resume && matches!(respawn, Respawn::Fresh(_)) {
        tracing::info!(
            "pane {id}'s session started in {}, which is gone: starting it afresh in {}",
            row.cwd,
            start_in.display()
        );
    }
    let restarter = Restarter {
        db: context.db.clone(),
        terminals: context.terminals.clone(),
        events: context.events.clone(),
    };
    let pane = restarter
        .respawn(id, respawn, start_in)
        .await
        .map_err(|err| match err {
            RestartError::Db(err) => err.into(),
            RestartError::Spawn(err) => spawn_error(err),
        })?;
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

/// Stops the pane's agent and keeps the pane, with its place in the layout and its session:
/// SIGHUP, then SIGKILL after 3 s, as `DELETE` does. An agent pane's process
/// is stopped, and the pane reads as exited once it has, so `POST /v1/panes/{id}/respawn`
/// (Restart agent) resumes the session later and a restarted daemon doesn't resume it. A
/// shell pane's hosted agent is stopped, its process group alone, and the shell keeps
/// running. Answers once the signal is sent: the `state` and `exit` events, or `changed` for
/// a hosted agent, follow. `exited` (409) when the process has already exited; `no_agent`
/// (409) for a shell pane hosting no agent.
#[endpoint { method = POST, path = "/v1/panes/{id}/stop" }]
pub(crate) async fn pane_stop(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<PaneId>>,
) -> Result<HttpResponseUpdatedNoContent, HttpError> {
    let context = rqctx.context();
    let id = path.into_inner().id;
    match context.terminals.stop(id) {
        Ok(()) => {
            tracing::info!("stopping pane {id}'s agent");
            Ok(HttpResponseUpdatedNoContent())
        }
        Err(NothingToStop::Exited) => {
            context.db.call(move |c| db::panes::get(c, id)).await?;
            Err(exited(id))
        }
        Err(NothingToStop::NoAgent) => Err(client_error(
            ClientErrorStatusCode::CONFLICT,
            "no_agent",
            format!("pane {id} is a shell hosting no agent: there is no agent to stop"),
        )),
    }
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
