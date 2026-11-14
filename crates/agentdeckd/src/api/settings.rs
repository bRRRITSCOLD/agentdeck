//! The settings in `config.toml`: `/v1/settings` (spec §2, §4).

use agentdeck_core::ws::Entity;
use dropshot::{
    ClientErrorStatusCode, HttpError, HttpResponseOk, RequestContext, TypedBody, endpoint,
};

use super::{ApiContext, Authorized, client_error};
use crate::config::UpdateError;
use crate::model::{Settings, SettingsPatch};

/// The id a [`Entity::Settings`] event names: there is one set of settings.
const SETTINGS_ID: &str = "settings";

/// Every setting with the value the daemon uses.
///
/// `overridden_by_env` names the keys an environment variable sets.
#[endpoint { method = GET, path = "/v1/settings" }]
pub(crate) async fn settings_get(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
) -> Result<HttpResponseOk<Settings>, HttpError> {
    Ok(HttpResponseOk(rqctx.context().settings.settings()))
}

/// Changes some settings and answers with them all.
///
/// The changes are written to `config.toml`, keeping its other keys and comments, and apply
/// at once. A key or value the daemon doesn't take is 400
/// `invalid_setting`, naming the key; a `config.toml` that doesn't parse is left as it is,
/// 409 `unparsable_config`. A key an environment variable overrides is written, but the
/// variable's value stays in use.
#[endpoint { method = PATCH, path = "/v1/settings" }]
pub(crate) async fn settings_update(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    body: TypedBody<SettingsPatch>,
) -> Result<HttpResponseOk<Settings>, HttpError> {
    let context = rqctx.context();
    let patch = body.into_inner();
    let config = context.settings.clone();
    let before = config.max_age_days();
    let settings = tokio::task::spawn_blocking(move || config.update(&patch))
        .await
        .map_err(|err| HttpError::for_internal_error(format!("writing the settings: {err}")))?
        .map_err(|err| match err {
            UpdateError::Invalid(_) => client_error(
                ClientErrorStatusCode::BAD_REQUEST,
                "invalid_setting",
                err.to_string(),
            ),
            UpdateError::Unparsable { .. } => client_error(
                ClientErrorStatusCode::CONFLICT,
                "unparsable_config",
                err.to_string(),
            ),
            UpdateError::Io { .. } => HttpError::for_internal_error(err.to_string()),
        })?;
    context.events.changed(Entity::Settings, SETTINGS_ID);
    if settings.pr_sync.max_age_days != before {
        context.prs.status_changed();
    }
    Ok(HttpResponseOk(settings))
}
