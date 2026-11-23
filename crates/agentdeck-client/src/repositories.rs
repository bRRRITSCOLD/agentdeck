//! Repositories as the GUI reads them (spec §4 `GET /v1/repositories`, §11 Notifications).

use crate::error::OLDER_DAEMON;
use crate::types::RepositoryLinks;
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
