//! What the GUI reads from and asks of the daemon over REST (spec §4), behind [`Api`] so a
//! view takes a fake in tests. The real one is the connected [`Daemon`]. A failed call reads
//! as the daemon's own message, through [`ApiError`].

use std::collections::BTreeSet;

use agentdeck_client::daemon::Daemon;
use agentdeck_client::instructions;
use agentdeck_client::panes::{self, pane_if_any};
use agentdeck_client::types::{
    Comment, DirtyReport, InstructionPatch, NewComment, NewInstruction, NewTask, NewWorkspace,
    Pane, PrClose, PrCloseResult, Settings, SettingsPatch, Tab, TaskPatch,
};
use agentdeck_client::{ApiError, Client};
use agentdeck_core::{
    InstructionId, PaneId, SplitDirection, TabId, TaskId, TaskStatus, WorkspaceId,
};
use gpui::{App, Task};

use crate::new_agent::{Launch, Launched};
use crate::new_task;
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

    /// Links `pane` to `task`, or unlinks it with `None` (spec §4). A refusal is an
    /// [`ApiError`]: `unknown_task`, or `invalid_task` for a task in another workspace or done.
    fn link_task(&self, pane: PaneId, task: Option<TaskId>, cx: &App) -> Task<anyhow::Result<()>>;

    /// Makes a workspace as `request` says; its id. A refusal is an [`ApiError`] whose code
    /// names the field: `invalid_name` or `invalid_dir`.
    fn new_workspace(&self, request: Request, cx: &App) -> Task<anyhow::Result<WorkspaceId>>;

    /// Makes a `todo` task as `request` says; its id.
    fn new_task(&self, request: new_task::Request, cx: &App) -> Task<anyhow::Result<TaskId>>;

    /// Moves `task` to `status`.
    fn set_task_status(
        &self,
        task: TaskId,
        status: TaskStatus,
        cx: &App,
    ) -> Task<anyhow::Result<()>>;

    /// Syncs PRs now, returning once the sync is done.
    fn sync_prs(&self, cx: &App) -> Task<anyhow::Result<()>>;

    /// Closes the PRs of the PR tasks `tasks` on GitHub (spec §8): one result per task, in
    /// order, each saying whether it closed and why not. Only a request that fails as a whole
    /// is an error.
    fn close_prs(&self, tasks: Vec<TaskId>, cx: &App) -> Task<anyhow::Result<Vec<PrCloseResult>>>;

    /// Starts an agent as `launch` says and places it in a tab (spec §6): the workspace's
    /// first empty tab, or a new one. A refusal is an [`ApiError`]; `invalid_argv` means the
    /// agent CLI couldn't start, and its message says why.
    fn launch_agent(&self, launch: Launch, cx: &App) -> Task<anyhow::Result<Launched>>;

    /// Types an instruction's `body` into `pane` at once with Enter (spec §9). The
    /// instruction stays as it is.
    fn send_instruction(&self, body: String, pane: PaneId, cx: &App) -> Task<anyhow::Result<()>>;

    /// Adds an instruction with `body` to `workspace`.
    fn new_instruction(
        &self,
        workspace: WorkspaceId,
        body: String,
        cx: &App,
    ) -> Task<anyhow::Result<()>>;

    /// Replaces `instruction`'s text with `body`.
    fn edit_instruction(
        &self,
        instruction: InstructionId,
        body: String,
        cx: &App,
    ) -> Task<anyhow::Result<()>>;

    /// Deletes `instruction`.
    fn delete_instruction(&self, instruction: InstructionId, cx: &App) -> Task<anyhow::Result<()>>;

    /// `task`'s comments, oldest first.
    fn comments(&self, task: TaskId, cx: &App) -> Task<anyhow::Result<Vec<Comment>>>;

    /// Adds the user's comment with `body` to `task`, for its agent (spec §9).
    fn new_comment(&self, task: TaskId, body: String, cx: &App) -> Task<anyhow::Result<()>>;

    /// Marks every agent comment on `task` seen, which clears its row's unseen mark.
    fn mark_comments_seen(&self, task: TaskId, cx: &App) -> Task<anyhow::Result<()>>;

    /// Archives `workspace`, keeping its dir, tasks and instructions. A refusal is an [`ApiError`];
    /// `panes_running` means a pane of it still runs.
    fn archive_workspace(&self, workspace: WorkspaceId, cx: &App) -> Task<anyhow::Result<()>>;

    /// Brings the archived `workspace` back.
    fn unarchive_workspace(&self, workspace: WorkspaceId, cx: &App) -> Task<anyhow::Result<()>>;

    /// Mutes `workspace`'s notifications, or unmutes them.
    fn set_muted(&self, workspace: WorkspaceId, muted: bool, cx: &App) -> Task<anyhow::Result<()>>;

    /// The git repos under `workspace`'s dir that hold work on no remote.
    fn dirty(&self, workspace: WorkspaceId, cx: &App) -> Task<anyhow::Result<DirtyReport>>;

    /// Deletes `workspace`, dirty or not: its tasks move to the Inbox, its instructions go, and so
    /// does its dir when agentdeck made it. Only call it once the user has seen the dirty
    /// report.
    fn delete_workspace(&self, workspace: WorkspaceId, cx: &App) -> Task<anyhow::Result<()>>;

    /// Every setting in `config.toml`, as the daemon uses it (spec §2).
    fn settings(&self, cx: &App) -> Task<anyhow::Result<Settings>>;

    /// Changes the settings `patch` names and returns them all. A refusal is an [`ApiError`];
    /// `invalid_setting` names the key in its message.
    fn update_settings(&self, patch: SettingsPatch, cx: &App) -> Task<anyhow::Result<Settings>>;
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

    fn link_task(&self, pane: PaneId, task: Option<TaskId>, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            panes::link_task(&client, pane, task).await?;
            Ok(())
        })
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

    fn new_task(&self, request: new_task::Request, cx: &App) -> Task<anyhow::Result<TaskId>> {
        let client = self.client.clone();
        let new_task::Request {
            title,
            body,
            workspace,
        } = request;
        runtime::spawn(cx, async move {
            let task = NewTask {
                body: (!body.is_empty()).then_some(body),
                status: Some(TaskStatus::Todo),
                title,
                workspace_id: workspace,
            };
            let made = (client.task_create().body(task).send().await).map_err(ApiError::from)?;
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

    fn close_prs(&self, tasks: Vec<TaskId>, cx: &App) -> Task<anyhow::Result<Vec<PrCloseResult>>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let closed = (client
                .prs_close()
                .body(PrClose { task_ids: tasks })
                .send()
                .await)
                .map_err(ApiError::from)?;
            Ok(closed.into_inner().results)
        })
    }

    fn launch_agent(&self, launch: Launch, cx: &App) -> Task<anyhow::Result<Launched>> {
        let client = self.client.clone();
        let Launch {
            workspace,
            agent,
            cwd,
            task,
        } = launch;
        runtime::spawn(cx, async move {
            let pane = panes::launch_agent(&client, workspace, agent, cwd, task).await?;
            Ok(Launched {
                workspace,
                tab: pane.tab_id,
                pane: pane.id,
            })
        })
    }

    fn send_instruction(&self, body: String, pane: PaneId, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            Ok(instructions::send_instruction(&client, &body, pane).await?)
        })
    }

    fn new_instruction(
        &self,
        workspace: WorkspaceId,
        body: String,
        cx: &App,
    ) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            client
                .instruction_create()
                .id(workspace)
                .body(NewInstruction { body })
                .send()
                .await
                .map_err(ApiError::from)?;
            Ok(())
        })
    }

    fn edit_instruction(
        &self,
        instruction: InstructionId,
        body: String,
        cx: &App,
    ) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let patch = InstructionPatch { body: Some(body) };
            (client
                .instruction_update()
                .id(instruction)
                .body(patch)
                .send()
                .await)
                .map_err(ApiError::from)?;
            Ok(())
        })
    }

    fn delete_instruction(&self, instruction: InstructionId, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            (client.instruction_delete().id(instruction).send().await).map_err(ApiError::from)?;
            Ok(())
        })
    }

    fn comments(&self, task: TaskId, cx: &App) -> Task<anyhow::Result<Vec<Comment>>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let listed = client.comment_list().id(task).send().await;
            Ok(listed.map_err(ApiError::from)?.into_inner())
        })
    }

    fn new_comment(&self, task: TaskId, body: String, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let comment = NewComment { body, pane: None };
            (client.comment_create().id(task).body(comment).send().await)
                .map_err(ApiError::from)?;
            Ok(())
        })
    }

    fn mark_comments_seen(&self, task: TaskId, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            (client.comment_seen().id(task).send().await).map_err(ApiError::from)?;
            Ok(())
        })
    }

    fn archive_workspace(&self, workspace: WorkspaceId, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            (client.workspace_archive().id(workspace).send().await).map_err(ApiError::from)?;
            Ok(())
        })
    }

    fn unarchive_workspace(&self, workspace: WorkspaceId, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            (client.workspace_unarchive().id(workspace).send().await).map_err(ApiError::from)?;
            Ok(())
        })
    }

    fn set_muted(&self, workspace: WorkspaceId, muted: bool, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let patch = client.workspace_update().id(workspace);
            let patch = patch.body_map(|patch| patch.muted(Some(muted)));
            patch.send().await.map_err(ApiError::from)?;
            Ok(())
        })
    }

    fn dirty(&self, workspace: WorkspaceId, cx: &App) -> Task<anyhow::Result<DirtyReport>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let report = client.workspace_dirty().id(workspace).send().await;
            Ok(report.map_err(ApiError::from)?.into_inner())
        })
    }

    fn delete_workspace(&self, workspace: WorkspaceId, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let deleting = client.workspace_delete().id(workspace).force(true).send();
            deleting.await.map_err(ApiError::from)?;
            Ok(())
        })
    }

    fn settings(&self, cx: &App) -> Task<anyhow::Result<Settings>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let settings = (client.settings_get().send().await).map_err(ApiError::from)?;
            Ok(settings.into_inner())
        })
    }

    fn update_settings(&self, patch: SettingsPatch, cx: &App) -> Task<anyhow::Result<Settings>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let settings =
                (client.settings_update().body(patch).send().await).map_err(ApiError::from)?;
            Ok(settings.into_inner())
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
/// layouts, then the tasks, every workspace's instructions and PR sync status.
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
        let placed = (tabs.iter()).flat_map(|tab| {
            let panes = tab.layout.as_ref().map(|l| l.panes()).unwrap_or_default();
            panes.into_iter().map(move |pane| (tab, pane))
        });
        for (tab, pane) in placed {
            if let Some(pane) = pane_if_any(&client, pane).await? {
                panes.push(PaneState {
                    pane: pane.id,
                    workspace: workspace.id,
                    state: pane.state,
                    agent: pane.agent,
                    task: pane.task_id,
                    tab: tab.title.clone(),
                });
            }
        }
    }
    let mut instructions = Vec::new();
    for workspace in &workspaces {
        let listed =
            (client.instruction_list().id(workspace.id).send().await).map_err(ApiError::from)?;
        instructions.extend(listed.into_inner());
    }
    Ok(Snapshot {
        workspaces,
        panes,
        tasks: (client.task_list().send().await)
            .map_err(ApiError::from)?
            .into_inner(),
        instructions,
        pr_sync: Some(
            (client.prs_status().send().await)
                .map_err(ApiError::from)?
                .into_inner(),
        ),
    })
}

