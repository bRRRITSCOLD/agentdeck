//! Running each CLI command against the daemon.

use std::path::Path;

use std::io::{IsTerminal, Write};

use agentdeck_client::types::{InputWhen, NewLink, NewWorkspace, PaneInput, PrClose};
use agentdeck_client::{Client, panes};
use agentdeck_core::{Layout, PaneId, PrRef, RepositoryName, WorkspaceId};
use anyhow::Context;
use futures_util::TryStreamExt;
use serde::Serialize;
use serde_json::{Value, json};

use crate::cli::{AgentCommand, Command, ConfigCommand, PrCommand, RepoCommand, WsCommand};
use crate::resolve::IdRef;
use crate::{launch, render, resolve};
use agentdeck_client::ApiError;

/// What a command prints: `json` with `--json`, else `human`.
pub struct Output {
    /// The machine-readable form, usually the API's own object.
    pub json: Value,
    /// The human-readable form; raw bytes for `tail`.
    pub human: Vec<u8>,
    /// Whether the CLI exits 1 after printing it: `pr close` when any PR didn't close.
    pub failed: bool,
}

impl Output {
    pub fn new(json: &impl Serialize, human: String) -> anyhow::Result<Output> {
        Ok(Output {
            json: serde_json::to_value(json)?,
            human: human.into_bytes(),
            failed: false,
        })
    }
}

/// Runs `command`, except `daemon`, which `main` runs itself.
pub async fn run(client: &Client, command: Command) -> anyhow::Result<Output> {
    match command {
        Command::Ws(command) => ws(client, command).await,
        Command::Agent(command) => agent(client, command).await,
        Command::Repo(command) => repo(client, command).await,
        Command::Pr(command) => pr(client, command).await,
        Command::Config(command) => config(client, command).await,
        Command::Tail { pane, bytes } => tail(client, pane_id(client, &pane).await?, bytes).await,
        Command::Daemon(_) => unreachable!("main runs daemon"),
    }
}

/// The workspace `reference` names: an id, a slug or a name.
pub async fn workspace_id(client: &Client, reference: &str) -> anyhow::Result<WorkspaceId> {
    if let Ok(id) = reference.parse() {
        return Ok(id);
    }
    let workspaces = client
        .workspace_list()
        .send()
        .await
        .map_err(ApiError::from)?
        .into_inner();
    resolve::workspace(&workspaces, reference)
}

/// The pane `reference` names: its id, or a short id matched against the panes in every
/// workspace's tab layouts (spec §10).
pub async fn pane_id(client: &Client, reference: &str) -> anyhow::Result<PaneId> {
    let digits = match resolve::id_ref("pane", reference)? {
        IdRef::Whole(id) => return Ok(id),
        IdRef::Short(digits) => digits,
    };
    let workspaces = client
        .workspace_list()
        .send()
        .await
        .map_err(ApiError::from)?
        .into_inner();
    let mut candidates = Vec::new();
    for ws in &workspaces {
        let tabs = client
            .tab_list()
            .id(ws.id)
            .send()
            .await
            .map_err(ApiError::from)?
            .into_inner();
        for tab in tabs {
            let ids = tab.layout.as_ref().map(Layout::panes).unwrap_or_default();
            candidates.extend(
                ids.into_iter()
                    .map(|id| (id, format!("{} · {}", ws.slug, tab.title))),
            );
        }
    }
    resolve::by_short_id("pane", candidates, reference, &digits)
}

/// An absolute form of `dir`, since the daemon takes only absolute dirs.
pub fn absolute(dir: &Path) -> anyhow::Result<String> {
    let dir = std::path::absolute(dir).with_context(|| format!("resolving {}", dir.display()))?;
    dir.into_os_string()
        .into_string()
        .map_err(|dir| anyhow::anyhow!("{} isn't UTF-8", Path::new(&dir).display()))
}

/// `ws mute` and `ws unmute`: patches the workspace's `muted`, printing `muted <slug>` or
/// `unmuted <slug>`.
async fn set_muted(client: &Client, ws: &str, muted: bool) -> anyhow::Result<Output> {
    let id = workspace_id(client, ws).await?;
    let ws = client
        .workspace_update()
        .id(id)
        .body_map(|patch| patch.muted(Some(muted)))
        .send()
        .await
        .map_err(ApiError::from)?;
    let verb = if muted { "muted" } else { "unmuted" };
    Output::new(&*ws, format!("{verb} {}\n", ws.slug))
}

/// The Repositories agentdeck keeps, with the Workspaces linking each; only those
/// `workspace` links when given.
async fn repositories(
    client: &Client,
    workspace: Option<WorkspaceId>,
) -> anyhow::Result<Vec<agentdeck_client::types::RepositoryLinks>> {
    let mut request = client.repository_list();
    if let Some(workspace) = workspace {
        request = request.workspace(workspace);
    }
    Ok(request.send().await.map_err(ApiError::from)?.into_inner())
}

