//! The HTTP API under `/v1` (spec §4), on dropshot.
//!
//! Every endpoint takes [`Authorized`] as its first extractor, so a request without the
//! bearer token gets 401 before anything else about it is looked at. An expected domain
//! failure is its own status with an `error_code`; see [`client_error`].

mod github;
mod health;
mod panes;
mod prs;
mod repositories;
mod settings;
mod tabs;
mod tasks;
mod workspaces;
mod ws;

use std::any::Any;
use std::path::PathBuf;
use std::time::Instant;

use async_trait::async_trait;
use dropshot::{
    ApiDescription, ApiEndpointBodyContentType, ClientErrorStatusCode, ErrorStatusCode,
    ExtensionMode, ExtractorMetadata, HttpError, RequestContext, ServerContext, SharedExtractor,
};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::auth::bearer_matches;
use crate::config::Config;
use crate::db::{Db, DbError};
use crate::events::Events;
use crate::github_auth::GithubAuth;
use crate::prsync::PrSync;
use crate::prsync::github::Github;
use crate::terminal::Terminals;

/// The version `openapi.json` declares: the API is `/v1`.
const API_VERSION: semver::Version = semver::Version::new(1, 0, 0);

/// What every handler can reach.
pub struct ApiContext {
    /// The database actor.
    pub db: Db,
    /// The bearer token from `daemon.json`.
    pub token: String,
    /// When the daemon started, for `/v1/health`.
    pub started: Instant,
    /// Where scratch workspace dirs are created.
    pub scratch_root: PathBuf,
    /// PR sync, for `/v1/prs`.
    pub prs: PrSync,
    /// The GitHub credentials, for `/v1/github`, and for reading Repositories from GitHub.
    pub github: GithubAuth,
    /// GitHub's APIs, for reading a Repository by name.
    pub github_api: Github,
    /// The settings in `config.toml`, for `/v1/settings`.
    pub settings: Config,
    /// The live panes.
    pub terminals: Terminals,
    /// The events every WebSocket client gets.
    pub events: Events,
}

/// Every endpoint.
pub fn api() -> ApiDescription<ApiContext> {
    let mut api = ApiDescription::new();
    let registered = [
        api.register(health::health),
        api.register(workspaces::workspace_list),
        api.register(workspaces::workspace_create),
        api.register(workspaces::workspace_get),
        api.register(workspaces::workspace_update),
        api.register(workspaces::workspace_archive),
        api.register(workspaces::workspace_unarchive),
        api.register(workspaces::workspace_dirty),
        api.register(workspaces::workspace_delete),
        api.register(tabs::tab_list),
        api.register(tabs::tab_create),
        api.register(tabs::tab_update),
        api.register(tabs::tab_delete),
        api.register(tasks::task_list),
        api.register(tasks::task_create),
        api.register(tasks::task_get),
        api.register(tasks::task_update),
        api.register(tasks::task_delete),
        api.register(prs::pr_list),
        api.register(prs::prs_sync),
        api.register(prs::prs_status),
        api.register(prs::prs_close),
        api.register(repositories::repository_list),
        api.register(repositories::repository_resolve),
        api.register(repositories::link_create),
        api.register(repositories::link_delete),
        api.register(github::github_auth_get),
        api.register(github::github_token_put),
        api.register(github::github_token_delete),
        api.register(github::github_client_id_put),
        api.register(github::github_client_id_delete),
        api.register(github::github_device_start),
        api.register(github::github_device_cancel),
        api.register(settings::settings_get),
        api.register(settings::settings_update),
        api.register(panes::pane_create),
        api.register(panes::pane_get),
        api.register(panes::pane_update),
        api.register(panes::pane_delete),
        api.register(panes::pane_respawn),
        api.register(panes::pane_stop),
        api.register(panes::pane_input),
        api.register(panes::pane_tail),
        api.register(ws::ws),
    ];
    for result in registered {
        result.expect("every endpoint registers");
    }
    api
}

/// The OpenAPI document `cargo xtask openapi` writes to `openapi.json`.
pub fn openapi_json() -> String {
    let api = api();
    let mut openapi = api.openapi("agentdeck", API_VERSION);
    openapi.description(
        "The agentdeck daemon's HTTP API. Every request carries \
         `Authorization: Bearer <token>`, with the token from daemon.json.",
    );
    let mut json = openapi.json().expect("the OpenAPI document serializes");
    // Key order follows serde_json's `preserve_order` feature, which another crate in the
    // workspace may turn on; sorting keeps the file the same however the daemon was built.
    json.sort_all_objects();
    let mut text = serde_json::to_string_pretty(&json).expect("JSON serializes");
    text.push('\n');
    text
}

