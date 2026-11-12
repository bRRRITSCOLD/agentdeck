//! Tabs and their layout trees: `/v1/workspaces/{id}/tabs` and `/v1/tabs/{id}`.

use dropshot::{
    ClientErrorStatusCode, HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk,
    Path, RequestContext, TypedBody, endpoint,
};

use super::{ApiContext, Authorized, IdPath, client_error};
use crate::db;
use crate::model::{Layout, NewTab, Tab, TabPatch};

fn check_layout(layout: Option<&Layout>) -> Result<(), HttpError> {
    match layout.map(Layout::validate) {
        Some(Err(message)) => Err(client_error(
            ClientErrorStatusCode::BAD_REQUEST,
            "invalid_layout",
            message,
        )),
        _ => Ok(()),
    }
}

/// A workspace's tabs, in position order.
#[endpoint { method = GET, path = "/v1/workspaces/{id}/tabs" }]
pub(crate) async fn tab_list(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath>,
) -> Result<HttpResponseOk<Vec<Tab>>, HttpError> {
    let id = path.into_inner().id;
    let tabs = rqctx
        .context()
        .db
        .call(move |c| db::tabs::list(c, id))
        .await?;
    Ok(HttpResponseOk(tabs))
}

/// Adds a tab after the workspace's last one.
#[endpoint { method = POST, path = "/v1/workspaces/{id}/tabs" }]
pub(crate) async fn tab_create(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath>,
    body: TypedBody<NewTab>,
) -> Result<HttpResponseCreated<Tab>, HttpError> {
    let id = path.into_inner().id;
    let new = body.into_inner();
    check_layout(new.layout.as_ref())?;
    let tab = rqctx
        .context()
        .db
        .call(move |c| db::tabs::create(c, id, &new.title, new.layout.as_ref()))
        .await?;
    Ok(HttpResponseCreated(tab))
}

/// Renames, moves or re-lays-out a tab.
#[endpoint { method = PATCH, path = "/v1/tabs/{id}" }]
pub(crate) async fn tab_update(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath>,
    body: TypedBody<TabPatch>,
) -> Result<HttpResponseOk<Tab>, HttpError> {
    let id = path.into_inner().id;
    let patch = body.into_inner();
    check_layout(patch.layout.as_ref())?;
    let tab = rqctx
        .context()
        .db
        .call(move |c| db::tabs::update(c, id, &patch))
        .await?;
    Ok(HttpResponseOk(tab))
}

/// Deletes a tab and its panes.
#[endpoint { method = DELETE, path = "/v1/tabs/{id}" }]
pub(crate) async fn tab_delete(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let id = path.into_inner().id;
    rqctx
        .context()
        .db
        .call(move |c| db::tabs::delete(c, id))
        .await?;
    Ok(HttpResponseDeleted())
}
