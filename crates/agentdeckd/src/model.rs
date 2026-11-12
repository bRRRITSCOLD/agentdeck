//! The API's request and response bodies (spec §3, §4). Ids, column enums and the layout
//! tree are `agentdeck-core`'s, so the client shares them.

use agentdeck_core::{
    Agent, AgentState, ChecksState, Layout, NoteId, PaneId, PaneKind, PrState, ReviewState, TabId,
    TaskId, TaskKind, TaskStatus, WorkspaceId,
};
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
    /// The pull request behind a PR task; `None` for a plain task.
    pub pr: Option<PullRequest>,
}

/// The GitHub pull request behind a PR task, as PR sync last saw it (spec §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequest {
    /// `owner/name` of the repo the PR targets.
    pub repo: String,
    /// The PR's number in that repo.
    pub number: i64,
    /// The PR's web URL.
    pub url: String,
    /// Open, merged or closed.
    pub state: PrState,
    /// A draft PR.
    pub draft: bool,
    /// The branch the PR merges from.
    pub head_ref: String,
    /// The head commit's rolled-up checks; `None` when it has none.
    pub checks: Option<ChecksState>,
    /// The review decision; `None` when the repo requires no review.
    pub review: Option<ReviewState>,
    /// When the PR last changed on GitHub.
    pub updated_at: String,
    /// When PR sync last saw the PR.
    pub synced_at: String,
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

/// Whether `gh auth token` gives the daemon a GitHub token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GhLogin {
    /// `gh` printed a token GitHub accepted, or one not yet tried.
    LoggedIn,
    /// No `gh`, `gh` not logged in, or GitHub rejected its token. The GUI shows a banner.
    LoggedOut,
}

/// `GET /v1/prs/status` and `POST /v1/prs/sync`: the `gh` login and the last sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PrSyncStatus {
    /// The `gh` login state.
    pub login: GhLogin,
    /// When the last successful sync finished; `None` until one has.
    pub last_synced_at: Option<String>,
    /// Why the most recent sync failed; `None` when it succeeded or none has run.
    pub last_error: Option<String>,
}

/// A terminal in a tab, running an agent CLI or a shell.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Pane {
    /// The pane's id.
    pub id: PaneId,
    /// The tab it belongs to.
    pub tab_id: TabId,
    /// An agent or a shell.
    pub kind: PaneKind,
    /// Which agent, for agent panes.
    pub agent: Option<Agent>,
    /// The command as first launched, launch prompt included. A resumed pane runs its
    /// agent's resume command, or a fresh shell, instead.
    pub argv: Vec<String>,
    /// The working directory it was launched in.
    pub cwd: String,
    /// The agent's session id: chosen at launch for `claude`, found after launch for `codex`
    /// (null until Codex has written its session file).
    pub session_id: Option<String>,
    /// The linked task, if any.
    pub task_id: Option<TaskId>,
    /// The agent state; a shell is `working` while it prints and `unknown` otherwise.
    pub state: AgentState,
    /// The title the program set, or empty.
    pub title: String,
    /// The exit code, once the process has exited.
    pub exit_code: Option<i64>,
    /// When the pane was created.
    pub created_at: String,
}

/// `POST /v1/panes`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewPane {
    /// The tab to add it to. The caller places it in the tab's layout.
    pub tab_id: TabId,
    /// An agent or a shell.
    pub kind: PaneKind,
    /// Which agent: required for agent panes, absent for shells.
    pub agent: Option<Agent>,
    /// The command. A shell pane without one runs `$SHELL`, else `/bin/sh`; an `other` agent
    /// pane needs one; `claude` and `codex` panes take none, since agentdeck builds theirs.
    pub argv: Option<Vec<String>>,
    /// The absolute working directory, which must exist.
    pub cwd: String,
    /// A task to link. A `claude` or `codex` pane's launch prompt carries its title and body.
    pub task_id: Option<TaskId>,
}

/// When `POST /v1/panes/{id}/input` reaches the terminal.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InputWhen {
    /// At once.
    #[default]
    Now,
    /// Once the pane is idle, done or blocked; at once if it already is.
    Idle,
}

/// `POST /v1/panes/{id}/input`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PaneInput {
    /// Typed into the terminal as is; end it with `\r` to press Enter.
    pub text: String,
    /// `now` when absent.
    #[serde(default)]
    pub when: InputWhen,
}

/// What became of `POST /v1/panes/{id}/input`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct InputAccepted {
    /// True when it waits for the pane to be idle; false when it was typed.
    pub queued: bool,
}

/// `GET /v1/panes/{id}/tail` query.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct TailQuery {
    /// At most this many of the last bytes; all that is kept (256 KiB) when absent.
    pub bytes: Option<u64>,
}
