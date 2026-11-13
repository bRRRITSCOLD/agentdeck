//! What the GUI reads from and asks of the daemon over REST (spec §4), behind [`Api`] so a
//! view takes a fake in tests. The real one is the connected [`Daemon`]. A failed call reads
//! as the daemon's own message, through [`ApiError`].

use std::collections::BTreeSet;

use agentdeck_client::daemon::Daemon;
use agentdeck_client::panes::{self, pane_if_any};
use agentdeck_client::types::{NewNote, NewWorkspace, Pane, Tab, TaskPatch};
use agentdeck_client::{ApiError, Client};
use agentdeck_core::{PaneId, SplitDirection, TabId, TaskId, TaskStatus, WorkspaceId};
use gpui::{App, Task};

use crate::new_workspace::Request;
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

    /// Takes the pane out of its tab's layout, then kills its process and deletes it. The
    /// tab's last pane takes the tab with it: a layout can't be emptied again.
    fn close_pane(&self, pane: PaneAt, cx: &App) -> Task<anyhow::Result<()>>;

    /// Makes a workspace as `request` says; its id. A refusal is an [`ApiError`] whose code
    /// names the field: `invalid_name` or `invalid_dir`.
    fn new_workspace(&self, request: Request, cx: &App) -> Task<anyhow::Result<WorkspaceId>>;

    /// Moves `task` to `status`.
    fn set_task_status(
        &self,
        task: TaskId,
        status: TaskStatus,
        cx: &App,
    ) -> Task<anyhow::Result<()>>;

    /// Syncs PRs now, returning once the sync is done.
    fn sync_prs(&self, cx: &App) -> Task<anyhow::Result<()>>;

    /// Leaves a note with `body` on `workspace`.
    fn new_note(&self, workspace: WorkspaceId, body: String, cx: &App) -> Task<anyhow::Result<()>>;
}

pub use agentdeck_client::panes::PaneAt;

/// A split of one pane, for [`Api::split_pane`].
#[derive(Debug, Clone, PartialEq)]
pub struct Split {
    /// The pane split; it keeps the first side.
    pub at: PaneAt,
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
    /// Every pane in the tabs' layouts that still exists.
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
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            Ok(panes::new_shell_tab(&client, workspace, dir).await?)
        })
    }

    fn split_pane(&self, split: Split, cx: &App) -> Task<anyhow::Result<PaneId>> {
        let client = self.client.clone();
        let Split { at, direction, cwd } = split;
        runtime::spawn(cx, async move {
            Ok(panes::split_pane(&client, at, direction, cwd).await?)
        })
    }

    fn close_pane(&self, pane: PaneAt, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(
            cx,
            async move { Ok(panes::close_pane(&client, pane).await?) },
        )
    }

    fn new_workspace(&self, request: Request, cx: &App) -> Task<anyhow::Result<WorkspaceId>> {
        let client = self.client.clone();
        let Request { name, dir } = request;
        runtime::spawn(cx, async move {
            let made = client
                .workspace_create()
                .body(NewWorkspace { dir, name })
                .send()
                .await
                .map_err(ApiError::from)?;
            Ok(made.id)
        })
    }

    fn set_task_status(
        &self,
        task: TaskId,
        status: TaskStatus,
        cx: &App,
    ) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let patch = TaskPatch {
                status: Some(status),
                ..TaskPatch::default()
            };
            (client.task_update().id(task).body(patch).send().await).map_err(ApiError::from)?;
            Ok(())
        })
    }

    fn sync_prs(&self, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            client.prs_sync().send().await.map_err(ApiError::from)?;
            Ok(())
        })
    }

    fn new_note(&self, workspace: WorkspaceId, body: String, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let note = NewNote {
                body,
                task_id: None,
                workspace_id: Some(workspace),
            };
            client
                .note_create()
                .body(note)
                .send()
                .await
                .map_err(ApiError::from)?;
            Ok(())
        })
    }
}

/// Reads a [`WorkspacePanes`]: the tabs, then each pane in their layouts, then each task
/// those panes link.
async fn panes(client: Client, workspace: WorkspaceId) -> anyhow::Result<WorkspacePanes> {
    let tabs = client
        .tab_list()
        .id(workspace)
        .send()
        .await
        .map_err(ApiError::from)?
        .into_inner();
    let mut panes = Vec::new();
    for pane in tabs
        .iter()
        .filter_map(|tab| tab.layout.as_ref())
        .flat_map(|l| l.panes())
    {
        panes.extend(pane_if_any(&client, pane).await?);
    }
    let linked: BTreeSet<TaskId> = panes.iter().filter_map(|pane| pane.task_id).collect();
    let mut tasks = Vec::new();
    for task in linked {
        let task = client.task_get().id(task).send().await;
        tasks.push(task.map_err(ApiError::from)?.into_inner());
    }
    Ok(WorkspacePanes { tabs, panes, tasks })
}

/// Reads a [`Snapshot`]: the workspaces, then each live one's tabs and the panes in their
/// layouts, then the tasks, unread notes and PR sync status.
async fn snapshot(client: Client) -> anyhow::Result<Snapshot> {
    let workspaces = (client.workspace_list().send().await)
        .map_err(ApiError::from)?
        .into_inner();
    let mut panes = Vec::new();
    for workspace in workspaces.iter().filter(|w| w.archived_at.is_none()) {
        let tabs = client
            .tab_list()
            .id(workspace.id)
            .send()
            .await
            .map_err(ApiError::from)?
            .into_inner();
        for pane in tabs
            .iter()
            .filter_map(|tab| tab.layout.as_ref())
            .flat_map(|l| l.panes())
        {
            if let Some(pane) = pane_if_any(&client, pane).await? {
                panes.push(PaneState {
                    pane: pane.id,
                    workspace: workspace.id,
                    state: pane.state,
                    agent: pane.agent,
                    task: pane.task_id,
                });
            }
        }
    }
    Ok(Snapshot {
        workspaces,
        panes,
        tasks: (client.task_list().send().await)
            .map_err(ApiError::from)?
            .into_inner(),
        notes: (client.note_list().send().await)
            .map_err(ApiError::from)?
            .into_inner(),
        pr_sync: (client.prs_status().send().await)
            .map_err(ApiError::from)?
            .into_inner(),
    })
}

