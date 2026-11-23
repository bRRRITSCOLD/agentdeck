//! Making and editing Workspaces as the GUI's New workspace and Edit workspace do (spec §4
//! `POST` and `PATCH /v1/workspaces`, §11): a name and the Repositories to link and unlink, in
//! one request the daemon applies together or not at all.

use agentdeck_core::{RepositoryId, RepositoryName, WorkspaceId};

use crate::types::{NewWorkspace, Workspace, WorkspacePatch};
use crate::{ApiError, Client};

/// Makes a Workspace named `name` linking `link`, each a Repository's `owner/name`. One GitHub
/// can't see fails it all, as the daemon's `unknown_repository`, and makes nothing.
pub async fn create(
    client: &Client,
    name: String,
    link: Vec<RepositoryName>,
) -> Result<Workspace, ApiError> {
    let made = client
        .workspace_create()
        .body(NewWorkspace { name, link })
        .send()
        .await?;
    Ok(made.into_inner())
}

/// What an edit changes: a new name, if any, and the Repositories to link and unlink.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    /// The new name; `None` keeps the one it has.
    pub name: Option<String>,
    /// The Repositories to link, by `owner/name`.
    pub link: Vec<RepositoryName>,
    /// The Repositories to unlink, by id.
    pub unlink: Vec<RepositoryId>,
}

/// Applies `change` to `workspace` in one request: all of it, or, when the daemon refuses any
/// part (a blank name, a Repository GitHub can't see), none of it.
pub async fn update(
    client: &Client,
    workspace: WorkspaceId,
    change: Change,
) -> Result<Workspace, ApiError> {
    let Change { name, link, unlink } = change;
    let body = WorkspacePatch {
        name,
        muted: None,
        link,
        unlink,
    };
    let updated = client
        .workspace_update()
        .id(workspace)
        .body(body)
        .send()
        .await?;
    Ok(updated.into_inner())
}