/// A scripted [`Api`] for tests.
#[cfg(test)]
pub(crate) mod fake {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use agentdeck_core::{Layout, PaneId};

    use super::*;

    /// The daemon's defaults: the system theme, a 90-day PR cutoff and `xdg-open`, nothing
    /// overridden.
    pub fn default_settings() -> Settings {
        use agentdeck_client::types::{
            AppearanceSettings, PrSyncSettings, TerminalSettings, ThemePreference,
        };

        Settings {
            appearance: AppearanceSettings {
                theme: ThemePreference::System,
            },
            pr_sync: PrSyncSettings { max_age_days: 90 },
            terminal: TerminalSettings {
                file_opener: agentdeck_core::opener::DEFAULT.into(),
            },
            overridden_by_env: Vec::new(),
        }
    }

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
        /// The tasks asked for, in order.
        pub made_tasks: Mutex<Vec<new_task::Request>>,
        /// How many PR syncs were asked for.
        pub syncs: Mutex<usize>,
        /// The instructions added, with the workspace each is for.
        pub instructions_added: Mutex<Vec<(WorkspaceId, String)>>,
        /// The instructions sent to panes: the text and the pane.
        pub sent_instructions: Mutex<Vec<(String, PaneId)>>,
        /// The instructions edited, with the new text, in order.
        pub edited_instructions: Mutex<Vec<(InstructionId, String)>>,
        /// The instructions deleted, in order.
        pub deleted_instructions: Mutex<Vec<InstructionId>>,
        /// Every task's comments, oldest first.
        comments: Mutex<Vec<Comment>>,
        /// How many times a task's comments were read.
        comment_reads: Mutex<usize>,
        /// The comments added, with the task each is on, in order.
        pub comments_added: Mutex<Vec<(TaskId, String)>>,
        /// The tasks whose comments were marked seen, in order.
        pub seen: Mutex<Vec<TaskId>>,
        /// The agents started.
        pub launched: Mutex<Vec<Launch>>,
        /// The panes closed, in order.
        pub closed: Mutex<Vec<PaneId>>,
        /// The links asked for: the pane and its task, or `None` to unlink it, in order.
        pub linked: Mutex<Vec<(PaneId, Option<TaskId>)>>,
        /// The workspaces archived, in order.
        pub archived: Mutex<Vec<WorkspaceId>>,
        /// The workspaces unarchived, in order.
        pub unarchived: Mutex<Vec<WorkspaceId>>,
        /// The mutes and unmutes asked for: the workspace and whether to mute it, in order.
        pub muted: Mutex<Vec<(WorkspaceId, bool)>>,
        /// Each workspace's dirty report; clean when unset.
        dirty: Mutex<HashMap<WorkspaceId, DirtyReport>>,
        /// The workspaces deleted, in order.
        pub deleted: Mutex<Vec<WorkspaceId>>,
        /// The settings; the daemon's defaults when unset.
        settings: Mutex<Option<Settings>>,
        /// How many times the settings were read.
        settings_reads: Mutex<usize>,
        /// The settings patches sent, in order.
        pub settings_patches: Mutex<Vec<SettingsPatch>>,
        /// Each close asked for: the tasks, in order.
        pub closes: Mutex<Vec<Vec<TaskId>>>,
        /// The PRs GitHub refuses to close, with its message.
        refusals: Mutex<HashMap<TaskId, String>>,
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

