//! What the GUI reads from and asks of the daemon over REST (spec §4), behind [`Api`] so a
//! view takes a fake in tests. The real one is the connected [`Daemon`]. A failed call reads
//! as the daemon's own message, through [`ApiError`].

use agentdeck_client::daemon::Daemon;
use agentdeck_client::panes::{self, pane_if_any};
use agentdeck_client::types::{
    DeviceSignIn, GithubAuthInfo, GithubClientIdBody, GithubRepositoryList, GithubTokenBody,
    InputWhen, Pane, PaneInput, PaneRun, PrClose, PrCloseResult, Repository, Settings,
    SettingsPatch, Tab,
};
pub use agentdeck_client::workspaces::Change;
use agentdeck_client::{ApiError, Client};
use agentdeck_core::{
    PaneId, PrRef, RepositoryId, RepositoryName, SplitDirection, TabId, WorkspaceId,
};
use gpui::{App, Task};

use crate::new_agent::{Launch, Launched};
use crate::new_workspace::Request;
use crate::overview::{PaneState, Snapshot};
use crate::runtime;

/// The daemon's API, as the GUI uses it.
pub trait Api: Send + Sync {
    /// Everything the sidebar shows, read fresh.
    fn snapshot(&self, cx: &App) -> Task<anyhow::Result<Snapshot>>;

    /// `workspace`'s tabs and the panes in their layouts.
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

    /// Starts exited `pane` again in place, running `run`: its agent again, or a shell it
    /// becomes (spec §4). A refusal is an [`ApiError`], such as `running`.
    fn respawn_pane(&self, pane: PaneId, run: PaneRun, cx: &App) -> Task<anyhow::Result<()>>;

    /// Stops `pane`'s agent and keeps the pane (spec §4): an agent pane's process, which then
    /// reads as exited, or the agent a shell pane hosts, leaving the shell running.
    fn stop_pane(&self, pane: PaneId, cx: &App) -> Task<anyhow::Result<()>>;

    /// Types `text` into `pane` now and presses Enter, as a submitted message (spec §4
    /// `POST /v1/panes/{id}/input` with `submit`). A refusal is an [`ApiError`], such as
    /// `exited`.
    fn submit_input(&self, pane: PaneId, text: String, cx: &App) -> Task<anyhow::Result<()>>;

    /// Makes a workspace as `request` says; its id. A refusal is an [`ApiError`];
    /// `invalid_name` means the name is blank.
    fn new_workspace(&self, request: Request, cx: &App) -> Task<anyhow::Result<WorkspaceId>>;

    /// Syncs PRs now, returning once the sync is done: a full sync when `full` (Sync now asks
    /// for one, spec §8), else the sync that is due.
    fn sync_prs(&self, full: bool, cx: &App) -> Task<anyhow::Result<()>>;

    /// Closes the pull requests `prs` names on GitHub (spec §8): one result per reference, in
    /// order, each saying whether it closed and why not. Only a request that fails as a whole
    /// is an error.
    fn close_prs(&self, prs: Vec<PrRef>, cx: &App) -> Task<anyhow::Result<Vec<PrCloseResult>>>;

    /// Starts an agent as `launch` says and places it in a tab (spec §6): the workspace's
    /// first empty tab, or a new one. A refusal is an [`ApiError`]; `invalid_argv` means the
    /// agent CLI couldn't start, and its message says why.
    fn launch_agent(&self, launch: Launch, cx: &App) -> Task<anyhow::Result<Launched>>;

    /// Archives `workspace`, keeping its panes and Links. A refusal is an [`ApiError`];
    /// `panes_running` means a pane of it still runs.
    fn archive_workspace(&self, workspace: WorkspaceId, cx: &App) -> Task<anyhow::Result<()>>;

    /// Brings the archived `workspace` back.
    fn unarchive_workspace(&self, workspace: WorkspaceId, cx: &App) -> Task<anyhow::Result<()>>;

    /// Applies Edit workspace's `change` to `workspace` in one request: a new name (its slug
    /// kept) and the Repositories to link and unlink, all or nothing. A refusal is an
    /// [`ApiError`]: `invalid_name` means the name is blank, `unknown_repository` that GitHub
    /// can't see a Repository to link.
    fn edit_workspace(
        &self,
        workspace: WorkspaceId,
        change: Change,
        cx: &App,
    ) -> Task<anyhow::Result<()>>;

    /// The Repository GitHub shows by `name` now: the picker's typed row. One GitHub can't
    /// see is an [`ApiError`] `unknown_repository` whose message says so; signed out it is
    /// `not_signed_in`.
    fn resolve_repository(
        &self,
        name: RepositoryName,
        cx: &App,
    ) -> Task<anyhow::Result<Repository>>;

