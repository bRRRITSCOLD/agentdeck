//! Workspaces: `/v1/workspaces`.

use std::fmt;
use std::path::{Path as FsPath, PathBuf};

use dropshot::{
    ClientErrorStatusCode, ErrorStatusCode, HttpError, HttpResponseCreated, HttpResponseDeleted,
    HttpResponseError, HttpResponseOk, Path, Query, RequestContext, TypedBody, endpoint,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ApiContext, Authorized, IdPath, client_error};
use crate::model::{DirtyReport, NewWorkspace, Workspace, WorkspacePatch};
use crate::{db, git};

fn check_name(name: Option<&str>) -> Result<(), HttpError> {
    match name {
        Some(name) if name.trim().is_empty() => Err(client_error(
            ClientErrorStatusCode::BAD_REQUEST,
            "invalid_name",
            "a workspace's name can't be empty".into(),
        )),
        _ => Ok(()),
    }
}

/// An attached dir must be an absolute path to an existing directory.
fn check_dir(dir: &str) -> Result<PathBuf, HttpError> {
    let path = PathBuf::from(dir);
    if path.is_absolute() && path.is_dir() {
        Ok(path)
    } else {
        Err(client_error(
            ClientErrorStatusCode::BAD_REQUEST,
            "invalid_dir",
            format!("{dir} is not an absolute path to an existing directory"),
        ))
    }
}

/// Runs the git calls off the async threads.
async fn dirty_report(dir: String) -> Result<DirtyReport, HttpError> {
    tokio::task::spawn_blocking(move || git::dirty_report(FsPath::new(&dir)))
        .await
        .map_err(|err| HttpError::for_internal_error(err.to_string()))?
        .map_err(|err| HttpError::for_internal_error(format!("dirty check: {err}")))
}

/// Every workspace, archived ones included, oldest first.
#[endpoint { method = GET, path = "/v1/workspaces" }]
pub(crate) async fn workspace_list(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
) -> Result<HttpResponseOk<Vec<Workspace>>, HttpError> {
    let workspaces = rqctx.context().db.call(|c| db::workspaces::list(c)).await?;
    Ok(HttpResponseOk(workspaces))
}

/// Creates a workspace on an existing dir, or on a new scratch dir when `dir` is absent.
#[endpoint { method = POST, path = "/v1/workspaces" }]
pub(crate) async fn workspace_create(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    body: TypedBody<NewWorkspace>,
) -> Result<HttpResponseCreated<Workspace>, HttpError> {
    let new = body.into_inner();
    check_name(Some(&new.name))?;
    let dir = new.dir.as_deref().map(check_dir).transpose()?;
    let scratch_root = rqctx.context().scratch_root.clone();
    let workspace = rqctx
        .context()
        .db
        .call(move |c| db::workspaces::create(c, &new.name, dir.as_deref(), &scratch_root))
        .await?;
    Ok(HttpResponseCreated(workspace))
}

/// One workspace.
#[endpoint { method = GET, path = "/v1/workspaces/{id}" }]
pub(crate) async fn workspace_get(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath>,
) -> Result<HttpResponseOk<Workspace>, HttpError> {
    let id = path.into_inner().id;
    let workspace = rqctx
        .context()
        .db
        .call(move |c| db::workspaces::get(c, id))
        .await?;
    Ok(HttpResponseOk(workspace))
}

/// Renames, mutes or unmutes a workspace.
#[endpoint { method = PATCH, path = "/v1/workspaces/{id}" }]
pub(crate) async fn workspace_update(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath>,
    body: TypedBody<WorkspacePatch>,
) -> Result<HttpResponseOk<Workspace>, HttpError> {
    let id = path.into_inner().id;
    let patch = body.into_inner();
    check_name(patch.name.as_deref())?;
    let workspace = rqctx
        .context()
        .db
        .call(move |c| db::workspaces::update(c, id, &patch))
        .await?;
    Ok(HttpResponseOk(workspace))
}

/// Archives a workspace; its tasks, notes and dir are kept.
#[endpoint { method = POST, path = "/v1/workspaces/{id}/archive" }]
pub(crate) async fn workspace_archive(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath>,
) -> Result<HttpResponseOk<Workspace>, HttpError> {
    let id = path.into_inner().id;
    let workspace = rqctx
        .context()
        .db
        .call(move |c| db::workspaces::set_archived(c, id, true))
        .await?;
    Ok(HttpResponseOk(workspace))
}

