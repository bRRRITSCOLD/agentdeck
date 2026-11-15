//! The API's request and response bodies (spec §3, §4). Ids, column enums and the layout
//! tree are `agentdeck-core`'s, so the client shares them.

use agentdeck_core::{
    Agent, AgentState, ChecksState, CommentAuthor, CommentId, InstructionId, Layout, PaneId,
    PaneKind, PrState, ReviewState, TabId, TaskId, TaskKind, TaskPlace, TaskStatus, WorkspaceId,
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
    /// A new title. A PR task's title is GitHub's: only its own title is accepted.
    pub title: Option<String>,
    /// A new body.
    pub body: Option<String>,
    /// A new status.
    pub status: Option<TaskStatus>,
    /// Where to move it: a workspace that isn't archived, or null for the inbox. Absent
    /// leaves it where it is.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub workspace_id: Option<TaskPlace>,
}

/// A field whose value may be null, read as present: `Some(..)` whatever its value, while
/// `#[serde(default)]` gives `None` for a field left out. Serde's own `Option` reads null as
/// `None`, which would make `"workspace_id": null` the same as leaving it out.
fn present<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
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

/// Whether the daemon has a GitHub token GitHub accepts (spec §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GhLogin {
    /// A token GitHub accepted, or one it couldn't be asked about yet.
    LoggedIn,
    /// No token from any source, or GitHub rejected the one found. The GUI shows a banner.
    LoggedOut,
}

/// `GET /v1/prs/status` and `POST /v1/prs/sync`: the GitHub login and the last sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PrSyncStatus {
    /// Whether there's a token GitHub accepts.
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

/// Where the daemon's GitHub token comes from (spec §8), the first that gives one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TokenSource {
    /// A token saved in the system keyring: pasted, or from Sign in with GitHub.
    Keyring,
    /// `GH_TOKEN` or `GITHUB_TOKEN` in the daemon's environment.
    Env,
    /// `gh auth token`.
    Gh,
    /// No source gives one.
    None,
}

/// An environment variable that gives the daemon a GitHub token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TokenEnvVar {
    /// `GH_TOKEN`, read first.
    #[serde(rename = "GH_TOKEN")]
    GhToken,
    /// `GITHUB_TOKEN`.
    #[serde(rename = "GITHUB_TOKEN")]
    GithubToken,
}

impl TokenEnvVar {
    /// The variable's name.
    pub fn name(self) -> &'static str {
        match self {
            TokenEnvVar::GhToken => "GH_TOKEN",
            TokenEnvVar::GithubToken => "GITHUB_TOKEN",
        }
    }
}

/// `GET /v1/github/auth`: where the GitHub token comes from, and whose it is. Never the token
/// or the OAuth client ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GithubAuthInfo {
    /// The source the token comes from.
    pub source: TokenSource,
    /// `GH_TOKEN` or `GITHUB_TOKEN`, whichever is set (`GH_TOKEN` when both are), also while
    /// a saved token overrides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_var: Option<TokenEnvVar>,
    /// The token's GitHub user; absent while there's no token, GitHub rejected it, or it
    /// couldn't be asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    /// The token's scopes, from `X-OAuth-Scopes`; absent for a token that sends none (a
    /// fine-grained one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    /// Whether the keyring or `AGENTDECK_GITHUB_CLIENT_ID` gives an OAuth client ID, so
    /// Sign in with GitHub can start.
    pub client_id_set: bool,
}

/// `PUT /v1/github/token`.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct GithubTokenBody {
    /// A GitHub token: a classic one, or a fine-grained one.
    pub token: String,
}

impl std::fmt::Debug for GithubTokenBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubTokenBody").finish_non_exhaustive()
    }
}

/// `PUT /v1/github/client-id`.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct GithubClientIdBody {
    /// The client ID of the user's GitHub OAuth App, with Device Flow on.
    pub client_id: String,
}

impl std::fmt::Debug for GithubClientIdBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubClientIdBody").finish_non_exhaustive()
    }
}

/// `POST /v1/github/device`: the code the user enters on GitHub to sign in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceSignIn {
    /// The code to enter, `ABCD-1234`.
    pub user_code: String,
    /// Where to enter it.
    pub verification_uri: String,
    /// When the code expires (RFC 3339).
    pub expires_at: String,
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

/// `POST /v1/prs/close`: the PR tasks whose PRs to close on GitHub.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PrClose {
    /// The tasks, each a PR task whose PR is open; the results come back in this order.
    pub task_ids: Vec<TaskId>,
}

/// `POST /v1/prs/close`'s answer: one result per task asked for, in the order asked.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PrCloseResults {
    /// Each task's result.
    pub results: Vec<PrCloseResult>,
}

/// Whether one PR closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PrCloseResult {
    /// The PR task.
    pub task_id: TaskId,
    /// Whether GitHub closed the PR.
    pub ok: bool,
    /// Why it didn't: GitHub's message, why the request failed, "Not an open PR" or "Not
    /// signed in to GitHub". `None` when it closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl PrCloseResult {
    /// The result for a PR GitHub closed.
    pub fn closed(task_id: TaskId) -> PrCloseResult {
        PrCloseResult {
            task_id,
            ok: true,
            error: None,
        }
    }

    /// The result for a PR that didn't close, and why.
    pub fn failed(task_id: TaskId, error: impl Into<String>) -> PrCloseResult {
        PrCloseResult {
            task_id,
            ok: false,
            error: Some(error.into()),
        }
    }
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

/// `PATCH /v1/panes/{id}`: the task to link the pane to, or null to unlink it. `task_id` is
/// required, so an empty body can't unlink a pane by accident.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PanePatch {
    /// A task in the pane's workspace that isn't done, or null.
    #[serde(deserialize_with = "Option::deserialize")]
    #[schemars(schema_with = "nullable_task_id")]
    pub task_id: Option<TaskId>,
}

/// A task id or null, as a required property: schemars leaves an `Option` field out of
/// `required` on its own.
fn nullable_task_id(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
    generator.subschema_for::<Option<TaskId>>()
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
    /// Typed into the terminal as is, unless `submit`.
    pub text: String,
    /// `now` when absent.
    #[serde(default)]
    pub when: InputWhen,
    /// Submits `text` as a message: types it as one bracketed paste when the program turned
    /// bracketed paste on (else as is), then presses Enter in a write of its own, after a
    /// pause. Claude Code and Codex read text arriving with its Enter as a paste and keep the
    /// Enter as a newline. False when absent.
    #[serde(default)]
    pub submit: bool,
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