    /// Every GitHub repository the sign-in can reach, as the daemon holds it now, with how far
    /// loading them has got (spec §8): the picker's All your repositories.
    fn github_repositories(&self, cx: &App) -> Task<anyhow::Result<GithubRepositoryList>>;

    /// Asks the daemon to load the GitHub repository list again, as a picker does when it
    /// opens and on Retry. Answers before GitHub does: the pages come with `changed
    /// github_repositories` events.
    fn refresh_github_repositories(&self, cx: &App) -> Task<anyhow::Result<()>>;

    /// Links `workspace` to `repository` by GitHub's id, as the view holds it (spec §4): the
    /// Repository the daemon keeps with that id, or, when an unlink took it meanwhile, one
    /// stored again from `repository` without asking GitHub, so Undo works signed out.
    fn link_repository(
        &self,
        workspace: WorkspaceId,
        repository: Repository,
        cx: &App,
    ) -> Task<anyhow::Result<()>>;

    /// Unlinks `workspace` from Repository `repository`; the daemon deletes the Repository once
    /// nothing names it (spec §3). A refusal is an [`ApiError`]; `not_found` means the
    /// Workspace doesn't link it.
    fn unlink_repository(
        &self,
        workspace: WorkspaceId,
        repository: RepositoryId,
        cx: &App,
    ) -> Task<anyhow::Result<()>>;

    /// Mutes `workspace`'s notifications, or unmutes them.
    fn set_muted(&self, workspace: WorkspaceId, muted: bool, cx: &App) -> Task<anyhow::Result<()>>;

    /// Deletes `workspace` with its panes, unlinking its Repositories (each one nothing else
    /// names goes, spec §3). No folder is touched.
    fn delete_workspace(&self, workspace: WorkspaceId, cx: &App) -> Task<anyhow::Result<()>>;

    /// Every setting in `config.toml`, as the daemon uses it (spec §2).
    fn settings(&self, cx: &App) -> Task<anyhow::Result<Settings>>;

    /// Changes the settings `patch` names and returns them all. A refusal is an [`ApiError`];
    /// `invalid_setting` names the key in its message.
    fn update_settings(&self, patch: SettingsPatch, cx: &App) -> Task<anyhow::Result<Settings>>;

    /// Where the GitHub token comes from, and whose it is (spec §8).
    fn github_auth(&self, cx: &App) -> Task<anyhow::Result<GithubAuthInfo>>;

    /// Checks a pasted token with GitHub and saves it in the keyring. A refusal is an
    /// [`ApiError`]: `invalid_token`, `no_keyring` or `github_error`.
    fn save_token(&self, token: String, cx: &App) -> Task<anyhow::Result<GithubAuthInfo>>;

    /// Deletes the saved token: Sign out.
    fn sign_out(&self, cx: &App) -> Task<anyhow::Result<GithubAuthInfo>>;

    /// Saves the OAuth client ID in the keyring. A refusal is an [`ApiError`]:
    /// `invalid_client_id` or `no_keyring`.
    fn save_client_id(&self, client_id: String, cx: &App) -> Task<anyhow::Result<GithubAuthInfo>>;

    /// Deletes the saved OAuth client ID.
    fn clear_client_id(&self, cx: &App) -> Task<anyhow::Result<GithubAuthInfo>>;

    /// Starts Sign in with GitHub; how it ends comes as a `github_auth` event. A refusal is
    /// an [`ApiError`]: `no_client_id`, `no_keyring` or `github_error`.
    fn start_sign_in(&self, cx: &App) -> Task<anyhow::Result<DeviceSignIn>>;

    /// Cancels the sign-in running, if any.
    fn cancel_sign_in(&self, cx: &App) -> Task<anyhow::Result<()>>;
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

