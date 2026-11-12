//! Tabs and their layout trees: `/v1/workspaces/{id}/tabs` and `/v1/tabs/{id}`.

use dropshot::{
    ClientErrorStatusCode, HttpError, HttpResponseCreated, HttpResponseDeleted, HttpResponseOk,
    Path, RequestContext, TypedBody, endpoint,
};

use std::collections::HashSet;

use agentdeck_core::{Layout, PaneId, TabId, WorkspaceId};

use super::{ApiContext, Authorized, IdPath, client_error};
use crate::db;
use crate::model::{NewTab, Tab, TabPatch};

/// A layout from a client must name each pane once; serde has already bounded its ratios.
fn check_layout(layout: Option<&Layout>) -> Result<(), HttpError> {
    match layout.and_then(repeated_pane) {
        Some(pane) => Err(client_error(
            ClientErrorStatusCode::BAD_REQUEST,
            "invalid_layout",
            format!("pane {pane} appears more than once in the layout"),
        )),
        None => Ok(()),
    }
}

/// The first pane the tree names twice.
fn repeated_pane(layout: &Layout) -> Option<PaneId> {
    let mut seen = HashSet::new();
    layout.panes().into_iter().find(|pane| !seen.insert(*pane))
}

/// A workspace's tabs, in position order.
#[endpoint { method = GET, path = "/v1/workspaces/{id}/tabs" }]
pub(crate) async fn tab_list(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    path: Path<IdPath<WorkspaceId>>,
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
    path: Path<IdPath<WorkspaceId>>,
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
    path: Path<IdPath<TabId>>,
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
    path: Path<IdPath<TabId>>,
) -> Result<HttpResponseDeleted, HttpError> {
    let id = path.into_inner().id;
    rqctx
        .context()
        .db
        .call(move |c| db::tabs::delete(c, id))
        .await?;
    Ok(HttpResponseDeleted())
}

#[cfg(test)]
mod tests {
    use agentdeck_core::{Ratio, SplitDirection};

    use super::*;

    fn pane(n: u128) -> Layout {
        Layout::single(uuid::Uuid::from_u128(n).into())
    }

    fn split(a: Layout, b: Layout) -> Layout {
        Layout::Split {
            split: SplitDirection::Horizontal,
            ratio: Ratio::HALF,
            a: Box::new(a),
            b: Box::new(b),
        }
    }

    mod repeated_pane {
        use super::*;

        #[test]
        fn should_find_nothing_when_every_pane_is_distinct() {
            assert_eq!(
                repeated_pane(&split(pane(1), split(pane(2), pane(3)))),
                None
            );
        }

        #[test]
        fn should_find_a_pane_named_twice_deep_in_the_tree() {
            assert_eq!(
                repeated_pane(&split(pane(1), split(pane(2), pane(1)))),
                Some(uuid::Uuid::from_u128(1).into())
            );
        }
    }
}
