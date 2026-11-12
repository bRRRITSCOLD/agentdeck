//! `agentdeck mcp`: an MCP server over stdio with the spec §10 tools. Each tool defaults to
//! the caller's workspace (`$AGENTDECK_WORKSPACE`) and pane (`$AGENTDECK_PANE`).

use agentdeck_client::Client;
use agentdeck_client::types::{NewNote, NewTask, NotePatch, TaskPatch};
use agentdeck_core::{NoteId, PaneId, TaskId, TaskStatus, WorkspaceId};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::schemars::JsonSchema;
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};

use crate::api::ApiError;
use crate::commands::{self, caller_pane};
use agentdeck_client::daemon;

/// Names the workspace an agent runs in; the tools' default workspace.
pub const WORKSPACE_VAR: &str = "AGENTDECK_WORKSPACE";

/// Serves MCP on stdin and stdout until the client closes them.
pub async fn serve() -> anyhow::Result<()> {
    let server = Server::new(Caller::from_env());
    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|err| anyhow::anyhow!("starting the MCP server: {err}"))?;
    running.waiting().await?;
    Ok(())
}

/// Who is calling: the workspace and pane the agent runs in, when it runs in one.
#[derive(Debug, Clone, Default)]
struct Caller {
    /// `$AGENTDECK_WORKSPACE`: an id, slug or name.
    workspace: Option<String>,
    /// `$AGENTDECK_PANE`.
    pane: Option<PaneId>,
}

impl Caller {
    fn from_env() -> Caller {
        Caller {
            workspace: std::env::var(WORKSPACE_VAR)
                .ok()
                .filter(|value| !value.is_empty()),
            pane: caller_pane(),
        }
    }
}

/// Which workspace a list or create tool works on.
#[derive(Debug, PartialEq, Eq)]
enum Scope<'a> {
    /// Every workspace and the inbox.
    All,
    /// Only tasks or notes with no workspace.
    Inbox,
    /// One workspace, by id, slug or name.
    Workspace(&'a str),
}

/// The scope a tool's `workspace`, `inbox` and `all_workspaces` arguments pick: what was asked
/// for, else the caller's workspace, else everything.
fn scope<'a>(
    workspace: Option<&'a str>,
    inbox: bool,
    all_workspaces: bool,
    caller: &'a Caller,
) -> Scope<'a> {
    match (workspace, inbox, all_workspaces) {
        (Some(workspace), _, _) => Scope::Workspace(workspace),
        (None, true, _) => Scope::Inbox,
        (None, false, true) => Scope::All,
        (None, false, false) => caller
            .workspace
            .as_deref()
            .map_or(Scope::All, Scope::Workspace),
    }
}

/// `tasks_list`'s arguments.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct TasksList {
    /// Only tasks with this status: todo, doing, review or done.
    status: Option<String>,
    /// Only tasks in this workspace (id, slug or name). Defaults to your workspace.
    workspace: Option<String>,
    /// Only tasks with no workspace (the inbox).
    #[serde(default)]
    inbox: bool,
    /// Tasks in every workspace and the inbox, instead of only yours.
    #[serde(default)]
    all_workspaces: bool,
}

/// A task's id.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct TaskRef {
    /// The task's id.
    id: String,
}

/// `task_create`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct TaskCreate {
    /// The title.
    title: String,
    /// The body.
    body: Option<String>,
    /// The workspace (id, slug or name). Defaults to your workspace; the inbox when you have none.
    workspace: Option<String>,
}

/// `task_update`'s arguments: the task and the fields to change.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct TaskUpdate {
    /// The task's id.
    id: String,
    /// A new title.
    title: Option<String>,
    /// A new body.
    body: Option<String>,
    /// A new status: todo, doing, review or done.
    status: Option<String>,
    /// A workspace to move it to (id, slug or name).
    workspace: Option<String>,
}

/// `notes_list`'s arguments.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct NotesList {
    /// Only notes on this workspace (id, slug or name). Defaults to your workspace.
    workspace: Option<String>,
    /// Only notes on this task.
    task: Option<String>,
    /// Only unread notes.
    #[serde(default)]
    unread: bool,
    /// Notes everywhere, instead of only on your workspace.
    #[serde(default)]
    all_workspaces: bool,
}

/// `note_create`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct NoteCreate {
    /// The message.
    body: String,
    /// The task to leave it on.
    task: Option<String>,
    /// The workspace to leave it on (id, slug or name). Defaults to your workspace when no task is given.
    workspace: Option<String>,
}

/// A note's id.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct NoteRef {
    /// The note's id.
    id: String,
}

/// A tool failure the model can read: a bad argument or the daemon's answer.
type ToolResult = Result<CallToolResult, rmcp::ErrorData>;