    fn respawn_pane(&self, pane: PaneId, run: PaneRun, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            panes::respawn(&client, pane, run).await?;
            Ok(())
        })
    }

    fn stop_pane(&self, pane: PaneId, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move { Ok(panes::stop(&client, pane).await?) })
    }

    fn submit_input(&self, pane: PaneId, text: String, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let input = PaneInput {
                text,
                when: InputWhen::Now,
                submit: true,
            };
            client
                .pane_input()
                .id(pane)
                .body(input)
                .send()
                .await
                .map_err(ApiError::from)?;
            Ok(())
        })
    }

    fn new_workspace(&self, request: Request, cx: &App) -> Task<anyhow::Result<WorkspaceId>> {
        let client = self.client.clone();
        let Request { name, link } = request;
        runtime::spawn(cx, async move {
            let made = agentdeck_client::workspaces::create(&client, name, link).await?;
            Ok(made.id)
        })
    }

    fn sync_prs(&self, full: bool, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            // An older daemon ignores `full` and runs the sync that is due.
            (client.prs_sync().full(full).send().await).map_err(ApiError::from)?;
            Ok(())
        })
    }

    fn close_prs(&self, prs: Vec<PrRef>, cx: &App) -> Task<anyhow::Result<Vec<PrCloseResult>>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let closed =
                (client.prs_close().body(PrClose { prs }).send().await).map_err(ApiError::from)?;
            Ok(closed.into_inner().results)
        })
    }

    fn launch_agent(&self, launch: Launch, cx: &App) -> Task<anyhow::Result<Launched>> {
        let client = self.client.clone();
        let Launch {
            workspace,
            agent,
            cwd,
        } = launch;
        runtime::spawn(cx, async move {
            let pane = panes::launch_agent(&client, workspace, agent, cwd).await?;
            Ok(Launched {
                workspace,
                tab: pane.tab_id,
                pane: pane.id,
            })
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

    fn edit_workspace(
        &self,
        workspace: WorkspaceId,
        change: Change,
        cx: &App,
    ) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            agentdeck_client::workspaces::update(&client, workspace, change).await?;
            Ok(())
        })
    }

    fn resolve_repository(
        &self,
        name: RepositoryName,
        cx: &App,
    ) -> Task<anyhow::Result<Repository>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            Ok(agentdeck_client::repositories::resolve(&client, &name).await?)
        })
    }

    fn github_repositories(&self, cx: &App) -> Task<anyhow::Result<GithubRepositoryList>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            Ok(agentdeck_client::repositories::github_list(&client).await?)
        })
    }

    fn refresh_github_repositories(&self, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            Ok(agentdeck_client::repositories::refresh_github_list(&client).await?)
        })
    }

    fn link_repository(
        &self,
        workspace: WorkspaceId,
        repository: Repository,
        cx: &App,
    ) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            agentdeck_client::repositories::link(&client, workspace, &repository).await?;
            Ok(())
        })
    }

    fn unlink_repository(
        &self,
        workspace: WorkspaceId,
        repository: RepositoryId,
        cx: &App,
    ) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            agentdeck_client::repositories::unlink(&client, workspace, repository).await?;
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

    fn delete_workspace(&self, workspace: WorkspaceId, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let deleting = client.workspace_delete().id(workspace).send();
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

    fn github_auth(&self, cx: &App) -> Task<anyhow::Result<GithubAuthInfo>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let auth = (client.github_auth_get().send().await).map_err(ApiError::from)?;
            Ok(auth.into_inner())
        })
    }

    fn save_token(&self, token: String, cx: &App) -> Task<anyhow::Result<GithubAuthInfo>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let body = GithubTokenBody { token };
            let auth =
                (client.github_token_put().body(body).send().await).map_err(ApiError::from)?;
            Ok(auth.into_inner())
        })
    }

    fn sign_out(&self, cx: &App) -> Task<anyhow::Result<GithubAuthInfo>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let auth = (client.github_token_delete().send().await).map_err(ApiError::from)?;
            Ok(auth.into_inner())
        })
    }

    fn save_client_id(&self, client_id: String, cx: &App) -> Task<anyhow::Result<GithubAuthInfo>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let body = GithubClientIdBody { client_id };
            let auth =
                (client.github_client_id_put().body(body).send().await).map_err(ApiError::from)?;
            Ok(auth.into_inner())
        })
    }

    fn clear_client_id(&self, cx: &App) -> Task<anyhow::Result<GithubAuthInfo>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let auth = (client.github_client_id_delete().send().await).map_err(ApiError::from)?;
            Ok(auth.into_inner())
        })
    }

    fn start_sign_in(&self, cx: &App) -> Task<anyhow::Result<DeviceSignIn>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            let sign_in = (client.github_device_start().send().await).map_err(ApiError::from)?;
            Ok(sign_in.into_inner())
        })
    }

    fn cancel_sign_in(&self, cx: &App) -> Task<anyhow::Result<()>> {
        let client = self.client.clone();
        runtime::spawn(cx, async move {
            (client.github_device_cancel().send().await).map_err(ApiError::from)?;
            Ok(())
        })
    }
}

/// Reads a [`WorkspacePanes`]: the tabs, then each pane in their layouts.
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
    Ok(WorkspacePanes { tabs, panes })
}

/// Reads a [`Snapshot`]: the workspaces, then each live one's Suggestions, tabs and the panes
/// in their layouts, then PR sync status, the pull requests and the Repositories with the
/// Workspaces linking each.
async fn snapshot(client: Client) -> anyhow::Result<Snapshot> {
    let workspaces = (client.workspace_list().send().await)
        .map_err(ApiError::from)?
        .into_inner();
    let mut panes = Vec::new();
    let mut suggestions = std::collections::HashMap::new();
    for workspace in workspaces.iter().filter(|w| w.archived_at.is_none()) {
        suggestions.insert(
            workspace.id,
            agentdeck_client::repositories::suggestions(&client, workspace.id).await?,
        );
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
                    agent: crate::pane::running_agent(&pane),
                    tab: tab.title.clone(),
                });
            }
        }
    }
    Ok(Snapshot {
        workspaces,
        panes,
        pr_sync: Some(
            (client.prs_status().send().await)
                .map_err(ApiError::from)?
                .into_inner(),
        ),
        prs: agentdeck_client::prs::list(&client).await?,
        repositories: agentdeck_client::repositories::list(&client).await?,
        suggestions,
    })
}

