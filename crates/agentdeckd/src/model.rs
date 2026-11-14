//! The API's request and response bodies (spec §3, §4). Ids, column enums and the layout
//! tree are `agentdeck-core`'s, so the client shares them.

use agentdeck_core::{
    Agent, AgentState, ChecksState, CommentAuthor, CommentId, InstructionId, Layout, PaneId,
    PaneKind, PrState, ReviewState, TabId, TaskId, TaskKind, TaskStatus, WorkspaceId,
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
    /// Local branches with commits that are on no remote, by name.
    pub unpushed_branches: Vec<UnpushedBranch>,
}

/// A local branch holding commits that are on no remote.
#[derive(Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct UnpushedBranch {
    /// The branch's short name, e.g. `feat/tabs`.
    pub name: String,
    /// How many of its commits are on no remote branch, whether or not it has an upstream.
    pub commits: u32,
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
    /// Agents' comments on the task the user hasn't seen yet.
    pub unseen_agent_comments: u32,
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

/// A comment in a task's thread (spec §9). The user's comments are delivered to the task's
/// agent; an agent's are a record and never delivered.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Comment {
    /// The comment's id.
    pub id: CommentId,
    /// The task it's on.
    pub task_id: TaskId,
    /// The text.
    pub body: String,
    /// Who wrote it: the user, or an agent from its pane.
    pub author: CommentAuthor,
    /// The agent that wrote it, when that is `claude` or `codex`.
    pub author_agent: Option<Agent>,
    /// The pane it was written from; `None` for the user's, and once the pane is gone.
    pub author_pane: Option<PaneId>,
    /// When it was written.
    pub created_at: String,
    /// When it was delivered to an agent; always `None` for an agent's comment.
    pub delivered_at: Option<String>,
    /// The pane it was delivered to.
    pub delivered_to_pane: Option<PaneId>,
    /// When the user saw it; `None` while an agent's comment is new to the user. The user's
    /// own comments are seen when written.
    pub seen_at: Option<String>,
}

/// `POST /v1/tasks/{id}/comments`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewComment {
    /// The text.
    pub body: String,
    /// The pane writing it: the comment is then that pane's agent's. Without it, the user's.
    pub pane: Option<PaneId>,
}

/// Standing context for every agent in a workspace (spec §9): it goes into every launch
/// prompt there and stays until deleted.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Instruction {
    /// The instruction's id.
    pub id: InstructionId,
    /// The workspace it's for.
    pub workspace_id: WorkspaceId,
    /// The text.
    pub body: String,
    /// When it was added.
    pub created_at: String,
    /// When its text last changed.
    pub updated_at: String,
}

/// `POST /v1/workspaces/{id}/instructions`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewInstruction {
    /// The text.
    pub body: String,
}

/// `PATCH /v1/instructions/{id}`: the fields to change.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct InstructionPatch {
    /// A new text.
    pub body: Option<String>,
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
    /// How far the sync running now is; `None` while none runs.
    pub progress: Option<PrSyncProgress>,
    /// How many days since GitHub last updated an open PR it may be and still sync: the
    /// setting `pr_sync.max_age_days` as it is now; `0` for no cutoff.
    pub max_age_days: u32,
}

/// How far a running PR sync is through the viewer's PRs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PrSyncProgress {
    /// PRs read and stored so far.
    pub prs_done: u32,
    /// PRs this sync expects, from the search's count; `0` until the first page answers.
    pub prs: u32,
    /// What the sync is doing now.
    pub stage: PrSyncStage,
}

/// What a running PR sync is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrSyncStage {
    /// Reading the list or the search, page by page; `prs_done` counts.
    Listing,
    /// Fetching reviews and checks by number, after the last page.
    Details,
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

/// `GET /v1/settings`, and what `PATCH /v1/settings` answers: every setting with the value
/// the daemon uses (spec §2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Settings {
    /// `[appearance]`.
    pub appearance: AppearanceSettings,
    /// `[pr_sync]`.
    pub pr_sync: PrSyncSettings,
    /// `[terminal]`.
    pub terminal: TerminalSettings,
    /// The dotted keys (`pr_sync.max_age_days`) whose value comes from an environment
    /// variable, not `config.toml`. A PATCH of one is written to the file, but the variable
    /// still wins until the daemon runs without it.
    pub overridden_by_env: Vec<String>,
}

/// `[appearance]`: how the GUI looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AppearanceSettings {
    /// `appearance.theme`: light, dark, or the desktop's choice.
    pub theme: ThemePreference,
}

/// `[pr_sync]`: which open PRs PR sync keeps (spec §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PrSyncSettings {
    /// `pr_sync.max_age_days`: only open PRs GitHub updated in the last this many days sync;
    /// `0` for every open PR.
    pub max_age_days: u32,
}

/// `[terminal]`: the GUI's terminal panes (spec §11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalSettings {
    /// `terminal.file_opener`: the command a `secondary`-click on a file path in a pane runs,
    /// split into words as a shell would and never run through a shell. `{path}` is the
    /// file's absolute path, `{line}` and `{col}` where the path names them (else `1`).
    pub file_opener: String,
}

/// `appearance.theme`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    /// Follow the desktop's light or dark setting.
    #[default]
    System,
    /// Always dark.
    Dark,
    /// Always light.
    Light,
}

/// `PATCH /v1/settings`: the settings to change, by section; the rest stay as they are.
///
/// The schema is typed, but each section is read as plain JSON so that a value or key the
/// daemon doesn't take is a 400 `invalid_setting` naming its dotted key, not a parse error.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SettingsPatch {
    /// `[appearance]` keys to change.
    #[schemars(with = "Option<AppearancePatch>")]
    pub appearance: Option<serde_json::Value>,
    /// `[pr_sync]` keys to change.
    #[schemars(with = "Option<PrSyncPatch>")]
    pub pr_sync: Option<serde_json::Value>,
    /// `[terminal]` keys to change.
    #[schemars(with = "Option<TerminalPatch>")]
    pub terminal: Option<serde_json::Value>,
    /// Sections no setting is in, so the PATCH can name them in its error.
    #[serde(flatten)]
    #[schemars(skip)]
    pub unknown: serde_json::Map<String, serde_json::Value>,
}

/// `[appearance]` in a [`SettingsPatch`]: its schema only, since `config::validate` reads the
/// JSON itself.
#[derive(JsonSchema)]
#[expect(dead_code, reason = "a schema, never built")]
pub struct AppearancePatch {
    /// A new `appearance.theme`.
    pub theme: Option<ThemePreference>,
}

/// `[pr_sync]` in a [`SettingsPatch`]: its schema only, since `config::validate` reads the
/// JSON itself.
#[derive(JsonSchema)]
#[expect(dead_code, reason = "a schema, never built")]
pub struct PrSyncPatch {
    /// A new `pr_sync.max_age_days`; `0` for no cutoff.
    pub max_age_days: Option<u32>,
}

/// `[terminal]` in a [`SettingsPatch`]: its schema only, since `config::validate` reads the
/// JSON itself.
#[derive(JsonSchema)]
#[expect(dead_code, reason = "a schema, never built")]
pub struct TerminalPatch {
    /// A new `terminal.file_opener`: a command holding `{path}` in an argument.
    pub file_opener: Option<String>,
}
