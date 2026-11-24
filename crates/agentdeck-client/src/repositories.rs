//! Repositories as the GUI reads them (spec §4 `GET /v1/repositories`, §11 Notifications),
//! and the Links a Repository group's `…` menu makes and takes away (spec §4
//! `/v1/workspaces/{id}/links`, §11 Pull requests view), and each Workspace's Suggestions
//! (`/v1/workspaces/{id}/suggestions`), and the GitHub repository list the Link picker's All
//! your repositories offers (`/v1/github/repositories`).

use agentdeck_core::{RepositoryId, RepositoryName, WorkspaceId};

use crate::error::OLDER_DAEMON;
use crate::types::{GithubRepositoryList, NewLink, Repository, RepositoryLinks};
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

/// Workspace `workspace`'s Suggestions (spec §3): the Repositories its Panes work in that it
/// doesn't link, by name, each as GitHub showed it, so the GUI links one by its id. Not
/// filtered by what the GUI was told Not now. A daemon older than the route has none to give,
/// and lists none rather than failing.
pub async fn suggestions(
    client: &Client,
    workspace: WorkspaceId,
) -> Result<Vec<Repository>, ApiError> {
    match client.suggestion_list().id(workspace).send().await {
        Ok(suggestions) => Ok(suggestions.into_inner()),
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

/// Every GitHub repository the sign-in can reach, by `owner/name`, with how far loading them
/// has got (spec §8): `status`, `loaded`, the `total` estimated from the last page, and why a
/// load failed. The daemon keeps it in memory for the run; nothing loads until
/// [`refresh_github_list`] asks.
pub async fn github_list(client: &Client) -> Result<GithubRepositoryList, ApiError> {
    let list = client.github_repository_list().send().await?;
    Ok(list.into_inner())
}

/// Asks the daemon to load the GitHub repository list again, as each Link picker does when it
/// opens. Answers at once: pages arrive in the background, each with a `changed
/// github_repositories` event, and a list already loaded keeps showing until the new one has
/// loaded whole.
pub async fn refresh_github_list(client: &Client) -> Result<(), ApiError> {
    client.github_repository_refresh().send().await?;
    Ok(())
}
