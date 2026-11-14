//! Running each CLI command against the daemon.

use std::path::Path;

use agentdeck_client::types::{
    InputWhen, InstructionPatch, NewComment, NewInstruction, NewTask, NewWorkspace, PaneInput,
    TaskPatch,
};
use agentdeck_client::{Client, panes};
use agentdeck_core::{PaneId, WorkspaceId};
use anyhow::Context;
use futures_util::TryStreamExt;
use serde::Serialize;
use serde_json::{Value, json};

use crate::cli::{
    AgentCommand, Command, CommentCommand, ConfigCommand, InstructionCommand, PrCommand,
    TaskCommand, WsCommand,
};
use crate::{launch, render, resolve};
use agentdeck_client::ApiError;

/// Names the pane an agent runs in; a comment added from it is that pane's agent's.
pub const PANE_VAR: &str = "AGENTDECK_PANE";

/// Names the workspace an agent runs in; the default workspace for instructions and the MCP
/// tools.
pub const WORKSPACE_VAR: &str = "AGENTDECK_WORKSPACE";

/// What a command prints: `json` with `--json`, else `human`.
pub struct Output {
    /// The machine-readable form, usually the API's own object.
    pub json: Value,
    /// The human-readable form; raw bytes for `tail`.
    pub human: Vec<u8>,
}

impl Output {
    fn new(json: &impl Serialize, human: String) -> anyhow::Result<Output> {
        Ok(Output {
            json: serde_json::to_value(json)?,
            human: human.into_bytes(),
        })
    }
}

