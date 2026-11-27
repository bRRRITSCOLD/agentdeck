//! The API's request and response bodies (spec §3, §4). Ids, column enums and the layout
//! tree are `agentdeck-core`'s, so the client shares them.

use agentdeck_core::{
    Agent, AgentState, ChecksState, Layout, PaneId, PaneKind, PrRef, PrState, RepositoryId,
    RepositoryName, ReviewState, TabId, WorkspaceId, Yours,
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

/// A long-lived area of work: a name, the Repositories it links and its Panes. It has no
/// folder: its Panes work wherever they go.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Workspace {
    /// The workspace's id.
    pub id: WorkspaceId,
    /// Unique, derived from the name at creation; the CLI finds a workspace by it.
    pub slug: String,
    /// The display name.
    pub name: String,
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
    /// Repositories to link the new Workspace to, each its `owner/name` or GitHub URL, as
    /// `POST /v1/workspaces/{id}/links` takes one. None when absent.
    #[serde(default)]
    pub link: Vec<RepositoryName>,
}

/// `PATCH /v1/workspaces/{id}`: the fields to change, applied together or not at all.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct WorkspacePatch {
    /// A new display name. The slug stays as it is.
    pub name: Option<String>,
    /// Mute or unmute notifications.
    pub muted: Option<bool>,
    /// Repositories to link, each its `owner/name` or GitHub URL, as
    /// `POST /v1/workspaces/{id}/links` takes one.
    #[serde(default)]
    pub link: Vec<RepositoryName>,
    /// Repositories to unlink, by id, as `DELETE /v1/workspaces/{id}/links/{repository}`
    /// names one. One the Workspace doesn't link is left as it is.
    #[serde(default)]
    pub unlink: Vec<RepositoryId>,
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

/// A GitHub Repository a pull request belongs to (spec §3): what GitHub says of it, as PR sync
/// last read it with one of its pull requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Repository {
    /// GitHub's id for it, which a rename or a transfer keeps.
    pub id: RepositoryId,
    /// `owner/name`, as GitHub calls it now.
    pub name: RepositoryName,
    /// Its GitHub page.
    pub url: String,
    /// Only those GitHub lets in can see it.
    pub private: bool,
    /// Archived on GitHub: read-only.
    pub archived: bool,
}

/// `GET /v1/github/repositories`: every GitHub repository the sign-in can reach, as the Link
/// picker offers them (spec §8), with how far loading them has got. Kept in memory for the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GithubRepositoryList {
    /// Where loading the list stands.
    pub status: GithubListStatus,
    /// How many repositories have loaded: `repositories`' length.
    pub loaded: u32,
    /// About how many there are, from the last page's number in page 1's `Link` header, then
    /// exact once the last page has loaded; `None` until page 1 answers.
    pub total: Option<u32>,
    /// Why the last load failed, while `status` is `failed`: GitHub's message, or why the
    /// request failed.
    pub error: Option<String>,
    /// The repositories loaded so far, by `owner/name` as GitHub sorts them. While a list is
    /// refreshed, the one before it, whole.
    pub repositories: Vec<Repository>,
}

/// Where loading the GitHub repository list stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GithubListStatus {
    /// Nothing asked for it yet this run, or since the sign-in changed.
    NotLoaded,
    /// The first load runs: `repositories` holds the pages that have arrived.
    Loading,
    /// Every page loaded. A refresh runs quietly under it, the list changing once it ends.
    Loaded,
    /// The load failed (no connection, a rate limit, a token GitHub rejects): `error` says why.
    Failed,
    /// There's no GitHub token to list with.
    SignedOut,
}