        /// What it holds now.
        pub fn snapshot_now(&self) -> Snapshot {
            self.snapshot.lock().unwrap().clone()
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

        /// Answers later reads of `workspace`'s dirty report with `report`.
        pub fn set_dirty(&self, workspace: WorkspaceId, report: DirtyReport) {
            self.dirty.lock().unwrap().insert(workspace, report);
        }

        /// Answers later reads with `comments`, every task's.
        pub fn set_comments(&self, comments: Vec<Comment>) {
            *self.comments.lock().unwrap() = comments;
        }

        /// How many times a task's comments were read.
        pub fn comment_reads(&self) -> usize {
            *self.comment_reads.lock().unwrap()
        }

        /// The settings it holds now.
        pub fn settings_now(&self) -> Settings {
            (self.settings.lock().unwrap().clone()).unwrap_or_else(default_settings)
        }

        /// Answers later reads with `settings`, as if another client had changed them.
        pub fn set_settings(&self, settings: Settings) {
            *self.settings.lock().unwrap() = Some(settings);
        }

        /// Makes GitHub refuse to close `task`'s PR with `message`.
        pub fn refuse_close(&self, task: TaskId, message: &str) {
            self.refusals
                .lock()
                .unwrap()
                .insert(task, message.to_owned());
        }

        /// How many times the settings were read.
        pub fn settings_reads(&self) -> usize {
            *self.settings_reads.lock().unwrap()
        }

        /// Stamps or clears `workspace`'s `archived_at`, as the daemon does.
        fn set_archived(&self, workspace: WorkspaceId, archived: bool) {
            let mut snapshot = self.snapshot.lock().unwrap();
            if let Some(w) = snapshot.workspaces.iter_mut().find(|w| w.id == workspace) {
                w.archived_at = archived.then(|| "2026-11-14T00:00:00Z".to_owned());
            }
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

        fn link_task(
            &self,
            pane: PaneId,
            task: Option<TaskId>,
            _cx: &App,
        ) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            if let Some(held) = self.panes.lock().unwrap().iter_mut().find(|p| p.id == pane) {
                held.task_id = task;
            }
            self.linked.lock().unwrap().push((pane, task));
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

        fn new_task(&self, request: new_task::Request, _cx: &App) -> Task<anyhow::Result<TaskId>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let id: TaskId = self.make_id();
            let made = agentdeck_client::types::Task {
                id,
                title: request.title.clone(),
                body: request.body.clone(),
                workspace_id: request.workspace,
                ..crate::overview::fixtures::task(0, agentdeck_core::TaskKind::Plain, None)
            };
            self.snapshot.lock().unwrap().tasks.push(made);
            self.made_tasks.lock().unwrap().push(request);
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

        fn close_prs(
            &self,
            tasks: Vec<TaskId>,
            _cx: &App,
        ) -> Task<anyhow::Result<Vec<PrCloseResult>>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            self.closes.lock().unwrap().push(tasks.clone());
            let refusals = self.refusals.lock().unwrap();
            let mut snapshot = self.snapshot.lock().unwrap();
            let results = (tasks.into_iter())
                .map(|task_id| {
                    if let Some(message) = refusals.get(&task_id) {
                        return PrCloseResult {
                            task_id,
                            ok: false,
                            error: Some(message.clone()),
                        };
                    }
                    // GitHub closed it: the daemon stores it closed and its task done.
                    if let Some(task) = snapshot.tasks.iter_mut().find(|t| t.id == task_id) {
                        task.status = TaskStatus::Done;
                        if let Some(pr) = &mut task.pr {
                            pr.state = agentdeck_core::PrState::Closed;
                        }
                    }
                    PrCloseResult {
                        task_id,
                        ok: true,
                        error: None,
                    }
                })
                .collect();
            Task::ready(Ok(results))
        }

        fn launch_agent(&self, launch: Launch, _cx: &App) -> Task<anyhow::Result<Launched>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let tab: TabId = self.make_id();
            let pane: PaneId = self.make_id();
            let workspace = launch.workspace;
            self.launched.lock().unwrap().push(launch);
            Task::ready(Ok(Launched {
                workspace,
                tab,
                pane,
            }))
        }

