//! What the GUI reads from the daemon over REST (spec §4), behind [`Api`] so a view takes a
//! fake in tests. The real one is the connected [`Daemon`].

use agentdeck_client::Client;
use agentdeck_client::daemon::Daemon;
use std::collections::BTreeSet;

use agentdeck_client::types::{NewPane, NewTab, Pane, Tab, TabPatch};
use agentdeck_core::{Layout, PaneId, PaneKind, SplitDirection, TabId, TaskId, WorkspaceId};
use gpui::{App, Task};

use crate::overview::{PaneState, Snapshot};
use crate::runtime;

/// The daemon's API, as the GUI uses it.
pub trait Api: Send + Sync {
    /// Everything the sidebar shows, read fresh.
    fn snapshot(&self, cx: &App) -> Task<anyhow::Result<Snapshot>>;

    /// `workspace`'s tabs, the panes in their layouts and the tasks those panes link.
    fn panes(&self, workspace: WorkspaceId, cx: &App) -> Task<anyhow::Result<WorkspacePanes>>;

    /// Adds a tab titled `shell` to `workspace`, running a shell in `dir` as its one pane;
    /// the new tab. A tab whose shell can't start is deleted again.
    fn new_shell_tab(
        &self,
        workspace: WorkspaceId,
        dir: String,
        cx: &App,
    ) -> Task<anyhow::Result<TabId>>;

    /// Splits a pane as `split` says: a shell takes the new side. The new pane; it is closed
    /// again if the layout can't take it.
    fn split_pane(&self, split: Split, cx: &App) -> Task<anyhow::Result<PaneId>>;

    /// Kills `pane`'s process and deletes it; the daemon takes it out of its tab's layout.
    fn close_pane(&self, pane: PaneId, cx: &App) -> Task<anyhow::Result<()>>;
}

/// A split of one pane, for [`Api::split_pane`].
#[derive(Debug, Clone, PartialEq)]
pub struct Split {
    /// The workspace holding the tab.
    pub workspace: WorkspaceId,
    /// The tab holding the pane.
    pub tab: TabId,
    /// The pane split; it keeps the first side.
    pub target: PaneId,
    /// Side by side ([`SplitDirection::Horizontal`]) or stacked.
    pub direction: SplitDirection,
    /// Where the new shell starts.
    pub cwd: String,
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

    fn new_shell_tab(
        &self,
        workspace: WorkspaceId,
        dir: String,
        cx: &App,
    ) -> Task<anyhow::Result<TabId>> {
        runtime::spawn(cx, new_shell_tab(self.client.clone(), workspace, dir))
    }

    fn split_pane(&self, split: Split, cx: &App) -> Task<anyhow::Result<PaneId>> {
        runtime::spawn(cx, split_pane(self.client.clone(), split))
    }

    fn close_pane(&self, pane: PaneId, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            client.pane_delete().id(pane).send().await?;
            Ok(())
        })
    }
}

/// Spawns a shell in `cwd` in `tab`: `$SHELL` (spec §5), placed by the caller.
async fn spawn_shell(client: &Client, tab: TabId, cwd: String) -> anyhow::Result<Pane> {
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
        .await?;
    Ok(pane.into_inner())
}

/// Sets `tab`'s layout.
async fn set_layout(client: &Client, tab: TabId, layout: Layout) -> anyhow::Result<()> {
    client
        .tab_update()
        .id(tab)
        .body(TabPatch {
            layout: Some(layout),
            ..TabPatch::default()
        })
        .send()
        .await?;
    Ok(())
}

/// See [`Api::new_shell_tab`].
async fn new_shell_tab(
    client: Client,
    workspace: WorkspaceId,
    dir: String,
) -> anyhow::Result<TabId> {
    let tab = client
        .tab_create()
        .id(workspace)
        .body(NewTab {
            layout: None,
            title: "shell".to_owned(),
        })
        .send()
        .await?
        .into_inner();
    let placed = async {
        let pane = spawn_shell(&client, tab.id, dir).await?;
        set_layout(&client, tab.id, Layout::single(pane.id)).await
    };
    if let Err(err) = placed.await {
        let _ = client.tab_delete().id(tab.id).send().await;
        return Err(err);
    }
    Ok(tab.id)
}

