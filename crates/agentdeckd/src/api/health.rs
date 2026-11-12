//! `GET /v1/health`.

use dropshot::{HttpError, HttpResponseOk, RequestContext, endpoint};

use super::{ApiContext, Authorized};
use crate::model::Health;

/// The daemon's version and uptime.
#[endpoint { method = GET, path = "/v1/health" }]
pub(crate) async fn health(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
) -> Result<HttpResponseOk<Health>, HttpError> {
    Ok(HttpResponseOk(Health {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        uptime_secs: rqctx.context().started.elapsed().as_secs(),
    }))
}