/// A scripted [`Api`] for tests.
#[cfg(test)]
pub(crate) mod fake {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use agentdeck_client::types::RepositoryLinks;
    use agentdeck_core::{AgentState, Layout, PaneId, PaneKind};

    use super::*;

    /// The daemon's defaults: the system theme, a 90-day PR cutoff, `xdg-open` and asking
    /// before quitting, nothing overridden.
    pub fn default_settings() -> Settings {
        use agentdeck_client::types::{
            AppearanceSettings, GuiSettings, PrSyncSettings, TerminalSettings, ThemePreference,
        };

        Settings {
            appearance: AppearanceSettings {
                theme: ThemePreference::System,
            },
            pr_sync: PrSyncSettings { max_age_days: 90 },
            terminal: TerminalSettings {
                file_opener: agentdeck_core::opener::DEFAULT.into(),
            },
            gui: GuiSettings { confirm_quit: true },
            overridden_by_env: Vec::new(),
        }
    }

    /// No token from any source and no client ID.
    pub fn signed_out() -> GithubAuthInfo {
        GithubAuthInfo {
            source: agentdeck_client::types::TokenSource::None,
            env_var: None,
            login: None,
            scopes: None,
            rejected: false,
            client_id_set: false,
        }
    }

    /// The daemon's `not_found` refusal, saying `message`.
    fn not_found(message: &str) -> anyhow::Error {
        ApiError {
            message: message.to_owned(),
            code: Some("not_found".into()),
        }
        .into()
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
        /// The last id handed to a tab or pane it made.
        made: Mutex<u32>,
        /// Why the next write fails, if it should.
        failure: Mutex<Option<anyhow::Error>>,
        /// Why the next snapshot read fails, if it should.
        read_failure: Mutex<Option<anyhow::Error>>,
        /// The workspaces asked for, in order.
        pub made_workspaces: Mutex<Vec<Request>>,
        /// The PR syncs asked for, in order: whether each was full.
        pub syncs: Mutex<Vec<bool>>,
        /// The agents started.
        pub launched: Mutex<Vec<Launch>>,
        /// The panes closed, in order.
        pub closed: Mutex<Vec<PaneId>>,
        /// The respawns asked for: the pane and what it runs, in order.
        pub respawned: Mutex<Vec<(PaneId, PaneRun)>>,
        /// The panes whose agents were stopped, in order.
        pub stopped: Mutex<Vec<PaneId>>,
        /// Holds the next stop's answer until its sender sends or drops.
        held_stop: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
        /// The messages submitted to panes: the pane and the text, in order.
        pub inputs: Mutex<Vec<(PaneId, String)>>,
        /// The workspaces archived, in order.
        pub archived: Mutex<Vec<WorkspaceId>>,
        /// The workspaces unarchived, in order.
        pub unarchived: Mutex<Vec<WorkspaceId>>,
        /// The mutes and unmutes asked for: the workspace and whether to mute it, in order.
        pub muted: Mutex<Vec<(WorkspaceId, bool)>>,
        /// The edits asked for: the workspace and what to change, in order.
        pub edits: Mutex<Vec<(WorkspaceId, Change)>>,
        /// The links asked for by GitHub's id: the workspace and the Repository, in order.
        pub links: Mutex<Vec<(WorkspaceId, Repository)>>,
        /// The unlinks asked for: the workspace and the Repository, in order.
        pub unlinks: Mutex<Vec<(WorkspaceId, agentdeck_core::RepositoryId)>>,
        /// The Repositories GitHub shows, by `owner/name` in lower case: what a typed name
        /// resolves to and what linking a Repository the snapshot doesn't hold finds.
        github_repositories: Mutex<HashMap<String, Repository>>,
        /// The names looked up on GitHub, in order.
        pub resolved: Mutex<Vec<RepositoryName>>,
        /// The GitHub repository list as the daemon holds it; loaded, and empty, when unset.
        github_list: Mutex<Option<GithubRepositoryList>>,
        /// Why reading the GitHub repository list fails, while it does.
        github_list_failure: Mutex<Option<String>>,
        /// How many times the GitHub repository list was asked to load again.
        pub github_list_refreshes: Mutex<usize>,
        /// The workspaces deleted, in order.
        pub deleted: Mutex<Vec<WorkspaceId>>,
        /// The settings; the daemon's defaults when unset.
        settings: Mutex<Option<Settings>>,
        /// How many times the settings were read.
        settings_reads: Mutex<usize>,
        /// Holds the next settings read's answer until its sender sends or drops.
        held_settings_read: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
        /// The settings patches sent, in order.
        pub settings_patches: Mutex<Vec<SettingsPatch>>,
        /// Each close asked for: the pull requests, in order.
        pub closes: Mutex<Vec<Vec<PrRef>>>,
        /// The PRs GitHub refuses to close, by their references in lower case, with its
        /// message.
        refused_prs: Mutex<HashMap<String, String>>,
        /// Holds the next close's answer until its sender sends or drops.
        held_close: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
        /// Where the GitHub token comes from; none and no client ID when unset.
        github: Mutex<Option<GithubAuthInfo>>,
        /// The login a pasted token belongs to, and its scopes.
        pub token_login: Mutex<(String, Option<Vec<String>>)>,
        /// The tokens pasted, in order.
        pub saved_tokens: Mutex<Vec<String>>,
        /// The client IDs saved, in order.
        pub saved_client_ids: Mutex<Vec<String>>,
        /// How many sign-ins were started.
        pub sign_ins: Mutex<usize>,
        /// How many sign-ins were cancelled.
        pub cancelled_sign_ins: Mutex<usize>,
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

        /// Fails the next snapshot read with `reason`.
        pub fn fail_next_read(&self, reason: &str) {
            *self.read_failure.lock().unwrap() = Some(anyhow::Error::msg(reason.to_owned()));
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

        /// The settings it holds now.
        pub fn settings_now(&self) -> Settings {
            (self.settings.lock().unwrap().clone()).unwrap_or_else(default_settings)
        }

        /// Answers later reads with `settings`, as if another client had changed them.
        pub fn set_settings(&self, settings: Settings) {
            *self.settings.lock().unwrap() = Some(settings);
        }

        /// Makes GitHub refuse to close pull request `pr` with `message`.
        pub fn refuse_close_pr(&self, pr: &PrRef, message: &str) {
            (self.refused_prs.lock().unwrap())
                .insert(pr.to_string().to_lowercase(), message.into());
        }

        /// Answers later reads with `prs` as the pull requests PR sync keeps.
        pub fn set_prs(&self, prs: Vec<agentdeck_client::types::PullRequest>) {
            self.snapshot.lock().unwrap().prs = prs;
        }

        /// Holds the next close's answer, as a daemon waiting on GitHub does, until the sender
        /// it returns sends or drops. What the close changes, it changes at once.
        pub fn hold_close(&self) -> tokio::sync::oneshot::Sender<()> {
            let (release, held) = tokio::sync::oneshot::channel();
            *self.held_close.lock().unwrap() = Some(held);
            release
        }

        /// Holds the next stop's answer, as a daemon slow to answer does, until the sender it
        /// returns sends or drops. The stop is recorded at once.
        pub fn hold_stop(&self) -> tokio::sync::oneshot::Sender<()> {
            let (release, held) = tokio::sync::oneshot::channel();
            *self.held_stop.lock().unwrap() = Some(held);
            release
        }

        /// Holds the next settings read's answer, as a slow daemon does, until the sender it
        /// returns sends or drops. The read answers with the settings held when it was asked.
        pub fn hold_settings_read(&self) -> tokio::sync::oneshot::Sender<()> {
            let (release, held) = tokio::sync::oneshot::channel();
            *self.held_settings_read.lock().unwrap() = Some(held);
            release
        }

        /// How many times the settings were read.
        pub fn settings_reads(&self) -> usize {
            *self.settings_reads.lock().unwrap()
        }

        /// The GitHub auth it holds now.
        pub fn github_now(&self) -> GithubAuthInfo {
            (self.github.lock().unwrap().clone()).unwrap_or_else(signed_out)
        }

        /// Answers later reads of the GitHub auth with `auth`.
        pub fn set_github(&self, auth: GithubAuthInfo) {
            *self.github.lock().unwrap() = Some(auth);
        }

        /// Answers later reads of the GitHub repository list with `list`.
        pub fn set_github_list(&self, list: GithubRepositoryList) {
            *self.github_list.lock().unwrap() = Some(list);
        }

        /// Fails every read of the GitHub repository list with `reason`, or (with `None`)
        /// stops failing them.
        pub fn fail_github_list(&self, reason: Option<&str>) {
            *self.github_list_failure.lock().unwrap() = reason.map(str::to_owned);
        }

        /// How many times the GitHub repository list was asked to load again.
        pub fn github_list_refreshes(&self) -> usize {
            *self.github_list_refreshes.lock().unwrap()
        }

        /// Makes GitHub show `repository`: a typed name resolves to it, and linking it by
        /// name stores it.
        pub fn on_github(&self, repository: Repository) {
            let key = repository.name.to_string().to_lowercase();
            self.github_repositories
                .lock()
                .unwrap()
                .insert(key, repository);
        }

        /// The renames asked for: the workspace and its new name, in order.
        pub fn renames(&self) -> Vec<(WorkspaceId, String)> {
            let edits = self.edits.lock().unwrap();
            (edits.iter())
                .filter_map(|(w, change)| Some((*w, change.name.clone()?)))
                .collect()
        }

        /// The Repository GitHub shows by `name`, or the daemon's `unknown_repository`.
        fn github(&self, name: &RepositoryName) -> anyhow::Result<Repository> {
            let key = name.to_string().to_lowercase();
            let found = self.github_repositories.lock().unwrap().get(&key).cloned();
            found.ok_or_else(|| {
                ApiError {
                    message: format!(
                        "GitHub can't see {name}. Check the name or your token's access."
                    ),
                    code: Some("unknown_repository".into()),
                }
                .into()
            })
        }

        /// Links `workspace` to each of `link` and unlinks it from each of `unlink`, as the
        /// daemon does: a Repository the snapshot doesn't hold by name is found on GitHub, and
        /// one nothing names any more goes. Nothing changes when one can't be found.
        fn relink(
            &self,
            workspace: WorkspaceId,
            link: &[RepositoryName],
            unlink: &[agentdeck_core::RepositoryId],
        ) -> anyhow::Result<()> {
            let mut found = Vec::new();
            for name in link {
                let known = (self.snapshot.lock().unwrap().repositories.iter())
                    .any(|r| r.name.to_string().eq_ignore_ascii_case(&name.to_string()));
                if !known {
                    found.push(self.github(name)?);
                }
            }
            let mut snapshot = self.snapshot.lock().unwrap();
            for repository in found {
                snapshot.repositories.push(RepositoryLinks {
                    id: repository.id,
                    name: repository.name,
                    url: repository.url,
                    private: repository.private,
                    archived: repository.archived,
                    unreachable: false,
                    workspaces: Vec::new(),
                });
            }
            for repository in &mut snapshot.repositories {
                let named = link.iter().any(|n| {
                    n.to_string()
                        .eq_ignore_ascii_case(&repository.name.to_string())
                });
                if named && !repository.workspaces.contains(&workspace) {
                    repository.workspaces.push(workspace);
                }
                if unlink.contains(&repository.id) {
                    repository.workspaces.retain(|w| *w != workspace);
                }
            }
            if let Some(suggested) = snapshot.suggestions.get_mut(&workspace) {
                suggested.retain(|r| {
                    !(link.iter()).any(|n| n.to_string().eq_ignore_ascii_case(&r.name.to_string()))
                });
            }
            let prs = snapshot.prs.clone();
            snapshot.repositories.retain(|repository| {
                !repository.workspaces.is_empty()
                    || prs.iter().any(|pr| pr.repository.id == repository.id)
            });
            snapshot
                .repositories
                .sort_by_key(|r| r.name.to_string().to_lowercase());
            Ok(())
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

        /// Adds a shell pane in `tab`, unplaced, in `cwd`.
        fn spawn_shell(&self, tab: TabId, cwd: String) -> PaneId {
            let pane = Pane {
                folder: cwd.clone(),
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
            if let Some(err) = self.read_failure.lock().unwrap().take() {
                return Task::ready(Err(err));
            }
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
            Task::ready(Ok(WorkspacePanes { tabs, panes }))
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

        fn respawn_pane(&self, pane: PaneId, run: PaneRun, _cx: &App) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            if let Some(held) = self.panes.lock().unwrap().iter_mut().find(|p| p.id == pane) {
                held.state = AgentState::Unknown;
                held.exit_code = None;
                if run == PaneRun::Shell {
                    held.kind = PaneKind::Shell;
                    held.agent = None;
                    held.session_id = None;
                    held.argv = vec!["/bin/sh".into()];
                }
            }
            self.respawned.lock().unwrap().push((pane, run));
            Task::ready(Ok(()))
        }

        fn stop_pane(&self, pane: PaneId, cx: &App) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            self.stopped.lock().unwrap().push(pane);
            match self.held_stop.lock().unwrap().take() {
                Some(held) => cx.background_executor().spawn(async move {
                    let _ = held.await;
                    Ok(())
                }),
                None => Task::ready(Ok(())),
            }
        }

        fn submit_input(&self, pane: PaneId, text: String, _cx: &App) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            self.inputs.lock().unwrap().push((pane, text));
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
            if let Err(err) = self.relink(id, &request.link, &[]) {
                self.snapshot
                    .lock()
                    .unwrap()
                    .workspaces
                    .retain(|w| w.id != id);
                return Task::ready(Err(err));
            }
            self.made_workspaces.lock().unwrap().push(request);
            Task::ready(Ok(id))
        }

        fn sync_prs(&self, full: bool, _cx: &App) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            self.syncs.lock().unwrap().push(full);
            Task::ready(Ok(()))
        }

        fn close_prs(&self, prs: Vec<PrRef>, cx: &App) -> Task<anyhow::Result<Vec<PrCloseResult>>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            self.closes.lock().unwrap().push(prs.clone());
            let refused_prs = self.refused_prs.lock().unwrap();
            let mut snapshot = self.snapshot.lock().unwrap();
            let results = (prs.into_iter())
                .map(|pr| {
                    let failed = |error: &str| PrCloseResult {
                        pr: pr.clone(),
                        ok: false,
                        error: Some(error.to_owned()),
                    };
                    // The daemon finds the PR by its Repository's name, ignoring case, and closes
                    // only one stored open and reachable.
                    let named = |repo: &str, number: i64| {
                        repo.eq_ignore_ascii_case(&pr.repository().to_string())
                            && number == i64::from(pr.number())
                    };
                    let stored = snapshot.prs.iter_mut().find(|stored| {
                        named(&stored.repository.name.to_string(), stored.number.into())
                            && stored.state == agentdeck_core::PrState::Open
                            && !stored.unreachable
                    });
                    let Some(stored) = stored else {
                        return failed("Not an open PR");
                    };
                    if let Some(message) = refused_prs.get(&pr.to_string().to_lowercase()) {
                        return failed(message);
                    }
                    // GitHub closed it: the daemon stores it closed and Finished.
                    stored.state = agentdeck_core::PrState::Closed;
                    stored.finished = true;
                    PrCloseResult {
                        pr,
                        ok: true,
                        error: None,
                    }
                })
                .collect();
            match self.held_close.lock().unwrap().take() {
                Some(held) => cx.background_executor().spawn(async move {
                    let _ = held.await;
                    Ok(results)
                }),
                None => Task::ready(Ok(results)),
            }
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

        fn edit_workspace(
            &self,
            workspace: WorkspaceId,
            change: Change,
            _cx: &App,
        ) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            if let Err(err) = self.relink(workspace, &change.link, &change.unlink) {
                return Task::ready(Err(err));
            }
            if let Some(name) = &change.name {
                let mut snapshot = self.snapshot.lock().unwrap();
                if let Some(w) = snapshot.workspaces.iter_mut().find(|w| w.id == workspace) {
                    w.name = name.clone();
                }
            }
            self.edits.lock().unwrap().push((workspace, change));
            Task::ready(Ok(()))
        }

