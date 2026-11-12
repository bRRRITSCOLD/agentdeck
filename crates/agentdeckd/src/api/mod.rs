//! The HTTP API under `/v1` (spec §4), on dropshot.
//!
//! Every endpoint takes [`Authorized`] as its first extractor, so a request without the
//! bearer token gets 401 before anything else about it is looked at. An expected domain
//! failure is its own status with an `error_code`; see [`client_error`].

mod health;
mod notes;
mod tabs;
mod tasks;
mod workspaces;

use std::any::Any;
use std::path::PathBuf;
use std::time::Instant;

use async_trait::async_trait;
use dropshot::{
    ApiDescription, ApiEndpointBodyContentType, ClientErrorStatusCode, ExtensionMode,
    ExtractorMetadata, HttpError, RequestContext, ServerContext, SharedExtractor,
};
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::bearer_matches;
use crate::db::{Db, DbError};

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
        api.register(notes::note_list),
        api.register(notes::note_create),
        api.register(notes::note_update),
        api.register(notes::note_delete),
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
    let json = openapi.json().expect("the OpenAPI document serializes");
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

/// A `{id}` path.
#[derive(Deserialize, JsonSchema)]
pub(crate) struct IdPath {
    /// The entity's id.
    pub id: Uuid,
}

/// An expected domain failure: `status` with a machine-readable `code`.
pub(crate) fn client_error(
    status: ClientErrorStatusCode,
    code: &str,
    message: String,
) -> HttpError {
    HttpError::for_client_error(Some(code.to_owned()), status, message)
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
    }
}