/// Brings an archived workspace back.
#[endpoint { method = POST, path = "/v1/workspaces/{id}/unarchive" }]
pub(crate) async fn workspace_unarchive(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath>,
) -> Result<HttpResponseOk<Workspace>, HttpError> {
    let id = path.into_inner().id;
    let workspace = rqctx
        .context()
        .db
        .call(move |c| db::workspaces::set_archived(c, id, false))
        .await?;
    Ok(HttpResponseOk(workspace))
}

/// Each git repo under the workspace's dir with uncommitted files or unpushed branches.
#[endpoint { method = GET, path = "/v1/workspaces/{id}/dirty" }]
pub(crate) async fn workspace_dirty(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath>,
) -> Result<HttpResponseOk<DirtyReport>, HttpError> {
    let id = path.into_inner().id;
    let workspace = rqctx
        .context()
        .db
        .call(move |c| db::workspaces::get(c, id))
        .await?;
    Ok(HttpResponseOk(dirty_report(workspace.dir).await?))
}

/// `DELETE /v1/workspaces/{id}` options.
#[derive(Deserialize, JsonSchema)]
pub(crate) struct DeleteQuery {
    /// Delete even when the dir holds uncommitted or unpushed work.
    force: Option<bool>,
}

/// `DELETE /v1/workspaces/{id}`'s error: dropshot's error body, plus the dirty report when
/// `error_code` is `dirty` (409).
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct DeleteWorkspaceError {
    /// What went wrong.
    message: String,
    /// A machine-readable code: `dirty`, `not_found`, `unauthorized`, …
    error_code: Option<String>,
    /// What deleting would lose; set only with `error_code` `dirty`.
    dirty: Option<DirtyReport>,
    #[serde(skip)]
    status: ErrorStatusCode,
    #[serde(skip)]
    internal_message: String,
}

impl fmt::Display for DeleteWorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.internal_message)
    }
}

impl From<HttpError> for DeleteWorkspaceError {
    fn from(err: HttpError) -> Self {
        DeleteWorkspaceError {
            message: err.external_message,
            error_code: err.error_code,
            dirty: None,
            status: err.status_code,
            internal_message: err.internal_message,
        }
    }
}

impl From<db::DbError> for DeleteWorkspaceError {
    fn from(err: db::DbError) -> Self {
        HttpError::from(err).into()
    }
}

impl HttpResponseError for DeleteWorkspaceError {
    fn status_code(&self) -> ErrorStatusCode {
        self.status
    }
}

/// Deletes a workspace, and its dir when agentdeck created it. Its tasks move to the inbox.
/// Without `force`, a dir holding uncommitted or unpushed work is a 409 `dirty` carrying the
/// dirty report.
#[endpoint { method = DELETE, path = "/v1/workspaces/{id}" }]
pub(crate) async fn workspace_delete(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath>,
    query: Query<DeleteQuery>,
) -> Result<HttpResponseDeleted, DeleteWorkspaceError> {
    let id = path.into_inner().id;
    let force = query.into_inner().force.unwrap_or(false);
    let db = &rqctx.context().db;
    let workspace = db.call(move |c| db::workspaces::get(c, id)).await?;

    if !force {
        let report = dirty_report(workspace.dir.clone()).await?;
        if !report.repos.is_empty() {
            let message = format!(
                "{} holds uncommitted or unpushed work; delete with force to lose it",
                workspace.dir
            );
            return Err(DeleteWorkspaceError {
                message: message.clone(),
                error_code: Some("dirty".into()),
                dirty: Some(report),
                status: ClientErrorStatusCode::CONFLICT.into(),
                internal_message: message,
            });
        }
    }

    if workspace.dir_owned {
        let dir = PathBuf::from(&workspace.dir);
        tokio::task::spawn_blocking(move || match std::fs::remove_dir_all(&dir) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        })
        .await
        .map_err(|err| HttpError::for_internal_error(err.to_string()))?
        .map_err(|err| HttpError::for_internal_error(format!("removing the dir: {err}")))?;
    }
    db.call(move |c| db::workspaces::delete(c, id)).await?;
    Ok(HttpResponseDeleted())
}
