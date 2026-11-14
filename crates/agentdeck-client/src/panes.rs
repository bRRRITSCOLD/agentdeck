//! Pane operations that take more than one call. The daemon leaves placing a pane in its tab's
//! layout to the caller (`POST /v1/panes`, `DELETE /v1/panes/{id}`), so each of these keeps
//! the layout and the panes in step, and undoes its first step when a later one fails.

use agentdeck_core::{
    Agent, Layout, LayoutError, PaneId, PaneKind, SplitDirection, TabId, TaskId, WorkspaceId,
};

use crate::types::{NewPane, NewTab, Pane, PanePatch, Tab, TabPatch};
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

/// Links `pane` to `task`, or unlinks it with `None` (spec §4); the pane as it is now. The
/// task's comments no agent has had yet then go to the pane once it is idle (spec §9).
pub async fn link_task(
    client: &Client,
    pane: PaneId,
    task: Option<TaskId>,
) -> Result<Pane, ApiError> {
    let linked = client
        .pane_update()
        .id(pane)
        .body(PanePatch { task_id: task })
        .send()
        .await?;
    Ok(linked.into_inner())
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

/// Launches `agent` in `cwd`, linked to `task`, in `workspace`'s first tab with no panes, or
/// in a new tab titled after the agent, and places it there; the pane (spec §6). A tab made
/// for it is deleted again if the agent can't start.
pub async fn launch_agent(
    client: &Client,
    workspace: WorkspaceId,
    agent: Agent,
    cwd: String,
    task: Option<TaskId>,
) -> Result<Pane, ApiError> {
    let tabs = client
        .tab_list()
        .id(workspace)
        .send()
        .await
        .map_err(ApiError::from)?;
    let (tab, created) = match empty_tab(&tabs) {
        Some(tab) => (tab, false),
        None => {
            let tab = client
                .tab_create()
                .id(workspace)
                .body(NewTab {
                    layout: None,
                    title: agent.to_string(),
                })
                .send()
                .await
                .map_err(ApiError::from)?;
            (tab.id, true)
        }
    };
    let spawned = client
        .pane_create()
        .body(NewPane {
            agent: Some(agent),
            argv: None,
            cwd,
            kind: PaneKind::Agent,
            tab_id: tab,
            task_id: task,
        })
        .send()
        .await;
    let pane = match spawned {
        Ok(pane) => pane.into_inner(),
        Err(err) => {
            if created {
                // Don't leave the tab made for this agent behind, empty.
                let _ = client.tab_delete().id(tab).send().await;
            }
            return Err(err.into());
        }
    };
    client
        .tab_update()
        .id(tab)
        .body(TabPatch {
            layout: Some(Layout::single(pane.id)),
            ..TabPatch::default()
        })
        .send()
        .await
        .map_err(ApiError::from)?;
    Ok(pane)
}

/// The first tab with no panes, which a new agent can fill.
fn empty_tab(tabs: &[Tab]) -> Option<TabId> {
    tabs.iter()
        .find(|tab| tab.layout.is_none())
        .map(|tab| tab.id)
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
///
/// Closing is done once the pane is gone, so closing a pane already closed (a second click,
/// or another client) succeeds.
pub async fn close_pane(client: &Client, at: PaneAt) -> Result<(), PaneOpError> {
    let layout = match layout(client, at.workspace, at.tab).await {
        Ok(layout) => layout,
        // The tab went, and its panes with it.
        Err(PaneOpError::TabGone) => return Ok(()),
        Err(err) => return Err(err),
    };
    match layout.remove(at.pane) {
        Ok(Some(rest)) => set_layout(client, at.tab, rest).await?,
        Ok(None) => {
            let deleted = client.tab_delete().id(at.tab).send().await;
            return gone_is_done(deleted.map_err(ApiError::from));
        }
        // Out of the layout already; delete it in case it outlived its place.
        Err(LayoutError::PaneNotFound(_)) => {}
        Err(err) => return Err(err.into()),
    }
    let deleted = client.pane_delete().id(at.pane).send().await;
    gone_is_done(deleted.map_err(ApiError::from))
}

/// A delete that found nothing to delete still leaves it gone.
fn gone_is_done<T>(deleted: Result<T, ApiError>) -> Result<(), PaneOpError> {
    match deleted {
        Ok(_) => Ok(()),
        Err(err) if err.code.as_deref() == Some("not_found") => Ok(()),
        Err(err) => Err(err.into()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: u8, layout: Option<Layout>) -> Tab {
        Tab {
            id: format!("0190a1b2-c3d4-7e5f-8a9b-0000000000{id:02}")
                .parse()
                .unwrap(),
            layout,
            position: i64::from(id),
            title: "t".into(),
            workspace_id: "0190a1b2-c3d4-7e5f-8a9b-000000000099".parse().unwrap(),
        }
    }

    fn pane() -> Layout {
        let pane: PaneId = "0190a1b2-c3d4-7e5f-8a9b-000000000077".parse().unwrap();
        Layout::single(pane)
    }

    mod empty_tab {
        use super::*;

        #[test]
        fn should_pick_the_first_tab_without_panes() {
            let tabs = [tab(1, Some(pane())), tab(2, None), tab(3, None)];

            assert_eq!(empty_tab(&tabs), Some(tabs[1].id));
        }

        #[test]
        fn should_be_none_when_every_tab_has_panes() {
            assert_eq!(empty_tab(&[tab(1, Some(pane()))]), None);
            assert_eq!(empty_tab(&[]), None);
        }
    }
}