async fn ws(client: &Client, command: WsCommand) -> anyhow::Result<Output> {
    match command {
        WsCommand::Ls => {
            let list = client
                .workspace_list()
                .send()
                .await
                .map_err(ApiError::from)?
                .into_inner();
            let listed = render::listed_workspaces(list, &repositories(client, None).await?);
            Output::new(&listed, render::workspaces(&listed))
        }
        WsCommand::New { name } => {
            let ws = client
                .workspace_create()
                .body(NewWorkspace {
                    name,
                    link: Vec::new(),
                })
                .send()
                .await
                .map_err(ApiError::from)?;
            Output::new(&*ws, render::workspace(&ws))
        }
        WsCommand::Rename { ws, name } => {
            let id = workspace_id(client, &ws).await?;
            let ws = client
                .workspace_update()
                .id(id)
                .body_map(|patch| patch.name(Some(name)))
                .send()
                .await
                .map_err(ApiError::from)?
                .into_inner();
            let human = render::workspaces(&render::listed_workspaces(
                vec![ws.clone()],
                &repositories(client, Some(id)).await?,
            ));
            Output::new(&ws, human)
        }
        WsCommand::Archive { ws } => {
            let id = workspace_id(client, &ws).await?;
            let ws = client
                .workspace_archive()
                .id(id)
                .send()
                .await
                .map_err(ApiError::from)?;
            Output::new(&*ws, format!("archived {}\n", ws.slug))
        }
        WsCommand::Mute { ws } => set_muted(client, &ws, true).await,
        WsCommand::Unmute { ws } => set_muted(client, &ws, false).await,
        WsCommand::Rm { ws } => {
            let id = workspace_id(client, &ws).await?;
            (client.workspace_delete().id(id).send().await).map_err(ApiError::from)?;
            Output::new(
                &json!({ "id": id, "deleted": true }),
                format!("deleted {ws}\n"),
            )
        }
    }
}

/// `agent ls`: every pane in the layouts of the tabs of the workspace `ws` names, else of
/// every workspace, in workspace, tab and layout order; exited panes too, until closed.
async fn agent_ls(client: &Client, ws: Option<&str>) -> anyhow::Result<Output> {
    let workspaces = client
        .workspace_list()
        .send()
        .await
        .map_err(ApiError::from)?
        .into_inner();
    let only = ws
        .map(|reference| resolve::workspace(&workspaces, reference))
        .transpose()?;
    let mut listed = Vec::new();
    for ws in (workspaces.iter()).filter(|ws| only.is_none_or(|id| ws.id == id)) {
        let tabs = client
            .tab_list()
            .id(ws.id)
            .send()
            .await
            .map_err(ApiError::from)?
            .into_inner();
        for tab in tabs {
            let ids = tab.layout.as_ref().map(Layout::panes).unwrap_or_default();
            for id in ids {
                // Gone since the tab was read: closed meanwhile.
                let Some(pane) = panes::pane_if_any(client, id).await? else {
                    continue;
                };
                listed.push(render::ListedPane {
                    workspace_id: ws.id,
                    workspace: ws.slug.clone(),
                    tab: tab.title.clone(),
                    pane,
                });
            }
        }
    }
    Output::new(&listed, render::panes(&listed))
}

async fn agent(client: &Client, command: AgentCommand) -> anyhow::Result<Output> {
    match command {
        AgentCommand::Ls { ws } => agent_ls(client, ws.as_deref()).await,
        AgentCommand::New { ws, agent, cwd } => {
            let pane = launch::agent_new(
                client,
                launch::Launch {
                    ws: &ws,
                    agent,
                    cwd: cwd.as_deref(),
                },
            )
            .await?;
            Output::new(&pane, render::pane(&pane))
        }
        AgentCommand::Stop { pane } => {
            let pane = pane_id(client, &pane).await?;
            panes::stop(client, pane).await?;
            Output::new(
                &json!({ "id": pane, "stopped": true }),
                format!("stopped {pane}\n"),
            )
        }
        AgentCommand::Send { pane, text, now } => {
            let pane = pane_id(client, &pane).await?;
            let accepted = client
                .pane_input()
                .id(pane)
                .body(PaneInput {
                    text,
                    when: if now { InputWhen::Now } else { InputWhen::Idle },
                    submit: true,
                })
                .send()
                .await
                .map_err(ApiError::from)?;
            let human = if accepted.queued {
                format!("queued until {pane} is idle\n")
            } else {
                format!("sent to {pane}\n")
            };
            Output::new(&*accepted, human)
        }
    }
}

