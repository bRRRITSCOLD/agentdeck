//! Repositories as the GUI reads them (spec §4 `GET /v1/repositories`, §11 Notifications),
//! and the Links a Repository group's `…` menu makes and takes away (spec §4
//! `/v1/workspaces/{id}/links`, §11 Pull requests view).

use agentdeck_core::{RepositoryId, RepositoryName, WorkspaceId};

use crate::error::OLDER_DAEMON;
use crate::types::{NewLink, Repository, RepositoryLinks};
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

impl NewLink {
    /// A link by `owner/name`: one agentdeck doesn't keep by that name is looked up on GitHub.
    pub fn named(repository: RepositoryName) -> NewLink {
        NewLink {
            repository,
            id: None,
            url: None,
            private: None,
            archived: None,
        }
    }

    /// A link by GitHub's id, carrying what the client holds of `repository`: the daemon links
    /// the Repository it keeps with that id, or stores it again from these, never asking GitHub.
    pub fn held(repository: &Repository) -> NewLink {
        NewLink {
            repository: repository.name.clone(),
            id: Some(repository.id),
            url: Some(repository.url.clone()),
            private: Some(repository.private),
            archived: Some(repository.archived),
        }
    }
}

/// Links `workspace` to `repository` by GitHub's id, as the client holds it (spec §4): the
/// Repository the daemon keeps with that id, or, when it no longer keeps one (an unlink took
/// it), one stored again from `repository`, without asking GitHub, so it works signed out.
/// Answers the Repository with every Workspace linking it now.
pub async fn link(
    client: &Client,
    workspace: WorkspaceId,
    repository: &Repository,
) -> Result<RepositoryLinks, ApiError> {
    let linked = client
        .link_create()
        .id(workspace)
        .body(NewLink::held(repository))
        .send()
        .await?;
    Ok(linked.into_inner())
}

/// Unlinks `workspace` from Repository `repository`. The daemon deletes the Repository when
/// nothing names it any more (spec §3); [`link`] brings it back.
pub async fn unlink(
    client: &Client,
    workspace: WorkspaceId,
    repository: RepositoryId,
) -> Result<(), ApiError> {
    client
        .link_delete()
        .id(workspace)
        .repository(repository)
        .send()
        .await?;
    Ok(())
}