/// Proof the request carried the bearer token. Declared first in every endpoint's
/// parameters, so it runs before any other extractor.
pub struct Authorized;

#[async_trait]
impl SharedExtractor for Authorized {
    async fn from_request<Context: ServerContext>(
        rqctx: &RequestContext<Context>,
    ) -> Result<Self, HttpError> {
        let context: &dyn Any = rqctx.context();
        let context = context.downcast_ref::<ApiContext>().ok_or_else(|| {
            HttpError::for_internal_error("Authorized used outside ApiContext".into())
        })?;
        let header = rqctx
            .request
            .headers()
            .get("authorization")
            .map(|value| value.as_bytes());
        if bearer_matches(header, &context.token) {
            Ok(Authorized)
        } else {
            Err(client_error(
                ClientErrorStatusCode::UNAUTHORIZED,
                "unauthorized",
                "missing or wrong bearer token".into(),
            ))
        }
    }

    fn metadata(_body_content_type: ApiEndpointBodyContentType) -> ExtractorMetadata {
        ExtractorMetadata {
            extension_mode: ExtensionMode::None,
            parameters: Vec::new(),
        }
    }
}

/// A `{id}` path, typed by the entity's id.
#[derive(Deserialize, JsonSchema)]
pub(crate) struct IdPath<T> {
    /// The entity's id.
    pub id: T,
}

/// An expected domain failure: `status` with a machine-readable `code`.
pub(crate) fn client_error(
    status: ClientErrorStatusCode,
    code: &str,
    message: String,
) -> HttpError {
    HttpError::for_client_error(Some(code.to_owned()), status, message)
}

/// A 5xx with a `code` and a message the client shows: a third party or a system service
/// failed.
pub(crate) fn server_error(status: ErrorStatusCode, code: &str, message: String) -> HttpError {
    HttpError {
        status_code: status,
        error_code: Some(code.to_owned()),
        external_message: message.clone(),
        internal_message: message,
        headers: None,
    }
}

impl From<DbError> for HttpError {
    fn from(err: DbError) -> Self {
        let message = err.to_string();
        match err {
            DbError::NotFound(_) => {
                client_error(ClientErrorStatusCode::NOT_FOUND, "not_found", message)
            }
            DbError::UnknownReference(entity) => client_error(
                ClientErrorStatusCode::BAD_REQUEST,
                &format!("unknown_{entity}"),
                message,
            ),
            DbError::Invalid { code, .. } => {
                client_error(ClientErrorStatusCode::BAD_REQUEST, code, message)
            }
            DbError::Conflict { code, .. } => {
                client_error(ClientErrorStatusCode::CONFLICT, code, message)
            }
            DbError::Sql(_) | DbError::Io(_) | DbError::Closed => {
                HttpError::for_internal_error(message)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod openapi_json {
        use super::*;

        const COMMITTED: &str = include_str!("../../../../openapi.json");

        #[test]
        fn should_match_the_committed_openapi_json() {
            assert!(
                openapi_json() == COMMITTED,
                "openapi.json is stale: run `cargo xtask openapi` and commit the result"
            );
        }
        /// `openapi.json`'s fingerprint when [`agentdeck_core::API_VERSION`] was last set,
        /// and that version. Clients tell an older daemon by its API version, so a route or
        /// field added without a bump would go unnoticed: they would call it and get a 404.
        const FINGERPRINT_AT_API_VERSION: (u64, u32) = (0x4b7e_8b2f_7d03_4654, 21);

        /// FNV-1a: stable across Rust releases, unlike `DefaultHasher`.
        fn fingerprint(text: &str) -> u64 {
            text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            })
        }

        #[test]
        fn should_bump_api_version_when_openapi_json_changes() {
            let now = (fingerprint(COMMITTED), agentdeck_core::API_VERSION);

            assert!(
                now == FINGERPRINT_AT_API_VERSION,
                "openapi.json or API_VERSION changed: if a route or a field was added or \
                 changed, bump agentdeck_core::API_VERSION (spec §2). Then set \
                 FINGERPRINT_AT_API_VERSION to ({:#x}, API_VERSION).",
                now.0
            );
        }
    }
}
