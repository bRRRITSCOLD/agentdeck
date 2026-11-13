//! Pane operations that take more than one call. The daemon leaves placing a pane in its tab's
//! layout to the caller (`POST /v1/panes`, `DELETE /v1/panes/{id}`), so each of these keeps
//! the layout and the panes in step, and undoes its first step when a later one fails.

use agentdeck_core::{Layout, LayoutError, PaneId, PaneKind, SplitDirection, TabId, WorkspaceId};

use crate::types::{NewPane, NewTab, Pane, TabPatch};
use crate::{ApiError, Client};

/// Why a pane operation failed.
#[derive(Debug, thiserror::Error)]
pub enum PaneOpError {
    /// The daemon refused a call, or couldn't be reached.
    #[error(transparent)]
    Api(#[from] ApiError),
    /// The layout couldn't take the change: the pane is no longer in it.
    #[error(transparent)]
    Layout(#[from] LayoutError),
    /// The tab, or its layout, went away meanwhile.
    #[error("the tab is gone")]
    TabGone,
}

/// Where a pane is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneAt {
    /// The workspace holding the tab.
    pub workspace: WorkspaceId,
    /// The tab whose layout holds the pane.
    pub tab: TabId,
    /// The pane.
    pub pane: PaneId,
}

/// `pane`, or `None` once it is gone: a layout read a moment before the pane was taken out
/// of it still names it.
pub async fn pane_if_any(client: &Client, pane: PaneId) -> Result<Option<Pane>, ApiError> {
    match client
        .pane_get()
        .id(pane)
        .send()
        .await
        .map_err(ApiError::from)
    {
        Ok(pane) => Ok(Some(pane.into_inner())),
        Err(err) if err.code.as_deref() == Some("not_found") => Ok(None),
        Err(err) => Err(err),
    }
}

/// Adds a tab titled `shell` to `workspace`, running `$SHELL` in `dir` as its one pane; the
/// new tab. A tab whose shell can't start or be placed is deleted again.
pub async fn new_shell_tab(
    client: &Client,
    workspace: WorkspaceId,
    dir: String,
) -> Result<TabId, PaneOpError> {
    let tab = client
        .tab_create()
        .id(workspace)
        .body(NewTab {
            layout: None,
            title: "shell".to_owned(),
        })
        .send()
        .await
        .map_err(ApiError::from)?
        .into_inner();
    let placed = async {
        let pane = spawn_shell(client, tab.id, dir).await?;
        set_layout(client, tab.id, Layout::single(pane.id)).await
    };
    if let Err(err) = placed.await {
        let _ = client.tab_delete().id(tab.id).send().await;
        return Err(err);
    }
    Ok(tab.id)
}

/// Splits the pane `at` in `direction`: it keeps the first side and `$SHELL` in `cwd` takes
/// the second. The new pane; it is closed again if the layout can't take it.
pub async fn split_pane(
    client: &Client,
    at: PaneAt,
    direction: SplitDirection,
    cwd: String,
) -> Result<PaneId, PaneOpError> {
    let pane = spawn_shell(client, at.tab, cwd).await?;
    let placed = async {
        let mut layout = layout(client, at.workspace, at.tab).await?;
        layout.split(at.pane, direction, pane.id)?;
        set_layout(client, at.tab, layout).await
    };
    if let Err(err) = placed.await {
        let _ = client.pane_delete().id(pane.id).send().await;
        return Err(err);
    }
    Ok(pane.id)
}

/// Takes the pane `at` out of its tab's layout, then kills its process and deletes it. The
/// tab's last pane takes the tab with it, since a layout can't be emptied again. The layout
/// loses the pane first, so no read finds it naming a pane that is gone.
pub async fn close_pane(client: &Client, at: PaneAt) -> Result<(), PaneOpError> {
    match layout(client, at.workspace, at.tab)
        .await?
        .remove(at.pane)?
    {
        Some(rest) => {
            set_layout(client, at.tab, rest).await?;
            client
                .pane_delete()
                .id(at.pane)
                .send()
                .await
                .map_err(ApiError::from)?;
        }
        None => {
            client
                .tab_delete()
                .id(at.tab)
                .send()
                .await
                .map_err(ApiError::from)?;
        }
    }
    Ok(())
}

/// `tab`'s layout, read fresh from `workspace`'s tabs, so a change made elsewhere since the
/// caller's last read isn't undone.
async fn layout(
    client: &Client,
    workspace: WorkspaceId,
    tab: TabId,
) -> Result<Layout, PaneOpError> {
    let tabs = client
        .tab_list()
        .id(workspace)
        .send()
        .await
        .map_err(ApiError::from)?;
    (tabs.iter().find(|t| t.id == tab))
        .and_then(|tab| tab.layout.clone())
        .ok_or(PaneOpError::TabGone)
}

/// Spawns `$SHELL` in `cwd` in `tab`, unplaced.
async fn spawn_shell(client: &Client, tab: TabId, cwd: String) -> Result<Pane, PaneOpError> {
    let pane = client
        .pane_create()
        .body(NewPane {
            agent: None,
            argv: None,
            cwd,
            kind: PaneKind::Shell,
            tab_id: tab,
            task_id: None,
        })
        .send()
        .await
        .map_err(ApiError::from)?;
    Ok(pane.into_inner())
}

/// Sets `tab`'s layout.
async fn set_layout(client: &Client, tab: TabId, layout: Layout) -> Result<(), PaneOpError> {
    client
        .tab_update()
        .id(tab)
        .body(TabPatch {
            layout: Some(layout),
            ..TabPatch::default()
        })
        .send()
        .await
        .map_err(ApiError::from)?;
    Ok(())
}
