//! The command line (spec §10), as clap types.

use std::path::PathBuf;

use agentdeck_core::{Agent, NoteId, PaneId, TaskId, TaskStatus};
use clap::{Args, Parser, Subcommand};

/// `agentdeck`: drive agentdeck's workspaces, tasks, notes and agents from a shell or an agent.
#[derive(Debug, Parser)]
#[command(name = "agentdeck", version, about, propagate_version = true)]
pub struct Cli {
    /// Print machine-readable JSON instead of text.
    #[arg(long, global = true)]
    pub json: bool,
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// The top-level commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Workspaces.
    #[command(subcommand)]
    Ws(WsCommand),
    /// Tasks.
    #[command(subcommand)]
    Task(TaskCommand),
    /// Notes left for agents.
    #[command(subcommand)]
    Note(NoteCommand),
    /// Agent panes.
    #[command(subcommand)]
    Agent(AgentCommand),
    /// GitHub pull request sync.
    #[command(subcommand)]
    Pr(PrCommand),
    /// Print a pane's recent raw output.
    Tail {
        /// The pane's id.
        pane: PaneId,
        /// Only the last N bytes.
        #[arg(long, value_name = "N")]
        bytes: Option<u64>,
    },
    /// Serve MCP over stdin and stdout.
    Mcp,
}

/// `agentdeck ws …`. A `<ws>` is a workspace's id, slug or name.
#[derive(Debug, Subcommand)]
pub enum WsCommand {
    /// List every workspace, archived ones included.
    Ls,
    /// Create a workspace on an existing dir, or on a new scratch dir.
    New {
        /// The display name.
        name: String,
        /// An existing directory to attach.
        #[arg(long, value_name = "PATH")]
        dir: Option<PathBuf>,
    },
    /// Archive a workspace; its data is kept.
    Archive {
        /// The workspace.
        ws: String,
    },
    /// Delete a workspace, and its dir when agentdeck created it.
    Rm {
        /// The workspace.
        ws: String,
        /// Delete even when its dir holds uncommitted or unpushed work.
        #[arg(long)]
        force: bool,
    },
}

/// `agentdeck task …`.
#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    /// List tasks.
    Ls {
        /// Only tasks with this status: todo, doing, review or done.
        #[arg(long, value_name = "S")]
        status: Option<TaskStatus>,
        /// Only tasks in this workspace.
        #[arg(long, value_name = "WS")]
        ws: Option<String>,
        /// Only tasks with no workspace.
        #[arg(long, conflicts_with = "ws")]
        inbox: bool,
    },
    /// Create a task.
    Add {
        /// The title.
        title: String,
        /// The workspace; the inbox without it.
        #[arg(long, value_name = "WS")]
        ws: Option<String>,
        /// The body.
        #[arg(long, value_name = "B")]
        body: Option<String>,
    },
    /// Change a task's status.
    Set {
        /// The task's id.
        id: TaskId,
        /// The new status: todo, doing, review or done.
        #[arg(long, value_name = "S")]
        status: TaskStatus,
    },
    /// Show one task.
    Show {
        /// The task's id.
        id: TaskId,
    },
}

/// `agentdeck note …`.
#[derive(Debug, Subcommand)]
pub enum NoteCommand {
    /// List notes.
    Ls {
        /// Only notes on this workspace.
        #[arg(long, value_name = "WS", conflicts_with = "task")]
        ws: Option<String>,
        /// Only notes on this task.
        #[arg(long, value_name = "T")]
        task: Option<TaskId>,
        /// Only unread notes.
        #[arg(long)]
        unread: bool,
    },
    /// Leave a note on a workspace or a task.
    Add {
        /// The message.
        body: String,
        /// Where the note goes.
        #[command(flatten)]
        on: NoteTarget,
    },
    /// Print a note and mark it read (by `$AGENTDECK_PANE`, when set).
    Read {
        /// The note's id.
        id: NoteId,
    },
}

/// Exactly one of `--ws` and `--task`.
#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub struct NoteTarget {
    /// The workspace to leave it on.
    #[arg(long, value_name = "WS")]
    pub ws: Option<String>,
    /// The task to leave it on.
    #[arg(long, value_name = "T")]
    pub task: Option<TaskId>,
}