        fn resolve_repository(
            &self,
            name: RepositoryName,
            _cx: &App,
        ) -> Task<anyhow::Result<Repository>> {
            self.resolved.lock().unwrap().push(name.clone());
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            Task::ready(self.github(&name))
        }

        fn github_repositories(&self, _cx: &App) -> Task<anyhow::Result<GithubRepositoryList>> {
            if let Some(reason) = self.github_list_failure.lock().unwrap().clone() {
                return Task::ready(Err(anyhow::Error::msg(reason)));
            }
            let list = self.github_list.lock().unwrap().clone();
            Task::ready(Ok(list.unwrap_or_else(|| GithubRepositoryList {
                status: agentdeck_client::types::GithubListStatus::Loaded,
                loaded: 0,
                total: None,
                error: None,
                repositories: Vec::new(),
            })))
        }

        fn refresh_github_repositories(&self, _cx: &App) -> Task<anyhow::Result<()>> {
            *self.github_list_refreshes.lock().unwrap() += 1;
            Task::ready(Ok(()))
        }

        fn link_repository(
            &self,
            workspace: WorkspaceId,
            repository: Repository,
            _cx: &App,
        ) -> Task<anyhow::Result<()>> {
            self.links
                .lock()
                .unwrap()
                .push((workspace, repository.clone()));
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let mut snapshot = self.snapshot.lock().unwrap();
            if !snapshot.workspaces.iter().any(|w| w.id == workspace) {
                return Task::ready(Err(not_found("workspace not found")));
            }
            if !(snapshot.repositories.iter()).any(|r| r.id == repository.id) {
                snapshot.repositories.push(RepositoryLinks {
                    id: repository.id,
                    name: repository.name,
                    url: repository.url,
                    private: repository.private,
                    archived: repository.archived,
                    unreachable: false,
                    workspaces: Vec::new(),
                });
                (snapshot.repositories).sort_by_key(|r| r.name.to_string().to_lowercase());
            }
            let held = (snapshot.repositories.iter_mut()).find(|r| r.id == repository.id);
            if let Some(held) = held.filter(|r| !r.workspaces.contains(&workspace)) {
                held.workspaces.push(workspace);
            }
            if let Some(suggested) = snapshot.suggestions.get_mut(&workspace) {
                suggested.retain(|r| r.id != repository.id);
            }
            Task::ready(Ok(()))
        }