/// See [`Api::split_pane`]. The layout is read fresh, so a change made elsewhere since the
/// view's last read isn't undone.
async fn split_pane(client: Client, split: Split) -> anyhow::Result<PaneId> {
    let pane = spawn_shell(&client, split.tab, split.cwd).await?;
    let placed = async {
        let tabs = client.tab_list().id(split.workspace).send().await?;
        let mut layout = (tabs.iter().find(|tab| tab.id == split.tab))
            .and_then(|tab| tab.layout.clone())
            .ok_or_else(|| anyhow::anyhow!("the tab or its pane is gone"))?;
        layout.split(split.target, split.direction, pane.id)?;
        set_layout(&client, split.tab, layout).await
    };
    if let Err(err) = placed.await {
        let _ = client.pane_delete().id(pane.id).send().await;
        return Err(err);
    }
    Ok(pane.id)
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

    /// Answers every read with what it holds now, and counts the snapshot reads. Writes
    /// change what it holds, as the daemon would, but broadcast nothing.
    #[derive(Default)]
    pub struct FakeApi {
        snapshot: Mutex<Snapshot>,
        reads: Mutex<usize>,
        tabs: Mutex<HashMap<WorkspaceId, Vec<Tab>>>,
        panes: Mutex<Vec<Pane>>,
        tasks: Mutex<Vec<agentdeck_client::types::Task>>,
        /// The last id handed to a tab or pane it made.
        made: Mutex<u32>,
        /// Why the next write fails, if it should.
        failure: Mutex<Option<String>>,
        /// The panes closed, in order.
        pub closed: Mutex<Vec<PaneId>>,
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

        /// Fails the next write with `reason`, changing nothing.
        pub fn fail_next(&self, reason: &str) {
            *self.failure.lock().unwrap() = Some(reason.to_owned());
        }

        /// The layout of `workspace`'s tab `tab`.
        pub fn layout(&self, workspace: WorkspaceId, tab: TabId) -> Option<Layout> {
            let tabs = self.tabs.lock().unwrap();
            let tab = tabs.get(&workspace)?.iter().find(|t| t.id == tab)?;
            tab.layout.clone()
        }

        /// The scripted failure, if any, taking it.
        fn failure(&self) -> Option<anyhow::Error> {
            (self.failure.lock().unwrap().take()).map(anyhow::Error::msg)
        }

        /// A fresh id, from 101 up, so it can't clash with a fixture's.
        fn make_id<T: std::str::FromStr<Err: std::fmt::Debug>>(&self) -> T {
            let mut made = self.made.lock().unwrap();
            *made += 1;
            crate::overview::fixtures::id(100 + *made)
        }

        /// Adds a shell pane in `tab`, unplaced.
        fn spawn_shell(&self, tab: TabId, cwd: String) -> PaneId {
            let pane = Pane {
                cwd,
                ..crate::pane::fixtures::shell_pane(0, tab)
            };
            let pane = Pane {
                id: self.make_id(),
                ..pane
            };
            let id = pane.id;
            self.panes.lock().unwrap().push(pane);
            id
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

        fn new_shell_tab(
            &self,
            workspace: WorkspaceId,
            dir: String,
            _cx: &App,
        ) -> Task<anyhow::Result<TabId>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let tab: TabId = self.make_id();
            let pane = self.spawn_shell(tab, dir);
            let mut tabs = self.tabs.lock().unwrap();
            let tabs = tabs.entry(workspace).or_default();
            let position = tabs.iter().map(|t| t.position + 1).max().unwrap_or(0);
            tabs.push(Tab {
                id: tab,
                layout: Some(Layout::single(pane)),
                position,
                title: "shell".into(),
                workspace_id: workspace,
            });
            Task::ready(Ok(tab))
        }

        fn split_pane(&self, split: Split, _cx: &App) -> Task<anyhow::Result<PaneId>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let pane = self.spawn_shell(split.tab, split.cwd);
            let mut tabs = self.tabs.lock().unwrap();
            let layout = (tabs.get_mut(&split.workspace))
                .and_then(|tabs| tabs.iter_mut().find(|t| t.id == split.tab))
                .and_then(|tab| tab.layout.as_mut())
                .expect("the split's tab has a layout");
            layout
                .split(split.target, split.direction, pane)
                .expect("the split's target is in the layout");
            Task::ready(Ok(pane))
        }

        fn close_pane(&self, pane: PaneId, _cx: &App) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            for tab in self.tabs.lock().unwrap().values_mut().flatten() {
                if let Some(layout) = tab.layout.take() {
                    tab.layout = match layout.contains(pane) {
                        true => layout.remove(pane).expect("it contains the pane"),
                        false => Some(layout),
                    };
                }
            }
            self.panes.lock().unwrap().retain(|p| p.id != pane);
            self.closed.lock().unwrap().push(pane);
            Task::ready(Ok(()))
        }
    }
}
