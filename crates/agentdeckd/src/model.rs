//! The API's request and response bodies (spec §3, §4). Ids, column enums and the layout
//! tree are `agentdeck-core`'s, so the client shares them.

use agentdeck_core::{Layout, NoteId, PaneId, TabId, TaskId, TaskKind, TaskStatus, WorkspaceId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `GET /v1/health`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Health {
    /// The daemon's version.
    pub version: String,
    /// Seconds since the daemon started.
    pub uptime_secs: u64,
}

/// A free-form unit of work with its own directory.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Workspace {
    /// The workspace's id.
    pub id: WorkspaceId,
    /// Unique, derived from the name at creation; names a scratch dir.
    pub slug: String,
    /// The display name.
    pub name: String,
    /// The absolute path of the workspace's directory.
    pub dir: String,
    /// True when agentdeck created `dir` as a scratch dir, so deleting the workspace removes it.
    pub dir_owned: bool,
    /// Notifications are muted.
    pub muted: bool,
    /// When the workspace was archived; archived workspaces keep their data.
    pub archived_at: Option<String>,
    /// When the workspace was created.
    pub created_at: String,
}

/// `POST /v1/workspaces`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewWorkspace {
    /// The display name.
    pub name: String,
    /// An existing absolute directory to attach. Without it, agentdeck creates a scratch dir.
    pub dir: Option<String>,
}

/// `PATCH /v1/workspaces/{id}`: the fields to change.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct WorkspacePatch {
    /// A new display name. The slug and the dir stay as they are.
    pub name: Option<String>,
    /// Mute or unmute notifications.
    pub muted: Option<bool>,
}

/// The git repos under a workspace's dir that hold work not on any remote.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DirtyReport {
    /// Only the dirty repos; an empty list means deleting loses nothing git knows about.
    pub repos: Vec<DirtyRepo>,
}

/// One dirty git repo.
#[derive(Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DirtyRepo {
    /// The repo's absolute path.
    pub path: String,
    /// Changed, staged and untracked files, as `git status --porcelain` prints them.
    pub uncommitted: Vec<String>,
    /// Local branches with commits that are on no remote.
    pub unpushed_branches: Vec<String>,
}

/// A tab in a workspace, holding a split tree of panes.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Tab {
    /// The tab's id.
    pub id: TabId,
    /// The workspace it belongs to.
    pub workspace_id: WorkspaceId,
    /// The tab's title.
    pub title: String,
    /// Sort key among the workspace's tabs.
    pub position: i64,
    /// The split tree; `None` until the tab has a pane.
    pub layout: Option<Layout>,
}

/// `POST /v1/workspaces/{id}/tabs`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewTab {
    /// The tab's title.
    pub title: String,
    /// The initial split tree, if any.
    pub layout: Option<Layout>,
}

/// `PATCH /v1/tabs/{id}`: the fields to change.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct TabPatch {
    /// A new title.
    pub title: Option<String>,
    /// A new sort key.
    pub position: Option<i64>,
    /// A new split tree.
    pub layout: Option<Layout>,
}

/// An item in the one global task list.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Task {
    /// The task's id.
    pub id: TaskId,
    /// The title.
    pub title: String,
    /// The body, possibly empty.
    pub body: String,
    /// The status.
    pub status: TaskStatus,
    /// Plain or PR.
    pub kind: TaskKind,
    /// The linked workspace; `None` puts the task in the inbox.
    pub workspace_id: Option<WorkspaceId>,
    /// When the task was created.
    pub created_at: String,
    /// When the task last changed.
    pub updated_at: String,
    /// When the task last moved to done; `None` while it isn't done.
    pub done_at: Option<String>,
}

/// `POST /v1/tasks`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewTask {
    /// The title.
    pub title: String,
    /// The body; empty when absent.
    pub body: Option<String>,
    /// The status; `todo` when absent.
    pub status: Option<TaskStatus>,
    /// The workspace to link; the inbox when absent.
    pub workspace_id: Option<WorkspaceId>,
}

/// `PATCH /v1/tasks/{id}`: the fields to change.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct TaskPatch {
    /// A new title.
    pub title: Option<String>,
    /// A new body.
    pub body: Option<String>,
    /// A new status.
    pub status: Option<TaskStatus>,
    /// A workspace to link.
    pub workspace_id: Option<WorkspaceId>,
}

/// `GET /v1/tasks` filters; they combine with AND.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct TaskFilter {
    /// Only tasks with this status.
    pub status: Option<TaskStatus>,
    /// Only tasks linked to this workspace.
    pub workspace: Option<WorkspaceId>,
    /// Only tasks of this kind.
    pub kind: Option<TaskKind>,
    /// `true`: only tasks with no workspace; `false`: only tasks with one.
    pub inbox: Option<bool>,
}

/// A message left on a workspace or a task for agents.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Note {
    /// The note's id.
    pub id: NoteId,
    /// The message.
    pub body: String,
    /// The workspace it's on; exactly one of this and `task_id` is set.
    pub workspace_id: Option<WorkspaceId>,
    /// The task it's on; exactly one of this and `workspace_id` is set.
    pub task_id: Option<TaskId>,
    /// When the note was created.
    pub created_at: String,
    /// When the note was delivered or marked read.
    pub read_at: Option<String>,
    /// The pane it was delivered to, if a pane read it.
    pub read_by_pane: Option<PaneId>,
}

/// `POST /v1/notes`: exactly one of `workspace_id` and `task_id`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewNote {
    /// The message.
    pub body: String,
    /// The workspace to leave it on.
    pub workspace_id: Option<WorkspaceId>,
    /// The task to leave it on.
    pub task_id: Option<TaskId>,
}

/// `PATCH /v1/notes/{id}`: the fields to change.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct NotePatch {
    /// A new message.
    pub body: Option<String>,
    /// `true` marks the note read now, `false` marks it unread.
    pub read: Option<bool>,
    /// The pane that read it; recorded only with `read: true`.
    pub read_by_pane: Option<PaneId>,
}

/// `GET /v1/notes` filters; they combine with AND.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct NoteFilter {
    /// Only notes on this workspace.
    pub workspace: Option<WorkspaceId>,
    /// Only notes on this task.
    pub task: Option<TaskId>,
    /// `true`: only unread notes; `false`: only read ones.
    pub unread: Option<bool>,
}