/// Runs `command`, except `mcp`, which `main` serves itself.
pub async fn run(client: &Client, command: Command) -> anyhow::Result<Output> {
    match command {
        Command::Ws(command) => ws(client, command).await,
        Command::Task(command) => task(client, command).await,
        Command::Comment(command) => comment(client, command).await,
        Command::Instruction(command) => instruction(client, command).await,
        Command::Agent(command) => agent(client, command).await,
        Command::Pr(command) => pr(client, command).await,
        Command::Config(command) => config(client, command).await,
        Command::Tail { pane, bytes } => tail(client, pane, bytes).await,
        Command::Mcp => unreachable!("main serves mcp"),
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

async fn optional_workspace(
    client: &Client,
    reference: Option<&str>,
) -> anyhow::Result<Option<WorkspaceId>> {
    match reference {
        Some(reference) => Ok(Some(workspace_id(client, reference).await?)),
        None => Ok(None),
    }
}

/// An absolute form of `dir`, since the daemon takes only absolute dirs.
pub fn absolute(dir: &Path) -> anyhow::Result<String> {
    let dir = std::path::absolute(dir).with_context(|| format!("resolving {}", dir.display()))?;
    dir.into_os_string()
        .into_string()
        .map_err(|dir| anyhow::anyhow!("{} isn't UTF-8", Path::new(&dir).display()))
}

async fn ws(client: &Client, command: WsCommand) -> anyhow::Result<Output> {
    match command {
        WsCommand::Ls => {
            let list = client
                .workspace_list()
                .send()
                .await
                .map_err(ApiError::from)?;
            Output::new(&*list, render::workspaces(&list))
        }
        WsCommand::New { name, dir } => {
            let dir = dir.as_deref().map(absolute).transpose()?;
            let ws = client
                .workspace_create()
                .body(NewWorkspace { dir, name })
                .send()
                .await
                .map_err(ApiError::from)?;
            Output::new(&*ws, render::workspace(&ws))
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
        WsCommand::Rm { ws, force } => {
            let id = workspace_id(client, &ws).await?;
            match client.workspace_delete().id(id).force(force).send().await {
                Ok(_) => Output::new(
                    &json!({ "id": id, "deleted": true }),
                    format!("deleted {ws}\n"),
                ),
                Err(agentdeck_client::Error::ErrorResponse(response))
                    if response.error_code.as_deref() == Some("dirty") =>
                {
                    let report = response.into_inner().dirty;
                    anyhow::bail!(
                        "{ws} holds work that isn't pushed; `--force` deletes it anyway:\n{}",
                        report
                            .as_ref()
                            .map(render::dirty)
                            .unwrap_or_default()
                            .trim_end()
                    )
                }
                Err(err) => Err(ApiError::from(err).into()),
            }
        }
    }
}

async fn task(client: &Client, command: TaskCommand) -> anyhow::Result<Output> {
    match command {
        TaskCommand::Ls { status, ws, inbox } => {
            let mut request = client.task_list();
            if let Some(status) = status {
                request = request.status(status);
            }
            if let Some(ws) = optional_workspace(client, ws.as_deref()).await? {
                request = request.workspace(ws);
            }
            if inbox {
                request = request.inbox(true);
            }
            let tasks = request.send().await.map_err(ApiError::from)?;
            Output::new(&*tasks, render::tasks(&tasks))
        }
        TaskCommand::Add { title, ws, body } => {
            let workspace_id = optional_workspace(client, ws.as_deref()).await?;
            let task = client
                .task_create()
                .body(NewTask {
                    body,
                    status: None,
                    title,
                    workspace_id,
                })
                .send()
                .await
                .map_err(ApiError::from)?;
            Output::new(&*task, render::task(&task))
        }
        TaskCommand::Set { id, status } => {
            let task = client
                .task_update()
                .id(id)
                .body(TaskPatch {
                    status: Some(status),
                    ..TaskPatch::default()
                })
                .send()
                .await
                .map_err(ApiError::from)?;
            Output::new(&*task, render::task(&task))
        }
        TaskCommand::Show { id } => {
            let task = client
                .task_get()
                .id(id)
                .send()
                .await
                .map_err(ApiError::from)?;
            Output::new(&*task, render::task(&task))
        }
    }
}

async fn comment(client: &Client, command: CommentCommand) -> anyhow::Result<Output> {
    match command {
        CommentCommand::Ls { task } => {
            let comments = client
                .comment_list()
                .id(task)
                .send()
                .await
                .map_err(ApiError::from)?;
            Output::new(&*comments, render::comments(&comments))
        }
        CommentCommand::Add { body, task } => {
            let comment = client
                .comment_create()
                .id(task)
                .body(NewComment {
                    body,
                    pane: caller_pane(),
                })
                .send()
                .await
                .map_err(ApiError::from)?;
            Output::new(&*comment, render::comment_created(&comment))
        }
    }
}

/// The workspace `--ws` names, else the caller's (`$AGENTDECK_WORKSPACE`).
async fn instruction_workspace(
    client: &Client,
    reference: Option<String>,
) -> anyhow::Result<WorkspaceId> {
    let reference = reference.or_else(caller_workspace).ok_or_else(|| {
        anyhow::anyhow!("name a workspace with --ws: you aren't in one (no ${WORKSPACE_VAR})")
    })?;
    workspace_id(client, &reference).await
}

async fn instruction(client: &Client, command: InstructionCommand) -> anyhow::Result<Output> {
    match command {
        InstructionCommand::Ls { ws } => {
            let workspace = instruction_workspace(client, ws).await?;
            let instructions = client
                .instruction_list()
                .id(workspace)
                .send()
                .await
                .map_err(ApiError::from)?;
            Output::new(&*instructions, render::instructions(&instructions))
        }
        InstructionCommand::Add { body, ws } => {
            let workspace = instruction_workspace(client, ws).await?;
            let instruction = client
                .instruction_create()
                .id(workspace)
                .body(NewInstruction { body })
                .send()
                .await
                .map_err(ApiError::from)?;
            Output::new(&*instruction, render::instruction_saved(&instruction))
        }
        InstructionCommand::Edit { id, body } => {
            let instruction = client
                .instruction_update()
                .id(id)
                .body(InstructionPatch { body: Some(body) })
                .send()
                .await
                .map_err(ApiError::from)?;
            Output::new(&*instruction, render::instruction_saved(&instruction))
        }
        InstructionCommand::Rm { id } => {
            client
                .instruction_delete()
                .id(id)
                .send()
                .await
                .map_err(ApiError::from)?;
            Output::new(
                &json!({ "id": id, "deleted": true }),
                format!("deleted {id}\n"),
            )
        }
    }
}

/// `$AGENTDECK_WORKSPACE`, when it is set: an id, slug or name.
pub fn caller_workspace() -> Option<String> {
    std::env::var(WORKSPACE_VAR)
        .ok()
        .filter(|value| !value.is_empty())
}

/// `$AGENTDECK_PANE`, when it holds a pane id.
pub fn caller_pane() -> Option<PaneId> {
    std::env::var(PANE_VAR).ok()?.parse().ok()
}

async fn agent(client: &Client, command: AgentCommand) -> anyhow::Result<Output> {
    match command {
        AgentCommand::New {
            ws,
            agent,
            cwd,
            task,
        } => {
            let pane = launch::agent_new(
                client,
                launch::Launch {
                    ws: &ws,
                    agent,
                    cwd: cwd.as_deref(),
                    task,
                },
            )
            .await?;
            Output::new(&pane, render::pane(&pane))
        }
        AgentCommand::Link { pane, task, .. } => {
            let linked = panes::link_task(client, pane, task).await?;
            let human = match task {
                Some(task) => format!("linked {pane} to {task}\n"),
                None => format!("unlinked {pane}\n"),
            };
            Output::new(&linked, human)
        }
        AgentCommand::Send { pane, text, now } => {
            let accepted = client
                .pane_input()
                .id(pane)
                .body(PaneInput {
                    text: format!("{text}\r"),
                    when: if now { InputWhen::Now } else { InputWhen::Idle },
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

async fn pr(client: &Client, command: PrCommand) -> anyhow::Result<Output> {
    let status = match command {
        PrCommand::Sync => client.prs_sync().send().await,
        PrCommand::Status => client.prs_status().send().await,
    }
    .map_err(ApiError::from)?;
    Output::new(&*status, render::pr_status(&status))
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
    })
}