        fn unlink_repository(
            &self,
            workspace: WorkspaceId,
            repository: agentdeck_core::RepositoryId,
            _cx: &App,
        ) -> Task<anyhow::Result<()>> {
            self.unlinks.lock().unwrap().push((workspace, repository));
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let linked = (self.snapshot.lock().unwrap().repositories.iter())
                .any(|r| r.id == repository && r.workspaces.contains(&workspace));
            if !linked {
                return Task::ready(Err(not_found("link not found")));
            }
            Task::ready(self.relink(workspace, &[], &[repository]))
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

        fn delete_workspace(&self, workspace: WorkspaceId, _cx: &App) -> Task<anyhow::Result<()>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let mut snapshot = self.snapshot.lock().unwrap();
            snapshot.workspaces.retain(|w| w.id != workspace);
            // Its Links go, and each Repository no Link and no kept PR names any more.
            for repository in &mut snapshot.repositories {
                repository.workspaces.retain(|w| *w != workspace);
            }
            let prs = std::mem::take(&mut snapshot.prs);
            snapshot.repositories.retain(|repository| {
                !repository.workspaces.is_empty()
                    || prs.iter().any(|pr| pr.repository.id == repository.id)
            });
            snapshot.prs = prs;
            self.deleted.lock().unwrap().push(workspace);
            Task::ready(Ok(()))
        }