/// `agentdeck agent …`.
#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    /// Launch an agent in a workspace.
    New {
        /// The workspace.
        #[arg(long, value_name = "WS")]
        ws: String,
        /// Which agent CLI.
        #[arg(long, default_value = "claude", value_parser = parse_launchable_agent)]
        agent: Agent,
        /// Where it runs; the workspace's dir without it.
        #[arg(long, value_name = "DIR")]
        cwd: Option<PathBuf>,
        /// A task to link; its title, body and unread notes become the launch prompt.
        #[arg(long, value_name = "T")]
        task: Option<TaskId>,
    },
    /// Type a message into a pane and press Enter, once the pane is idle.
    Send {
        /// The pane's id.
        pane: PaneId,
        /// The message.
        text: String,
        /// Type it at once instead of waiting for the pane to be idle.
        #[arg(long)]
        now: bool,
    },
}

/// `agentdeck pr …`.
#[derive(Debug, Subcommand)]
pub enum PrCommand {
    /// Sync PR tasks with GitHub now.
    Sync,
    /// The `gh` login state and the last sync.
    Status,
}

/// `--agent` takes the agents `agent new` launches: `claude` or `codex`.
fn parse_launchable_agent(value: &str) -> Result<Agent, String> {
    match value.parse::<Agent>() {
        Ok(agent @ (Agent::Claude | Agent::Codex)) => Ok(agent),
        _ => Err(format!("`{value}` isn't claude or codex")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("agentdeck").chain(args.iter().copied()))
    }

    const TASK: &str = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";

    mod cli {
        use super::*;

        #[test]
        fn should_be_a_valid_clap_command() {
            use clap::CommandFactory;
            Cli::command().debug_assert();
        }

        #[test]
        fn should_take_json_after_the_subcommand() {
            let cli = parse(&["ws", "ls", "--json"]).unwrap();

            assert!(cli.json);
            assert!(matches!(cli.command, Command::Ws(WsCommand::Ls)));
        }
    }

    mod task {
        use super::*;

        #[test]
        fn should_parse_the_status_filter() {
            let cli = parse(&["task", "ls", "--status", "review"]).unwrap();

            assert!(matches!(
                cli.command,
                Command::Task(TaskCommand::Ls {
                    status: Some(TaskStatus::Review),
                    ..
                })
            ));
        }

        #[test]
        fn should_reject_an_unknown_status() {
            assert!(parse(&["task", "set", TASK, "--status", "blocked"]).is_err());
        }

        #[test]
        fn should_reject_inbox_with_a_workspace() {
            assert!(parse(&["task", "ls", "--inbox", "--ws", "web"]).is_err());
        }

        #[test]
        fn should_require_a_status_to_set() {
            assert!(parse(&["task", "set", TASK]).is_err());
        }
    }

    mod note {
        use super::*;

        #[test]
        fn should_require_a_workspace_or_a_task_to_add() {
            assert!(parse(&["note", "add", "hello"]).is_err());
        }

        #[test]
        fn should_reject_both_a_workspace_and_a_task_to_add() {
            assert!(parse(&["note", "add", "hello", "--ws", "web", "--task", TASK]).is_err());
        }

        #[test]
        fn should_add_to_a_task() {
            let cli = parse(&["note", "add", "hello", "--task", TASK]).unwrap();

            let Command::Note(NoteCommand::Add { body, on }) = cli.command else {
                panic!("not note add: {:?}", cli.command);
            };
            assert_eq!(body, "hello");
            assert_eq!(on.task, Some(TASK.parse().unwrap()));
            assert_eq!(on.ws, None);
        }
    }

    mod agent {
        use super::*;

        #[test]
        fn should_launch_claude_by_default() {
            let cli = parse(&["agent", "new", "--ws", "web"]).unwrap();

            assert!(matches!(
                cli.command,
                Command::Agent(AgentCommand::New {
                    agent: Agent::Claude,
                    ..
                })
            ));
        }

        #[test]
        fn should_reject_an_agent_it_cant_launch() {
            assert!(parse(&["agent", "new", "--ws", "web", "--agent", "other"]).is_err());
        }

        #[test]
        fn should_wait_for_idle_unless_now() {
            let cli = parse(&["agent", "send", TASK, "hi"]).unwrap();

            assert!(matches!(
                cli.command,
                Command::Agent(AgentCommand::Send { now: false, .. })
            ));
        }
    }
}
