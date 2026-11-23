//! Pull requests as the GUI reads them (spec §4 `GET /v1/prs`, §11 Pull requests view).

use crate::error::OLDER_DAEMON;
use crate::types::PullRequest;
use crate::{ApiError, Client};

/// Every pull request the daemon keeps, Finished ones included, by Repository name, then
/// number. A daemon older than the route (before `API_VERSION` 16) has none to give, and lists
/// none rather than failing: the GUI reads the rest of its window as before while its banner
/// offers the restart. One older than Finished (`API_VERSION` 20) ignores `finished` and
/// marks none Finished.
pub async fn list(client: &Client) -> Result<Vec<PullRequest>, ApiError> {
    match client.pr_list().finished(true).send().await {
        Ok(prs) => Ok(prs.into_inner()),
        Err(err) => match ApiError::from(err) {
            ApiError {
                message,
                code: None,
            } if message == OLDER_DAEMON => Ok(Vec::new()),
            err => Err(err),
        },
    }
}