/// A scripted [`Api`] for tests.
#[cfg(test)]
pub(crate) mod fake {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use agentdeck_core::{Layout, PaneId};

    use super::*;

    /// Answers every read with what it holds now, and counts the snapshot reads. Writes
    /// change what it holds, as the daemon would, but broadcast nothing.
    #[derive(Default)]
    pub struct FakeApi {
        snapshot: Mutex<Snapshot>,
        reads: Mutex<usize>,
        /// How many times a workspace's tabs and panes were read.
        pane_reads: Mutex<usize>,
        tabs: Mutex<HashMap<WorkspaceId, Vec<Tab>>>,
        panes: Mutex<Vec<Pane>>,
        tasks: Mutex<Vec<agentdeck_client::types::Task>>,
        /// The last id handed to a tab or pane it made.
        made: Mutex<u32>,
        /// Why the next write fails, if it should.
        failure: Mutex<Option<anyhow::Error>>,
        /// The workspaces asked for, in order.
        pub made_workspaces: Mutex<Vec<Request>>,
        /// How many PR syncs were asked for.
        pub syncs: Mutex<usize>,
        /// The notes left, with the workspace each is on.
        pub notes_left: Mutex<Vec<(WorkspaceId, String)>>,
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

        /// How many times a workspace's tabs and panes were read.
        pub fn pane_reads(&self) -> usize {
            *self.pane_reads.lock().unwrap()
        }

        /// Fails the next write with `reason`, changing nothing.
        pub fn fail_next(&self, reason: &str) {
            *self.failure.lock().unwrap() = Some(anyhow::Error::msg(reason.to_owned()));
        }

        /// Fails the next write as the daemon refuses one: `message`, with `code`.
        pub fn refuse_next(&self, code: &str, message: &str) {
            let refusal = ApiError {
                message: message.to_owned(),
                code: Some(code.to_owned()),
            };
            *self.failure.lock().unwrap() = Some(refusal.into());
        }

        /// The layout of `workspace`'s tab `tab`.
        pub fn layout(&self, workspace: WorkspaceId, tab: TabId) -> Option<Layout> {
            let tabs = self.tabs.lock().unwrap();
            let tab = tabs.get(&workspace)?.iter().find(|t| t.id == tab)?;
            tab.layout.clone()
        }

        /// The scripted failure, if any, taking it.
        fn failure(&self) -> Option<anyhow::Error> {
            self.failure.lock().unwrap().take()
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
            *self.pane_reads.lock().unwrap() += 1;
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
            let Split { at, direction, cwd } = split;
            let pane = self.spawn_shell(at.tab, cwd);
            let mut tabs = self.tabs.lock().unwrap();
            let layout = (tabs.get_mut(&at.workspace))
                .and_then(|tabs| tabs.iter_mut().find(|t| t.id == at.tab))
                .and_then(|tab| tab.layout.as_mut())
                .expect("the split's tab has a layout");
            layout
                .split(at.pane, direction, pane)
                .expect("the split's pane is in the layout");
            Task::ready(Ok(pane))
        }

        fn close_pane(&self, at: PaneAt, _cx: &App) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let mut tabs = self.tabs.lock().unwrap();
            let tabs = tabs.entry(at.workspace).or_default();
            // Closing a pane already gone is done, as with the daemon.
            let holder = tabs
                .iter_mut()
                .find(|t| t.id == at.tab && t.layout.as_ref().is_some_and(|l| l.contains(at.pane)));
            if let Some(tab) = holder {
                let layout = tab.layout.take().expect("it holds the pane");
                match layout.remove(at.pane).expect("it holds the pane") {
                    Some(rest) => tab.layout = Some(rest),
                    None => tabs.retain(|t| t.id != at.tab),
                }
            }
            self.panes.lock().unwrap().retain(|p| p.id != at.pane);
            self.closed.lock().unwrap().push(at.pane);
            Task::ready(Ok(()))
        }

        fn new_workspace(&self, request: Request, _cx: &App) -> Task<anyhow::Result<WorkspaceId>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let id: WorkspaceId = self.make_id();
            let made = agentdeck_client::types::Workspace {
                id,
                ..crate::overview::fixtures::workspace(0, &request.name)
            };
            self.snapshot.lock().unwrap().workspaces.push(made);
            self.made_workspaces.lock().unwrap().push(request);
            Task::ready(Ok(id))
        }

        fn set_task_status(
            &self,
            task: TaskId,
            status: TaskStatus,
            _cx: &App,
        ) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let mut snapshot = self.snapshot.lock().unwrap();
            if let Some(task) = snapshot.tasks.iter_mut().find(|t| t.id == task) {
                task.status = status;
            }
            Task::ready(Ok(()))
        }

        fn sync_prs(&self, _cx: &App) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            *self.syncs.lock().unwrap() += 1;
            Task::ready(Ok(()))
        }

        fn new_note(
            &self,
            workspace: WorkspaceId,
            body: String,
            _cx: &App,
        ) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            self.notes_left.lock().unwrap().push((workspace, body));
            Task::ready(Ok(()))
        }
    }
}
