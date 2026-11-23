//! Workspaces: `/v1/workspaces`.

use dropshot::{
    ClientErrorStatusCode, HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk,
    Path, RequestContext, TypedBody, endpoint,
};

use agentdeck_core::WorkspaceId;
use agentdeck_core::ws::Entity;

use super::{ApiContext, Authorized, IdPath, client_error};
use crate::db;
use crate::model::{NewWorkspace, Workspace, WorkspacePatch};

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

/// Every workspace, archived ones included, oldest first.
#[endpoint { method = GET, path = "/v1/workspaces" }]
pub(crate) async fn workspace_list(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
) -> Result<HttpResponseOk<Vec<Workspace>>, HttpError> {
    let workspaces = rqctx.context().db.call(|c| db::workspaces::list(c)).await?;
    Ok(HttpResponseOk(workspaces))
}

/// Creates a workspace. It has no folder, so nothing is made on disk.
#[endpoint { method = POST, path = "/v1/workspaces" }]
pub(crate) async fn workspace_create(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    body: TypedBody<NewWorkspace>,
) -> Result<HttpResponseCreated<Workspace>, HttpError> {
    let new = body.into_inner();
    check_name(Some(&new.name))?;
    let workspace = rqctx
        .context()
        .db
        .call(move |c| db::workspaces::create(c, &new.name))
        .await?;
    rqctx
        .context()
        .events
        .changed(Entity::Workspace, workspace.id);
    Ok(HttpResponseCreated(workspace))
}

/// One workspace.
#[endpoint { method = GET, path = "/v1/workspaces/{id}" }]
pub(crate) async fn workspace_get(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<WorkspaceId>>,
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
    path: Path<IdPath<WorkspaceId>>,
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
    rqctx
        .context()
        .events
        .changed(Entity::Workspace, workspace.id);
    Ok(HttpResponseOk(workspace))
}

/// Archives a workspace; its panes and Links are kept. Refused (409 `panes_running`) while
/// any of its panes runs.
#[endpoint { method = POST, path = "/v1/workspaces/{id}/archive" }]
pub(crate) async fn workspace_archive(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<WorkspaceId>>,
) -> Result<HttpResponseOk<Workspace>, HttpError> {
    let id = path.into_inner().id;
    let workspace = rqctx
        .context()
        .db
        .call(move |c| db::workspaces::set_archived(c, id, true))
        .await?;
    rqctx
        .context()
        .events
        .changed(Entity::Workspace, workspace.id);
    Ok(HttpResponseOk(workspace))
}

/// Brings an archived workspace back.
#[endpoint { method = POST, path = "/v1/workspaces/{id}/unarchive" }]
pub(crate) async fn workspace_unarchive(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<WorkspaceId>>,
) -> Result<HttpResponseOk<Workspace>, HttpError> {
    let id = path.into_inner().id;
    let workspace = rqctx
        .context()
        .db
        .call(move |c| db::workspaces::set_archived(c, id, false))
        .await?;
    rqctx
        .context()
        .events
        .changed(Entity::Workspace, workspace.id);
    Ok(HttpResponseOk(workspace))
}

/// Deletes a workspace with its tabs and panes, stopping their processes. Its Links go, and
/// with them each Repository nothing else names, with a `repository` change for each
/// Repository it linked. It deletes no folder: a workspace has none.
#[endpoint { method = DELETE, path = "/v1/workspaces/{id}" }]
pub(crate) async fn workspace_delete(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<WorkspaceId>>,
) -> Result<HttpResponseDeleted, HttpError> {
    let id = path.into_inner().id;
    let unlinked = (rqctx.context().db)
        .call(move |c| db::workspaces::delete(c, id))
        .await?;
    // Its panes went with it: stop their processes.
    rqctx.context().terminals.reap().await?;
    let events = &rqctx.context().events;
    for repository in unlinked {
        events.changed(Entity::Repository, repository);
    }
    events.changed(Entity::Workspace, id);
    Ok(HttpResponseDeleted())
}
