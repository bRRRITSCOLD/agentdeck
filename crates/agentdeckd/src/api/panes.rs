//! Panes: `/v1/panes` and `/v1/panes/{id}` (spec §4, §6).

use std::collections::HashSet;
use std::path::PathBuf;

use agentdeck_core::ws::Entity;
use agentdeck_core::{Agent, PaneId, PaneKind};
use dropshot::{
    Body, ClientErrorStatusCode, FreeformBody, HttpError, HttpResponseAccepted,
    HttpResponseCreated, HttpResponseDeleted, HttpResponseOk, Path, Query, RequestContext,
    TypedBody, endpoint,
};

use super::{ApiContext, Authorized, IdPath, client_error};
use crate::agents::{self, codex, delivery};
use crate::db;
use crate::model::{InputAccepted, InputWhen, NewPane, Pane, PaneInput, TailQuery};
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

/// The stored pane, with the live title, state and exit code while this daemon holds it
/// (they reach the database a little later).
fn with_live_status(context: &ApiContext, mut pane: Pane) -> Pane {
    if let Some(live) = context.terminals.get(pane.id) {
        let status = live.status();
        pane.title = status.title;
        pane.state = status.state;
        pane.exit_code = status.exit_code;
    }
    pane
}

/// Spawns a pane in a tab and starts its process (spec §6). The caller places the pane in
/// the tab's layout.
///
/// `claude` and `codex` panes start with a launch prompt: the linked task's title and body,
/// then the unread notes on the task and on the pane's workspace, which are then marked read
/// by the pane. Claude Code starts on a session id agentdeck chooses; a Codex pane's is
/// found once Codex writes its session file, and `session_id` is set then.
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
    let rules = new.agent.map_or("shell", Agent::as_str);
    let (pane, workspace, prompted) = context
        .db
        .call(move |c| {
            let (argv, session_id, notes) = match command {
                Command::Given(argv) => (argv, None, Vec::new()),
                Command::Agent(agent) => {
                    let workspace = db::panes::workspace_for(c, &new)?;
                    let task = new.task_id.map(|id| db::tasks::get(c, id)).transpose()?;
                    let notes = db::notes::unread_for_launch(c, workspace, new.task_id)?;
                    let prompt = agents::launch_prompt(task.as_ref(), &notes);
                    let launch = agents::launch(agent, prompt).expect("claude and codex launch");
                    (launch.argv, launch.session_id, notes)
                }
            };
            let (pane, workspace) = db::panes::create(c, &new, &argv, session_id.as_deref())?;
            Ok((pane, workspace, notes))
        })
        .await?;

    let launch = Launch::pane(pane.id, workspace, pane.argv.clone(), cwd);
    let codex_sessions = (pane.agent == Some(Agent::Codex))
        .then(|| codex::sessions_dir(&launch.env))
        .flatten()
        .map(|dir| {
            let before: HashSet<PathBuf> = codex::session_files(&dir)
                .into_iter()
                .map(|(path, _)| path)
                .collect();
            (dir, before)
        });
    if let Err(err) = context.terminals.spawn(pane.id, launch, rules) {
        let id = pane.id;
        context.db.call(move |c| db::panes::delete(c, id)).await?;
        return Err(match err {
            SpawnError::Command(_) => client_error(
                ClientErrorStatusCode::BAD_REQUEST,
                "invalid_argv",
                err.to_string(),
            ),
            SpawnError::Pty(_) => HttpError::for_internal_error(err.to_string()),
        });
    }
    context.events.changed(Entity::Pane, pane.id);
    let delivered = prompted.iter().map(|note| note.id).collect();
    delivery::mark_read(&context.db, &context.events, delivered, pane.id).await;
    if let Some((dir, before)) = codex_sessions {
        codex::watch(
            context.db.clone(),
            context.terminals.clone(),
            context.events.clone(),
            pane.id,
            dir,
            before,
        );
    }
    Ok(HttpResponseCreated(with_live_status(context, pane)))
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

/// Types text into the pane: now, or once it is idle, done or blocked (`when: "idle"`).
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
        .input(input.text.as_bytes(), when)
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