fn success(value: &impl Serialize) -> ToolResult {
    match serde_json::to_string_pretty(value) {
        Ok(text) => Ok(CallToolResult::success(vec![ContentBlock::text(text)])),
        Err(err) => failure(err),
    }
}

fn failure(err: impl std::fmt::Display) -> ToolResult {
    Ok(CallToolResult::error(vec![ContentBlock::text(
        err.to_string(),
    )]))
}

/// Runs a tool body; an `Err` becomes a tool error the model sees, not a protocol error.
async fn respond<T: Serialize>(body: impl Future<Output = anyhow::Result<T>>) -> ToolResult {
    match body.await {
        Ok(value) => success(&value),
        Err(err) => failure(format!("{err:#}")),
    }
}

fn parse<T: std::str::FromStr>(what: &str, value: &str) -> anyhow::Result<T>
where
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|err| anyhow::anyhow!("invalid {what} `{value}`: {err}"))
}

fn parse_opt<T: std::str::FromStr>(what: &str, value: Option<&str>) -> anyhow::Result<Option<T>>
where
    T::Err: std::fmt::Display,
{
    value.map(|value| parse(what, value)).transpose()
}

/// The MCP server: one tool per spec §10 entry.
#[derive(Debug, Clone)]
struct Server {
    caller: Caller,
    tool_router: ToolRouter<Server>,
}

impl Server {
    fn new(caller: Caller) -> Server {
        Server {
            caller,
            tool_router: Server::tool_router(),
        }
    }

    async fn workspace(
        client: &Client,
        reference: Option<&str>,
    ) -> anyhow::Result<Option<WorkspaceId>> {
        match reference {
            Some(reference) => Ok(Some(commands::workspace_id(client, reference).await?)),
            None => Ok(None),
        }
    }
}

