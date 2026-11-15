//! PR sync: `/v1/prs` (spec §8).

use dropshot::{HttpError, HttpResponseOk, RequestContext, TypedBody, endpoint};

use super::{ApiContext, Authorized};
use crate::model::{PrClose, PrCloseResults, PrSyncStatus};

/// Syncs PR tasks with GitHub now and answers with the status the sync left. Clients call it
/// when a task is opened. A sync that fails still answers 200: `last_error` says why, and
/// `login` is `logged_out` when there's no usable `gh` token.
#[endpoint { method = POST, path = "/v1/prs/sync" }]
pub(crate) async fn prs_sync(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
) -> Result<HttpResponseOk<PrSyncStatus>, HttpError> {
    Ok(HttpResponseOk(rqctx.context().prs.sync().await))
}

/// The `gh` login state and the last sync.
#[endpoint { method = GET, path = "/v1/prs/status" }]
pub(crate) async fn prs_status(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
) -> Result<HttpResponseOk<PrSyncStatus>, HttpError> {
    Ok(HttpResponseOk(rqctx.context().prs.status()))
}

/// Closes PRs on GitHub: only closes them, with no comment and no branch delete. Answers one
/// result per task, in the order asked, once every PR is tried; 200 even when some failed. A
/// closed PR's task moves to done at once.
#[endpoint { method = POST, path = "/v1/prs/close" }]
pub(crate) async fn prs_close(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    body: TypedBody<PrClose>,
) -> Result<HttpResponseOk<PrCloseResults>, HttpError> {
    let results = rqctx.context().prs.close(body.into_inner().task_ids).await;
    Ok(HttpResponseOk(PrCloseResults { results }))
}
