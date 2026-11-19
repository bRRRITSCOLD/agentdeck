//! `agentdeck mcp`: an MCP server over stdio with the spec §10 tools. Each tool defaults to
//! the caller's workspace (`$AGENTDECK_WORKSPACE`) and pane (`$AGENTDECK_PANE`), and a task
//! tool to the task that pane is linked to now.
//!
//! It starts outside a pane too, as it does for an agent the user started elsewhere with
//! agentdeck in its own MCP config (`agentdeck mcp install`). A tool that needs the caller's
//! pane then fails saying it isn't in one.

use agentdeck_client::Client;
use agentdeck_client::types::{NewComment, NewTask, TaskPatch};
use agentdeck_core::{PaneId, TaskId, TaskPlace, TaskStatus, WorkspaceId};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::schemars::JsonSchema;
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};

use crate::commands::{self, caller_pane, caller_workspace};
use agentdeck_client::ApiError;
use agentdeck_client::daemon;

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

/// Why a tool that needs the caller's pane can't run: an agent started outside agentdeck,
/// with agentdeck in its own MCP config (spec §10).
const NOT_IN_A_PANE: &str = "you aren't in an agentdeck pane (no $AGENTDECK_PANE)";

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
            workspace: caller_workspace(),
            pane: caller_pane(),
        }
    }
}

/// Which workspace a list or create tool works on.
#[derive(Debug, PartialEq, Eq)]
enum Scope<'a> {
    /// Every workspace and the inbox.
    All,
    /// Only tasks with no workspace.
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
    /// The task's id. Defaults to the task your pane is linked to.
    id: Option<String>,
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
    /// The task's id. Defaults to the task your pane is linked to.
    id: Option<String>,
    /// A new title.
    title: Option<String>,
    /// A new body.
    body: Option<String>,
    /// A new status: todo, doing, review or done.
    status: Option<String>,
    /// A workspace to move it to (id, slug or name).
    workspace: Option<String>,
    /// Move it to the inbox, out of any workspace.
    #[serde(default)]
    inbox: bool,
}

/// `task_comments`' arguments.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct TaskComments {
    /// The task's id. Defaults to the task your pane is linked to.
    task: Option<String>,
}

/// `task_comment`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct TaskComment {
    /// The task's id. Defaults to the task your pane is linked to.
    task: Option<String>,
    /// The text: progress, what blocks you, a PR you opened, a summary when done.
    body: String,
}

/// `instructions_list`'s arguments.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct InstructionsList {
    /// The workspace (id, slug or name). Defaults to your workspace.
    workspace: Option<String>,
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

    /// The task `reference` names, else the one the caller's pane is linked to now. `arg` is
    /// the argument that names it, for the error asking for one.
    async fn task(
        &self,
        client: &Client,
        reference: Option<&str>,
        arg: &str,
    ) -> anyhow::Result<TaskId> {
        let task = parse_opt("task id", reference)?;
        commands::task_or_callers(client, task, self.caller.pane, &format!("`{arg}`")).await
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

    #[tool(
        description = "Get one task, with its pull request when it has one. Defaults to the \
                          task your pane is linked to."
    )]
    async fn task_get(&self, Parameters(args): Parameters<TaskRef>) -> ToolResult {
        respond(async {
            let client = daemon::connect().await?.client;
            let id = self.task(&client, args.id.as_deref(), "id").await?;
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
        description = "Change a task's title, body, status (todo, doing, review, done) or \
                          workspace; set `inbox` to move it out of any workspace. A PR task's \
                          title is its PR's. Defaults to the task your pane is linked to."
    )]
    async fn task_update(&self, Parameters(args): Parameters<TaskUpdate>) -> ToolResult {
        respond(async {
            let client = daemon::connect().await?.client;
            let id = self.task(&client, args.id.as_deref(), "id").await?;
            let workspace_id = match (args.workspace.as_deref(), args.inbox) {
                (Some(_), true) => anyhow::bail!("give either a workspace or inbox, not both"),
                (Some(reference), false) => Some(TaskPlace::Workspace(
                    commands::workspace_id(&client, reference).await?,
                )),
                (None, true) => Some(TaskPlace::Inbox),
                (None, false) => None,
            };
            let patch = TaskPatch {
                body: args.body,
                status: parse_opt("status", args.status.as_deref())?,
                title: args.title,
                workspace_id,
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
        description = "A task's comments, oldest first: the user's and the agents'. Defaults \
                          to the task your pane is linked to."
    )]
    async fn task_comments(&self, Parameters(args): Parameters<TaskComments>) -> ToolResult {
        respond(async {
            let client = daemon::connect().await?.client;
            let task = self.task(&client, args.task.as_deref(), "task").await?;
            Ok(client
                .comment_list()
                .id(task)
                .send()
                .await
                .map_err(ApiError::from)?
                .into_inner())
        })
        .await
    }

    #[tool(
        description = "Comment on a task, as your pane's agent: progress, what blocks you, a PR \
                          you opened, a summary when done. The user reads it; it isn't sent to \
                          any agent. Defaults to the task your pane is linked to."
    )]
    async fn task_comment(&self, Parameters(args): Parameters<TaskComment>) -> ToolResult {
        respond(async {
            // Without a pane the comment would be the user's, and go to the task's agent.
            if self.caller.pane.is_none() {
                anyhow::bail!("{NOT_IN_A_PANE}, so a comment from here would show as the user's");
            }
            let client = daemon::connect().await?.client;
            let task = self.task(&client, args.task.as_deref(), "task").await?;
            let new = NewComment {
                body: args.body,
                pane: self.caller.pane,
            };
            Ok(client
                .comment_create()
                .id(task)
                .body(new)
                .send()
                .await
                .map_err(ApiError::from)?
                .into_inner())
        })
        .await
    }

    #[tool(
        description = "The standing instructions for every agent in a workspace, oldest first. \
                          Defaults to your workspace."
    )]
    async fn instructions_list(
        &self,
        Parameters(args): Parameters<InstructionsList>,
    ) -> ToolResult {
        respond(async {
            let client = daemon::connect().await?.client;
            let reference = args
                .workspace
                .as_deref()
                .or(self.caller.workspace.as_deref());
            let Some(reference) = reference else {
                anyhow::bail!("name a workspace: you aren't in one");
            };
            let workspace = commands::workspace_id(&client, reference).await?;
            Ok(client
                .instruction_list()
                .id(workspace)
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
                "agentdeck's tasks, task comments, workspace instructions, workspaces and PR \
                 status. Tools default to the workspace and pane you run in, and to the task \
                 that pane is linked to.",
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
                    "instructions_list",
                    "prs_status",
                    "task_comment",
                    "task_comments",
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