        fn settings(&self, cx: &App) -> Task<anyhow::Result<Settings>> {
            *self.settings_reads.lock().unwrap() += 1;
            let settings = self.settings_now();
            match self.held_settings_read.lock().unwrap().take() {
                Some(held) => cx.background_executor().spawn(async move {
                    let _ = held.await;
                    Ok(settings)
                }),
                None => Task::ready(Ok(settings)),
            }
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
            if let Some(confirm) = patch.gui.as_ref().and_then(|g| g.confirm_quit) {
                settings.gui.confirm_quit = confirm;
            }
            self.set_settings(settings.clone());
            self.settings_patches.lock().unwrap().push(patch);
            Task::ready(Ok(settings))
        }

        fn github_auth(&self, _cx: &App) -> Task<anyhow::Result<GithubAuthInfo>> {
            Task::ready(Ok(self.github_now()))
        }

        fn save_token(&self, token: String, _cx: &App) -> Task<anyhow::Result<GithubAuthInfo>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let (login, scopes) = self.token_login.lock().unwrap().clone();
            let auth = GithubAuthInfo {
                source: agentdeck_client::types::TokenSource::Keyring,
                login: Some(login),
                scopes,
                rejected: false,
                ..self.github_now()
            };
            self.set_github(auth.clone());
            self.saved_tokens.lock().unwrap().push(token);
            Task::ready(Ok(auth))
        }

        fn sign_out(&self, _cx: &App) -> Task<anyhow::Result<GithubAuthInfo>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let auth = GithubAuthInfo {
                source: agentdeck_client::types::TokenSource::None,
                login: None,
                scopes: None,
                rejected: false,
                ..self.github_now()
            };
            self.set_github(auth.clone());
            Task::ready(Ok(auth))
        }

        fn save_client_id(
            &self,
            client_id: String,
            _cx: &App,
        ) -> Task<anyhow::Result<GithubAuthInfo>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let auth = GithubAuthInfo {
                client_id_set: true,
                ..self.github_now()
            };
            self.set_github(auth.clone());
            self.saved_client_ids.lock().unwrap().push(client_id);
            Task::ready(Ok(auth))
        }

        fn clear_client_id(&self, _cx: &App) -> Task<anyhow::Result<GithubAuthInfo>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            let auth = GithubAuthInfo {
                client_id_set: false,
                ..self.github_now()
            };
            self.set_github(auth.clone());
            Task::ready(Ok(auth))
        }

        fn start_sign_in(&self, _cx: &App) -> Task<anyhow::Result<DeviceSignIn>> {
            if let Some(err) = self.failure() {
                return Task::ready(Err(err));
            }
            *self.sign_ins.lock().unwrap() += 1;
            Task::ready(Ok(DeviceSignIn {
                user_code: "WDJB-MJHT".into(),
                verification_uri: "https://github.com/login/device".into(),
                expires_at: (chrono::Utc::now() + chrono::TimeDelta::seconds(872))
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            }))
        }

        fn cancel_sign_in(&self, _cx: &App) -> Task<anyhow::Result<()>> {
            *self.cancelled_sign_ins.lock().unwrap() += 1;
            Task::ready(Ok(()))
        }
    }
}
