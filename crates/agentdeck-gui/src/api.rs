//! What the GUI reads from the daemon over REST (spec §4), behind [`Api`] so a view takes a
//! fake in tests. The real one is the connected [`Daemon`].

use agentdeck_client::Client;
use agentdeck_client::daemon::Daemon;
use std::collections::BTreeSet;

use agentdeck_client::types::{Pane, Tab};
use agentdeck_core::{TaskId, WorkspaceId};
use gpui::{App, Task};

use crate::overview::{PaneState, Snapshot};
use crate::runtime;

/// The daemon's API, as the GUI uses it.
pub trait Api: Send + Sync {
    /// Everything the sidebar shows, read fresh.
    fn snapshot(&self, cx: &App) -> Task<anyhow::Result<Snapshot>>;

    /// `workspace`'s tabs, the panes in their layouts and the tasks those panes link.
    fn panes(&self, workspace: WorkspaceId, cx: &App) -> Task<anyhow::Result<WorkspacePanes>>;
}

/// What the workspace view shows, read in one go.
#[derive(Debug, Clone, Default)]
pub struct WorkspacePanes {
    /// The tabs, in `position` order.
    pub tabs: Vec<Tab>,
    /// Every pane in the tabs' layouts.
    pub panes: Vec<Pane>,
    /// Every task a pane links.
    pub tasks: Vec<agentdeck_client::types::Task>,
}

impl Api for Daemon {
    fn snapshot(&self, cx: &App) -> Task<anyhow::Result<Snapshot>> {
        runtime::spawn(cx, snapshot(self.client.clone()))
    }

    fn panes(&self, workspace: WorkspaceId, cx: &App) -> Task<anyhow::Result<WorkspacePanes>> {
        runtime::spawn(cx, panes(self.client.clone(), workspace))
    }
}

/// Reads a [`WorkspacePanes`]: the tabs, then each pane in their layouts, then each task
/// those panes link.
async fn panes(client: Client, workspace: WorkspaceId) -> anyhow::Result<WorkspacePanes> {
    let tabs = client.tab_list().id(workspace).send().await?.into_inner();
    let mut panes = Vec::new();
    for pane in tabs
        .iter()
        .filter_map(|tab| tab.layout.as_ref())
        .flat_map(|l| l.panes())
    {
        panes.push(client.pane_get().id(pane).send().await?.into_inner());
    }
    let linked: BTreeSet<TaskId> = panes.iter().filter_map(|pane| pane.task_id).collect();
    let mut tasks = Vec::new();
    for task in linked {
        tasks.push(client.task_get().id(task).send().await?.into_inner());
    }
    Ok(WorkspacePanes { tabs, panes, tasks })
}

/// Reads a [`Snapshot`]: the workspaces, then each live one's tabs and the panes in their
/// layouts, then the tasks, unread notes and PR sync status.
async fn snapshot(client: Client) -> anyhow::Result<Snapshot> {
    let workspaces = client.workspace_list().send().await?.into_inner();
    let mut panes = Vec::new();
    for workspace in workspaces.iter().filter(|w| w.archived_at.is_none()) {
        let tabs = client
            .tab_list()
            .id(workspace.id)
            .send()
            .await?
            .into_inner();
        for pane in tabs
            .iter()
            .filter_map(|tab| tab.layout.as_ref())
            .flat_map(|l| l.panes())
        {
            let pane = client.pane_get().id(pane).send().await?.into_inner();
            panes.push(PaneState {
                pane: pane.id,
                workspace: workspace.id,
                state: pane.state,
            });
        }
    }
    Ok(Snapshot {
        workspaces,
        panes,
        tasks: client.task_list().send().await?.into_inner(),
        unread_notes: client.note_list().unread(true).send().await?.into_inner(),
        pr_sync: client.prs_status().send().await?.into_inner(),
    })
}

/// A scripted [`Api`] for tests.
#[cfg(test)]
pub(crate) mod fake {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use agentdeck_core::PaneId;

    use super::*;

    /// Answers every read with what it holds now, and counts the snapshot reads.
    #[derive(Default)]
    pub struct FakeApi {
        snapshot: Mutex<Snapshot>,
        reads: Mutex<usize>,
        tabs: Mutex<HashMap<WorkspaceId, Vec<Tab>>>,
        panes: Mutex<Vec<Pane>>,
        tasks: Mutex<Vec<agentdeck_client::types::Task>>,
    }

    impl FakeApi {
        /// A fake that answers with `snapshot`.
        pub fn with(snapshot: Snapshot) -> Self {
            FakeApi {
                snapshot: Mutex::new(snapshot),
                ..FakeApi::default()
            }
        }

        /// Answers later reads with `snapshot`.
        pub fn set(&self, snapshot: Snapshot) {
            *self.snapshot.lock().unwrap() = snapshot;
        }

        /// Answers later reads of `workspace`'s tabs with `tabs`.
        pub fn set_tabs(&self, workspace: WorkspaceId, tabs: Vec<Tab>) {
            self.tabs.lock().unwrap().insert(workspace, tabs);
        }

        /// Answers later reads with `panes`, those in the workspace's layouts.
        pub fn set_panes(&self, panes: Vec<Pane>) {
            *self.panes.lock().unwrap() = panes;
        }

        /// Answers later reads with `tasks`, those a pane links.
        pub fn set_tasks(&self, tasks: Vec<agentdeck_client::types::Task>) {
            *self.tasks.lock().unwrap() = tasks;
        }

        /// How many snapshots were read.
        pub fn reads(&self) -> usize {
            *self.reads.lock().unwrap()
        }
    }

    impl Api for FakeApi {
        fn snapshot(&self, _cx: &App) -> Task<anyhow::Result<Snapshot>> {
            *self.reads.lock().unwrap() += 1;
            Task::ready(Ok(self.snapshot.lock().unwrap().clone()))
        }

        fn panes(&self, workspace: WorkspaceId, _cx: &App) -> Task<anyhow::Result<WorkspacePanes>> {
            let mut tabs = self
                .tabs
                .lock()
                .unwrap()
                .get(&workspace)
                .cloned()
                .unwrap_or_default();
            tabs.sort_by_key(|tab| tab.position);
            let shown = |pane: &PaneId| {
                tabs.iter()
                    .filter_map(|tab| tab.layout.as_ref())
                    .any(|layout| layout.contains(*pane))
            };
            let panes: Vec<Pane> = (self.panes.lock().unwrap().iter())
                .filter(|pane| shown(&pane.id))
                .cloned()
                .collect();
            let tasks = (self.tasks.lock().unwrap().iter())
                .filter(|task| panes.iter().any(|pane| pane.task_id == Some(task.id)))
                .cloned()
                .collect();
            Task::ready(Ok(WorkspacePanes { tabs, panes, tasks }))
        }
    }
}