async fn repo(client: &Client, command: RepoCommand) -> anyhow::Result<Output> {
    match command {
        RepoCommand::Ls { ws } => {
            let workspaces = client
                .workspace_list()
                .send()
                .await
                .map_err(ApiError::from)?
                .into_inner();
            let ws = ws
                .map(|ws| resolve::workspace(&workspaces, &ws))
                .transpose()?;
            let list = repositories(client, ws).await?;
            Output::new(&list, render::repositories(&list, &workspaces))
        }
        RepoCommand::Link { repo, ws } => {
            let id = workspace_id(client, &ws).await?;
            let linked = client
                .link_create()
                .id(id)
                .body(NewLink::named(repo))
                .send()
                .await
                .map_err(ApiError::from)?;
            Output::new(&*linked, format!("linked {} to {ws}\n", linked.name))
        }
        RepoCommand::Unlink { repo, ws } => {
            let id = workspace_id(client, &ws).await?;
            let linked = client
                .repository_list()
                .workspace(id)
                .send()
                .await
                .map_err(ApiError::from)?
                .into_inner();
            let Some(repository) = linked.iter().find(|linked| same_name(&linked.name, &repo))
            else {
                anyhow::bail!("{repo} isn't linked to {ws} (not_found)");
            };
            client
                .link_delete()
                .id(id)
                .repository(repository.id)
                .send()
                .await
                .map_err(ApiError::from)?;
            Output::new(
                &json!({ "workspace_id": id, "repository_id": repository.id, "unlinked": true }),
                format!("unlinked {} from {ws}\n", repository.name),
            )
        }
    }
}

/// Whether two Repository names are the same, ignoring case as GitHub does.
fn same_name(a: &RepositoryName, b: &RepositoryName) -> bool {
    a.to_string().eq_ignore_ascii_case(&b.to_string())
}

async fn pr(client: &Client, command: PrCommand) -> anyhow::Result<Output> {
    let status = match command {
        PrCommand::Ls { repo, ws } => {
            let mut request = client.pr_list();
            if let Some(repo) = repo {
                request = request.repository(repo);
            }
            if let Some(ws) = ws {
                request = request.workspace(workspace_id(client, &ws).await?);
            }
            let prs = request.send().await.map_err(ApiError::from)?;
            return Output::new(&*prs, render::pull_requests(&prs));
        }
        // A sync someone asks for is full (spec §8).
        PrCommand::Sync => client.prs_sync().full(true).send().await,
        PrCommand::Status => client.prs_status().send().await,
        PrCommand::Close { prs, yes } => return close_prs(client, prs, yes).await,
    }
    .map_err(ApiError::from)?;
    Output::new(&*status, render::pr_status(&status))
}

/// `pr close`: asks first unless `yes`, on a terminal only, then closes the pull requests
/// `prs` names with `POST /v1/prs/close`, printing a line per PR. Fails when any didn't close.
async fn close_prs(client: &Client, prs: Vec<PrRef>, yes: bool) -> anyhow::Result<Output> {
    if !yes {
        if !std::io::stdin().is_terminal() {
            anyhow::bail!(
                "pr close asks before closing PRs, and stdin isn't a terminal; pass --yes to \
                 close them without asking"
            );
        }
        let repos: Vec<String> = prs.iter().map(|pr| pr.repository().to_string()).collect();
        let repos: Vec<&str> = repos.iter().map(String::as_str).collect();
        eprint!("{}", render::close_prompt(prs.len(), &repos));
        std::io::stderr().flush()?;
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .context("reading the answer")?;
        if answer.trim() != "y" {
            return Output::new(&json!({ "results": [] }), "Closed nothing\n".into());
        }
    }
    let closed = client
        .prs_close()
        .body(PrClose { prs })
        .send()
        .await
        .map_err(ApiError::from)?
        .into_inner();
    let mut output = Output::new(&closed, render::close_results(&closed.results))?;
    output.failed = closed.results.iter().any(|result| !result.ok);
    Ok(output)
}

async fn config(client: &Client, command: ConfigCommand) -> anyhow::Result<Output> {
    use agentdeck_client::settings;

    match command {
        ConfigCommand::Ls => {
            let all = client.settings_get().send().await.map_err(ApiError::from)?;
            Output::new(&*all, render::settings(&all, None))
        }
        ConfigCommand::Get { key } => {
            let all = client.settings_get().send().await.map_err(ApiError::from)?;
            let value = settings::value(&all, &key).ok_or_else(|| {
                anyhow::anyhow!("{key} isn't a setting; `agentdeck config ls` lists them")
            })?;
            let human = format!("{}\n", render::setting_value(&value));
            Output::new(&json!({ "key": key, "value": value }), human)
        }
        ConfigCommand::Set { key, value } => {
            let all = settings::set(client, &key, settings::parse_value(&value)).await?;
            Output::new(&all, render::settings(&all, Some(&key)))
        }
    }
}

async fn tail(client: &Client, pane: PaneId, bytes: Option<u64>) -> anyhow::Result<Output> {
    let mut request = client.pane_tail().id(pane);
    if let Some(bytes) = bytes {
        request = request.bytes(bytes);
    }
    let stream = request.send().await.map_err(ApiError::from)?.into_inner();
    let chunks: Vec<_> = stream
        .into_inner()
        .try_collect()
        .await
        .context("reading the pane's output")?;
    let output = chunks.concat();
    Ok(Output {
        json: json!({ "pane": pane, "output": String::from_utf8_lossy(&output) }),
        human: output,
        failed: false,
    })
}