/// A pull request PR sync keeps (spec §3, §8), as it last saw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequest {
    /// `owner/name#123`, by its Repository's name now.
    pub reference: PrRef,
    /// Its number in its Repository.
    pub number: u32,
    /// The Repository it belongs to.
    pub repository: Repository,
    /// Its title.
    pub title: String,
    /// Its GitHub page.
    pub url: String,
    /// Open, merged or closed.
    pub state: PrState,
    /// A draft.
    pub draft: bool,
    /// The branch it merges from.
    pub head_ref: String,
    /// The branch it merges into.
    pub base_ref: String,
    /// The GitHub login of who opened it; `None` when that account is gone.
    pub author: Option<String>,
    /// Why it is yours, in this order: `authored`, `review_requested` (a review request names
    /// you), `assigned`; empty once it is none of these, when it is Finished (spec §8). Absent
    /// from an older daemon, which read only the PRs you authored.
    #[serde(default)]
    pub yours: Vec<Yours>,
    /// The head commit's rolled-up checks; `None` when it has none.
    pub checks: Option<ChecksState>,
    /// The review decision; `None` when the Repository requires no review.
    pub review: Option<ReviewState>,
    /// When it last changed on GitHub.
    pub updated_at: String,
    /// Whether the GitHub sign-in can't see it: it shows as last synced (spec §8).
    pub unreachable: bool,
    /// Whether it is Finished: merged, closed, or open but not updated within the age cutoff.
    /// PR sync deletes it 7 days after it became Finished (spec §8). Absent from an older
    /// daemon, which reads as false.
    #[serde(default)]
    pub finished: bool,
}

/// `GET /v1/prs` filters; given together, a PR must pass both.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct PrFilter {
    /// Only the pull requests of the Repository GitHub calls this now, `owner/name`, ignoring
    /// case.
    pub repository: Option<RepositoryName>,
    /// Only the pull requests of the Repositories this Workspace links.
    pub workspace: Option<WorkspaceId>,
    /// Include Finished pull requests; without it they are left out.
    pub finished: Option<bool>,
}

/// A Repository and the Workspaces that link it (spec §3): what GitHub last said of it, and
/// whether the GitHub sign-in can still see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepositoryLinks {
    /// GitHub's id for it, which a rename or a transfer keeps.
    pub id: RepositoryId,
    /// `owner/name`, as GitHub calls it now.
    pub name: RepositoryName,
    /// Its GitHub page.
    pub url: String,
    /// Only those GitHub lets in can see it.
    pub private: bool,
    /// Archived on GitHub: read-only.
    pub archived: bool,
    /// Whether the GitHub sign-in can no longer see it: it shows as last read (spec §8).
    pub unreachable: bool,
    /// The Workspaces that link it, archived ones included, oldest first; empty when none
    /// does and it is kept for its pull requests.
    pub workspaces: Vec<WorkspaceId>,
}

/// `GET /v1/repositories` filters.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct RepositoryFilter {
    /// Only the Repositories this Workspace links.
    pub workspace: Option<WorkspaceId>,
}

/// `POST /v1/workspaces/{id}/links`: the Repository to link, by name, or by GitHub's id with
/// what the client holds of it.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewLink {
    /// The Repository's `owner/name` (or its GitHub URL). Without `id`, one agentdeck doesn't
    /// know by that name is looked up on GitHub first.
    pub repository: RepositoryName,
    /// GitHub's id for it, as the client last read it (Undo after an unlink sends it). The
    /// Repository agentdeck keeps with this id is linked, whatever its name now; one it no
    /// longer keeps is stored again from `repository`, `url`, `private` and `archived`,
    /// without asking GitHub, so this works signed out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<RepositoryId>,
    /// Its GitHub page, with `id`; `https://github.com/<repository>` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Whether it is private, with `id`; false when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private: Option<bool>,
    /// Whether it is archived on GitHub, with `id`; false when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
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
    /// The GitHub login the last good sync ran as, kept across sign-outs and restarts; absent
    /// until a sync has run as a login GitHub named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_as: Option<String>,
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
    /// Whether GitHub rejected the token `source` gives (revoked or expired). A token GitHub
    /// couldn't be asked about has no `login` either, but isn't rejected.
    #[serde(default)]
    pub rejected: bool,
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

/// `POST /v1/prs/close`: the pull requests to close on GitHub.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PrClose {
    /// Each pull request's reference, `owner/name#123`; each must be stored open and reachable.
    /// The results come back in this order.
    pub prs: Vec<PrRef>,
}

/// `POST /v1/prs/close`'s answer: one result per reference asked for, in the order asked.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PrCloseResults {
    /// Each reference's result.
    pub results: Vec<PrCloseResult>,
}