#[tool_router]
impl Server {
    #[tool(
        description = "List tasks. Defaults to your workspace's tasks; set `inbox` for tasks \
                          with no workspace or `all_workspaces` for every task."
    )]
    async fn tasks_list(&self, Parameters(args): Parameters<TasksList>) -> ToolResult {
        respond(async {
            let client = daemon::connect().await?.client;
            let mut request = client.task_list();
            if let Some(status) = parse_opt::<TaskStatus>("status", args.status.as_deref())? {
                request = request.status(status);
            }
            match scope(
                args.workspace.as_deref(),
                args.inbox,
                args.all_workspaces,
                &self.caller,
            ) {
                Scope::All => {}
                Scope::Inbox => request = request.inbox(true),
                Scope::Workspace(reference) => {
                    request = request.workspace(commands::workspace_id(&client, reference).await?)
                }
            }
            Ok(request.send().await.map_err(ApiError::from)?.into_inner())
        })
        .await
    }

    #[tool(description = "Get one task, with its pull request when it has one.")]
    async fn task_get(&self, Parameters(args): Parameters<TaskRef>) -> ToolResult {
        respond(async {
            let client = daemon::connect().await?.client;
            let id: TaskId = parse("task id", &args.id)?;
            Ok(client
                .task_get()
                .id(id)
                .send()
                .await
                .map_err(ApiError::from)?
                .into_inner())
        })
        .await
    }

    #[tool(description = "Create a task. It goes in your workspace unless you name another.")]
    async fn task_create(&self, Parameters(args): Parameters<TaskCreate>) -> ToolResult {
        respond(async {
            let client = daemon::connect().await?.client;
            let reference = args
                .workspace
                .as_deref()
                .or(self.caller.workspace.as_deref());
            let workspace_id = Server::workspace(&client, reference).await?;
            let new = NewTask {
                body: args.body,
                status: None,
                title: args.title,
                workspace_id,
            };
            Ok(client
                .task_create()
                .body(new)
                .send()
                .await
                .map_err(ApiError::from)?
                .into_inner())
        })
        .await
    }

    #[tool(
        description = "Change a task's title, body, status (todo, doing, review, done) or workspace."
    )]
    async fn task_update(&self, Parameters(args): Parameters<TaskUpdate>) -> ToolResult {
        respond(async {
            let client = daemon::connect().await?.client;
            let id: TaskId = parse("task id", &args.id)?;
            let patch = TaskPatch {
                body: args.body,
                status: parse_opt("status", args.status.as_deref())?,
                title: args.title,
                workspace_id: Server::workspace(&client, args.workspace.as_deref()).await?,
            };
            Ok(client
                .task_update()
                .id(id)
                .body(patch)
                .send()
                .await
                .map_err(ApiError::from)?
                .into_inner())
        })
        .await
    }

    #[tool(
        description = "List notes. Defaults to notes on your workspace; name a task for its \
                          notes, or set `all_workspaces` for every note."
    )]
    async fn notes_list(&self, Parameters(args): Parameters<NotesList>) -> ToolResult {
        respond(async {
            let client = daemon::connect().await?.client;
            let mut request = client.note_list();
            if let Some(task) = parse_opt::<TaskId>("task id", args.task.as_deref())? {
                request = request.task(task);
            } else if let Scope::Workspace(reference) = scope(
                args.workspace.as_deref(),
                false,
                args.all_workspaces,
                &self.caller,
            ) {
                request = request.workspace(commands::workspace_id(&client, reference).await?);
            }
            if args.unread {
                request = request.unread(true);
            }
            Ok(request.send().await.map_err(ApiError::from)?.into_inner())
        })
        .await
    }

    #[tool(description = "Leave a note on a task, or on a workspace (yours by default).")]
    async fn note_create(&self, Parameters(args): Parameters<NoteCreate>) -> ToolResult {
        respond(async {
            let client = daemon::connect().await?.client;
            let task_id = parse_opt::<TaskId>("task id", args.task.as_deref())?;
            let workspace_id = match task_id {
                Some(_) => Server::workspace(&client, args.workspace.as_deref()).await?,
                None => {
                    let reference = args
                        .workspace
                        .as_deref()
                        .or(self.caller.workspace.as_deref());
                    let Some(reference) = reference else {
                        anyhow::bail!("name a task or a workspace: you aren't in a workspace");
                    };
                    Some(commands::workspace_id(&client, reference).await?)
                }
            };
            let new = NewNote {
                body: args.body,
                task_id,
                workspace_id,
            };
            Ok(client
                .note_create()
                .body(new)
                .send()
                .await
                .map_err(ApiError::from)?
                .into_inner())
        })
        .await
    }

    #[tool(description = "Mark a note read, by your pane.")]
    async fn note_mark_read(&self, Parameters(args): Parameters<NoteRef>) -> ToolResult {
        respond(async {
            let client = daemon::connect().await?.client;
            let id: NoteId = parse("note id", &args.id)?;
            let patch = NotePatch {
                read: Some(true),
                read_by_pane: self.caller.pane,
                ..NotePatch::default()
            };
            Ok(client
                .note_update()
                .id(id)
                .body(patch)
                .send()
                .await
                .map_err(ApiError::from)?
                .into_inner())
        })
        .await
    }

    #[tool(description = "List every workspace, archived ones included.")]
    async fn workspaces_list(&self) -> ToolResult {
        respond(async {
            let client = daemon::connect().await?.client;
            Ok(client
                .workspace_list()
                .send()
                .await
                .map_err(ApiError::from)?
                .into_inner())
        })
        .await
    }

    #[tool(description = "The GitHub login state of PR sync and when it last synced.")]
    async fn prs_status(&self) -> ToolResult {
        respond(async {
            let client = daemon::connect().await?.client;
            Ok(client
                .prs_status()
                .send()
                .await
                .map_err(ApiError::from)?
                .into_inner())
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("agentdeck", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "agentdeck's tasks, notes, workspaces and PR status. Tools default to the \
                 workspace and pane you run in.",
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller(workspace: Option<&str>) -> Caller {
        Caller {
            workspace: workspace.map(str::to_owned),
            pane: None,
        }
    }

    mod scope {
        use super::*;

        #[test]
        fn should_default_to_the_callers_workspace() {
            let caller = caller(Some("web"));

            assert_eq!(scope(None, false, false, &caller), Scope::Workspace("web"));
        }

        #[test]
        fn should_prefer_the_workspace_asked_for() {
            let caller = caller(Some("web"));

            assert_eq!(
                scope(Some("api"), false, false, &caller),
                Scope::Workspace("api")
            );
        }

        #[test]
        fn should_take_the_inbox_over_the_callers_workspace() {
            let caller = caller(Some("web"));

            assert_eq!(scope(None, true, false, &caller), Scope::Inbox);
        }

        #[test]
        fn should_widen_to_everything_on_request() {
            let caller = caller(Some("web"));

            assert_eq!(scope(None, false, true, &caller), Scope::All);
        }

        #[test]
        fn should_be_everything_outside_a_workspace() {
            assert_eq!(scope(None, false, false, &caller(None)), Scope::All);
        }
    }

    mod tools {
        use super::*;

        #[test]
        fn should_be_the_spec_tool_list() {
            let server = Server::new(Caller::default());
            let mut names: Vec<String> = server
                .tool_router
                .list_all()
                .into_iter()
                .map(|tool| tool.name.into_owned())
                .collect();
            names.sort();

            assert_eq!(
                names,
                [
                    "note_create",
                    "note_mark_read",
                    "notes_list",
                    "prs_status",
                    "task_create",
                    "task_get",
                    "task_update",
                    "tasks_list",
                    "workspaces_list",
                ]
            );
        }
    }
}
