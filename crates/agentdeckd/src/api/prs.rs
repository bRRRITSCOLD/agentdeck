//! PR sync: `/v1/prs` (spec §8).

use dropshot::{HttpError, HttpResponseOk, RequestContext, endpoint};

use super::{ApiContext, Authorized};
use crate::model::PrSyncStatus;

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