        fn send_instruction(
            &self,
            body: String,
            pane: PaneId,
            _cx: &App,
        ) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            self.sent_instructions.lock().unwrap().push((body, pane));
            Task::ready(Ok(()))
        }

        fn new_instruction(
            &self,
            workspace: WorkspaceId,
            body: String,
            _cx: &App,
        ) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            self.instructions_added
                .lock()
                .unwrap()
                .push((workspace, body));
            Task::ready(Ok(()))
        }

        fn edit_instruction(
            &self,
            instruction: InstructionId,
            body: String,
            _cx: &App,
        ) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let mut snapshot = self.snapshot.lock().unwrap();
            if let Some(held) = (snapshot.instructions.iter_mut()).find(|i| i.id == instruction) {
                held.body = body.clone();
            }
            (self.edited_instructions.lock().unwrap()).push((instruction, body));
            Task::ready(Ok(()))
        }

        fn delete_instruction(
            &self,
            instruction: InstructionId,
            _cx: &App,
        ) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            (self.snapshot.lock().unwrap().instructions).retain(|i| i.id != instruction);
            (self.deleted_instructions.lock().unwrap()).push(instruction);
            Task::ready(Ok(()))
        }

        fn comments(&self, task: TaskId, _cx: &App) -> Task<anyhow::Result<Vec<Comment>>> {
            *self.comment_reads.lock().unwrap() += 1;
            let comments = (self.comments.lock().unwrap().iter())
                .filter(|c| c.task_id == task)
                .cloned()
                .collect();
            Task::ready(Ok(comments))
        }

        fn new_comment(&self, task: TaskId, body: String, _cx: &App) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let comment = Comment {
                id: self.make_id(),
                body: body.clone(),
                ..crate::overview::fixtures::comment(0, task)
            };
            self.comments.lock().unwrap().push(comment);
            self.comments_added.lock().unwrap().push((task, body));
            Task::ready(Ok(()))
        }

        fn mark_comments_seen(&self, task: TaskId, _cx: &App) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let mut snapshot = self.snapshot.lock().unwrap();
            if let Some(task) = snapshot.tasks.iter_mut().find(|t| t.id == task) {
                task.unseen_agent_comments = 0;
            }
            self.seen.lock().unwrap().push(task);
            Task::ready(Ok(()))
        }

        fn archive_workspace(&self, workspace: WorkspaceId, _cx: &App) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            self.set_archived(workspace, true);
            self.archived.lock().unwrap().push(workspace);
            Task::ready(Ok(()))
        }

        fn unarchive_workspace(
            &self,
            workspace: WorkspaceId,
            _cx: &App,
        ) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            self.set_archived(workspace, false);
            self.unarchived.lock().unwrap().push(workspace);
            Task::ready(Ok(()))
        }

        fn set_muted(
            &self,
            workspace: WorkspaceId,
            muted: bool,
            _cx: &App,
        ) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let mut snapshot = self.snapshot.lock().unwrap();
            if let Some(w) = snapshot.workspaces.iter_mut().find(|w| w.id == workspace) {
                w.muted = muted;
            }
            self.muted.lock().unwrap().push((workspace, muted));
            Task::ready(Ok(()))
        }

        fn dirty(&self, workspace: WorkspaceId, _cx: &App) -> Task<anyhow::Result<DirtyReport>> {
            let dirty = self.dirty.lock().unwrap().get(&workspace).cloned();
            Task::ready(Ok(dirty.unwrap_or(DirtyReport { repos: Vec::new() })))
        }

        fn delete_workspace(&self, workspace: WorkspaceId, _cx: &App) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let mut snapshot = self.snapshot.lock().unwrap();
            snapshot.workspaces.retain(|w| w.id != workspace);
            for task in &mut snapshot.tasks {
                if task.workspace_id == Some(workspace) {
                    task.workspace_id = None;
                }
            }
            snapshot
                .instructions
                .retain(|i| i.workspace_id != workspace);
            self.deleted.lock().unwrap().push(workspace);
            Task::ready(Ok(()))
        }

        fn settings(&self, _cx: &App) -> Task<anyhow::Result<Settings>> {
            *self.settings_reads.lock().unwrap() += 1;
            Task::ready(Ok(self.settings_now()))
        }

        fn update_settings(
            &self,
            patch: SettingsPatch,
            _cx: &App,
        ) -> Task<anyhow::Result<Settings>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let mut settings = self.settings_now();
            if let Some(theme) = patch.appearance.as_ref().and_then(|a| a.theme) {
                settings.appearance.theme = theme;
            }
            if let Some(days) = patch.pr_sync.as_ref().and_then(|p| p.max_age_days) {
                settings.pr_sync.max_age_days = days;
            }
            if let Some(opener) = patch.terminal.as_ref().and_then(|t| t.file_opener.clone()) {
                settings.terminal.file_opener = opener;
            }
            self.set_settings(settings.clone());
            self.settings_patches.lock().unwrap().push(patch);
            Task::ready(Ok(settings))
        }
    }
}
