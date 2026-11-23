//! Workspaces: `/v1/workspaces`.

use dropshot::{
    ClientErrorStatusCode, HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk,
    Path, RequestContext, TypedBody, endpoint,
};

use agentdeck_core::WorkspaceId;
use agentdeck_core::ws::Entity;

use super::repositories::linkings;
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

/// Creates a workspace. It has no folder, so nothing is made on disk. It links each
/// Repository in `link` as `POST /v1/workspaces/{id}/links` does: one agentdeck keeps by
/// that name is linked without asking GitHub, any other is looked up on GitHub first. One
/// GitHub doesn't show the sign-in is 400 `unknown_repository` and makes no workspace;
/// `409 not_signed_in` and `502 github_error` as the link route. Sends `changed` for each
/// Repository linked, then the workspace.
#[endpoint { method = POST, path = "/v1/workspaces" }]
pub(crate) async fn workspace_create(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    body: TypedBody<NewWorkspace>,
) -> Result<HttpResponseCreated<Workspace>, HttpError> {
    let context = rqctx.context();
    let new = body.into_inner();
    check_name(Some(&new.name))?;
    let links = linkings(context, new.link).await?;
    let (workspace, relinked) = context
        .db
        .call(move |c| db::workspaces::create_linking(c, &new.name, &links))
        .await?;
    for repository in relinked {
        context.events.changed(Entity::Repository, repository);
    }
    context.events.changed(Entity::Workspace, workspace.id);
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

/// Renames, mutes or unmutes a workspace, and links and unlinks its Repositories, all in one
/// request applied together: a request that fails changes nothing. `link` takes Repositories
/// as `POST /v1/workspaces/{id}/links` does (one GitHub doesn't show the sign-in is 400
/// `unknown_repository`; `409 not_signed_in` and `502 github_error` as that route), `unlink`
/// takes their ids as `DELETE /v1/workspaces/{id}/links/{repository}` does, and a Repository
/// nothing names any more goes. Unlinking one it doesn't link changes nothing; naming one
/// Repository in both is 400 `invalid_links`. An archived workspace can be changed too.
/// Sends `changed` for each Repository whose Link changed, then the workspace.
#[endpoint { method = PATCH, path = "/v1/workspaces/{id}" }]
pub(crate) async fn workspace_update(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<WorkspaceId>>,
    body: TypedBody<WorkspacePatch>,
) -> Result<HttpResponseOk<Workspace>, HttpError> {
    let context = rqctx.context();
    let id = path.into_inner().id;
    let mut patch = body.into_inner();
    check_name(patch.name.as_deref())?;
    // A workspace that doesn't exist is 404 before GitHub is asked about anything.
    context.db.call(move |c| db::workspaces::get(c, id)).await?;
    let links = linkings(context, std::mem::take(&mut patch.link)).await?;
    let (workspace, relinked) = context
        .db
        .call(move |c| db::workspaces::update_linking(c, id, &patch, &links))
        .await?;
    for repository in relinked {
        context.events.changed(Entity::Repository, repository);
    }
    context.events.changed(Entity::Workspace, workspace.id);
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
