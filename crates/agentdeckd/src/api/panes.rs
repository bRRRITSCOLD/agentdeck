//! Panes: `/v1/panes` and `/v1/panes/{id}` (spec §4, §6).

use std::path::PathBuf;

use agentdeck_core::ws::Entity;
use agentdeck_core::{PaneId, PaneKind};
use dropshot::{
    Body, ClientErrorStatusCode, FreeformBody, HttpError, HttpResponseAccepted,
    HttpResponseCreated, HttpResponseDeleted, HttpResponseOk, Path, Query, RequestContext,
    TypedBody, endpoint,
};

use super::{ApiContext, Authorized, IdPath, client_error};
use crate::db;
use crate::model::{InputAccepted, InputWhen, NewPane, Pane, PaneInput, TailQuery};
use crate::terminal::env::pane_env;
use crate::terminal::fanout::Size;
use crate::terminal::pane::{Launch, SpawnError, When};
use crate::terminal::tail::TAIL_CAPACITY;

/// A new pane's size until a client resizes it.
const INITIAL_SIZE: Size = Size { cols: 80, rows: 24 };

/// The command a shell pane runs: `argv` as given, else the daemon's `$SHELL`, else
/// `/bin/sh`.
fn shell_argv(argv: Option<Vec<String>>) -> Result<Vec<String>, HttpError> {
    match argv {
        Some(argv) if argv.first().is_none_or(|program| program.is_empty()) => Err(client_error(
            ClientErrorStatusCode::BAD_REQUEST,
            "invalid_argv",
            "argv must name a program".into(),
        )),
        Some(argv) => Ok(argv),
        None => {
            let shell = std::env::var("SHELL")
                .ok()
                .filter(|shell| shell.starts_with('/'))
                .unwrap_or_else(|| "/bin/sh".to_owned());
            Ok(vec![shell])
        }
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

/// Spawns a pane in a tab and starts its process. The caller places the pane in the tab's
/// layout. Agent panes are not launched yet: only `kind: "shell"`.
#[endpoint { method = POST, path = "/v1/panes" }]
pub(crate) async fn pane_create(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    body: TypedBody<NewPane>,
) -> Result<HttpResponseCreated<Pane>, HttpError> {
    let context = rqctx.context();
    let new = body.into_inner();
    if new.kind != PaneKind::Shell {
        return Err(client_error(
            ClientErrorStatusCode::BAD_REQUEST,
            "unsupported_kind",
            "only shell panes can be launched so far".into(),
        ));
    }
    let argv = shell_argv(new.argv.clone())?;
    let cwd = check_cwd(&new.cwd)?;
    let stored = argv.clone();
    let (pane, workspace) = context
        .db
        .call(move |c| db::panes::create(c, &new, &stored))
        .await?;

    let extra = [
        ("AGENTDECK_PANE".to_owned(), pane.id.to_string()),
        ("AGENTDECK_WORKSPACE".to_owned(), workspace.to_string()),
    ];
    let launch = Launch {
        argv,
        cwd,
        env: pane_env(std::env::vars_os(), &extra),
        size: INITIAL_SIZE,
    };
    if let Err(err) = context.terminals.spawn(pane.id, launch, "shell") {
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

    mod shell_argv {
        use super::*;

        #[test]
        fn should_keep_the_argv_given() {
            let argv = vec!["sh".to_owned(), "-c".to_owned(), "env".to_owned()];
            assert_eq!(shell_argv(Some(argv.clone())).unwrap(), argv);
        }

        #[test]
        fn should_reject_an_argv_without_a_program() {
            assert!(shell_argv(Some(vec![])).is_err());
            assert!(shell_argv(Some(vec![String::new()])).is_err());
        }
    }
}
