//! Repositories as the GUI reads them (spec §4 `GET /v1/repositories`, §11 Notifications).

use agentdeck_core::RepositoryName;

use crate::error::OLDER_DAEMON;
use crate::types::{Repository, RepositoryLinks};
use crate::{ApiError, Client};

/// Every Repository the daemon keeps, by name, each with the Workspaces linking it, archived
/// ones included. A daemon older than the route has none to give, and lists none rather than
/// failing: the GUI reads the rest of its window as before while its banner offers the restart.
pub async fn list(client: &Client) -> Result<Vec<RepositoryLinks>, ApiError> {
    match client.repository_list().send().await {
        Ok(repositories) => Ok(repositories.into_inner()),
        Err(err) => match ApiError::from(err) {
            ApiError {
                message,
                code: None,
            } if message == OLDER_DAEMON => Ok(Vec::new()),
            err => Err(err),
        },
    }
}

/// The Repository GitHub shows by `name` now (`owner/name` or its URL), as the picker's typed
/// row looks one up. One GitHub can't see is the daemon's `unknown_repository`, whose message
/// is what the GUI shows: "GitHub can't see owner/name. Check the name or your token's
/// access."
pub async fn resolve(client: &Client, name: &RepositoryName) -> Result<Repository, ApiError> {
    let found = client
        .repository_resolve()
        .repository(name.clone())
        .send()
        .await?;
    Ok(found.into_inner())
}