/// Whether one PR closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PrCloseResult {
    /// The pull request, as it was asked for.
    pub pr: PrRef,
    /// Whether GitHub closed the PR.
    pub ok: bool,
    /// Why it didn't: GitHub's message, why the request failed, "Not an open PR" or "Not
    /// signed in to GitHub". `None` when it closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl PrCloseResult {
    /// The result for a PR GitHub closed.
    pub fn closed(pr: PrRef) -> PrCloseResult {
        PrCloseResult {
            pr,
            ok: true,
            error: None,
        }
    }

    /// The result for a PR that didn't close, and why.
    pub fn failed(pr: PrRef, error: impl Into<String>) -> PrCloseResult {
        PrCloseResult {
            pr,
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
    /// The agent a shell pane hosts: `claude` or `codex` while the user's own is its
    /// foreground process, else null. Its kind stays `shell`; it runs that agent's state rules
    /// meanwhile (spec §3, §7).
    pub hosted_agent: Option<Agent>,
    /// The agent a shell pane hosted last, once it has left and the shell is back: which one,
    /// and its session when the daemon found it. Null while the pane hosts an agent, before it
    /// hosted any, once its shell exits and after a daemon restart (spec §3, §7).
    pub last_hosted: Option<LastHosted>,
    /// The command as first launched. A resumed pane runs its
    /// agent's resume command, or a fresh shell, instead.
    pub argv: Vec<String>,
    /// The working directory its process was launched in: by `POST /v1/panes`, or by its last
    /// `POST /v1/panes/{id}/respawn`. A respawn resumes its agent's session here, since the
    /// session started here (spec §4).
    pub cwd: String,
    /// Its current folder (spec §6): the folder its shell last reported with OSC 7, else its
    /// process's working directory, else `cwd`, the folder it launched in, as the daemon last
    /// read them. Kept after it exits; a pane resumed after a restart starts there, and so do
    /// Open shell and a fresh Restart agent (spec §4).
    pub folder: String,
    /// The agent's session id: chosen at launch for `claude`, found after launch for `codex`
    /// (null until Codex has written its session file).
    pub session_id: Option<String>,
    /// The agent state; a shell is `working` while it prints and `unknown` otherwise.
    pub state: AgentState,
    /// The title the program set, or empty.
    pub title: String,
    /// The exit code, once the process has exited.
    pub exit_code: Option<i64>,
    /// When the pane was created.
    pub created_at: String,
}

/// The agent a shell pane hosted last, once it has left (spec §7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LastHosted {
    /// The agent: `claude` or `codex`.
    pub agent: Agent,
    /// The session it ran, when found: the one its command line named (`--resume <id>`,
    /// `--session-id <id>`, `codex resume <id>`), else the newest session file it wrote for its
    /// folder while hosted. Always a UUID; null when none was found.
    pub session_id: Option<String>,
    /// When it left, RFC 3339: each time an agent leaves is told apart by it.
    pub left_at: String,
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
}

/// What `POST /v1/panes/{id}/respawn` runs in an exited pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaneRun {
    /// The pane's agent again: claude and codex resume their session when it is known, else
    /// start fresh on a new session; an `other` agent runs its argv again. Agent panes only.
    Agent,
    /// A shell, which turns the pane into a shell pane.
    Shell,
}

/// `POST /v1/panes/{id}/respawn`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PaneRespawn {
    /// What to run.
    pub run: PaneRun,
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
    /// `[gui]`.
    pub gui: GuiSettings,
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

/// `[gui]`: how the GUI behaves (spec §11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GuiSettings {
    /// `gui.confirm_quit`: whether closing the window or Quit asks first while an agent runs,
    /// since agents keep running after the GUI quits.
    pub confirm_quit: bool,
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
    /// `[gui]` keys to change.
    #[schemars(with = "Option<GuiPatch>")]
    pub gui: Option<serde_json::Value>,
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

/// `[gui]` in a [`SettingsPatch`]: its schema only, since `config::validate` reads the JSON
/// itself.
#[derive(JsonSchema)]
#[expect(dead_code, reason = "a schema, never built")]
pub struct GuiPatch {
    /// A new `gui.confirm_quit`.
    pub confirm_quit: Option<bool>,
}
