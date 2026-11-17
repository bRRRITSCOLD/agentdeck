//! The command line (spec §10), as clap types.

use std::path::PathBuf;

use agentdeck_core::{Agent, InstructionId, TaskStatus};
use clap::{ArgGroup, Parser, Subcommand};

/// `agentdeck`: drive agentdeck's workspaces, tasks, comments, instructions and agents from a
/// shell or an agent.
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
    /// Comments on a task: yours reach its agent, an agent's report back.
    #[command(subcommand)]
    Comment(CommentCommand),
    /// Standing instructions for every agent in a workspace.
    #[command(subcommand)]
    Instruction(InstructionCommand),
    /// Agent panes.
    #[command(subcommand)]
    Agent(AgentCommand),
    /// GitHub pull request sync.
    #[command(subcommand)]
    Pr(PrCommand),
    /// Settings, kept in config.toml; a change applies at once.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// agentdeck's background service, agentdeckd.
    #[command(subcommand)]
    Daemon(DaemonCommand),
    /// Print a pane's recent raw output.
    Tail {
        /// The pane: its id or a short id.
        pane: String,
        /// Only the last N bytes.
        #[arg(long, value_name = "N")]
        bytes: Option<u64>,
    },
    /// Serve MCP over stdin and stdout.
    Mcp,
}

/// `agentdeck daemon …`.
#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Stop agentdeckd and start it again, as after an update; running agents resume.
    Restart,
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
    /// Rename a workspace; its slug and dir stay, so its old slug still finds it.
    Rename {
        /// The workspace.
        ws: String,
        /// The new display name.
        name: String,
    },
    /// Archive a workspace; its data is kept.
    Archive {
        /// The workspace.
        ws: String,
    },
    /// Stop the GUI's notifications for a workspace's agents and PRs.
    Mute {
        /// The workspace.
        ws: String,
    },
    /// Let the GUI notify about a muted workspace again.
    Unmute {
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

/// `agentdeck task …`. A task's id may be a short id: 6 or more of its hex digits in a row,
/// such as its last 8.
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
        /// The task: its id or a short id.
        id: String,
        /// The new status: todo, doing, review or done.
        #[arg(long, value_name = "S")]
        status: TaskStatus,
    },
    /// Change a task's title, body or workspace. A PR task's title is its PR's.
    #[command(group(
        ArgGroup::new("change").required(true).multiple(true).args(["title", "body", "ws", "inbox"])
    ))]
    Edit {
        /// The task: its id or a short id.
        id: String,
        /// A new title.
        #[arg(long, value_name = "T")]
        title: Option<String>,
        /// A new body.
        #[arg(long, value_name = "B")]
        body: Option<String>,
        /// Move it to this workspace.
        #[arg(long, value_name = "WS")]
        ws: Option<String>,
        /// Move it to the inbox.
        #[arg(long, conflicts_with = "ws")]
        inbox: bool,
    },
    /// Show one task.
    Show {
        /// The task: its id or a short id.
        id: String,
    },
}

/// `agentdeck comment …`. Run inside a pane (`$AGENTDECK_PANE` set), a comment is that
/// pane's agent's; otherwise it is yours, and goes to the task's agent. Without `--task`, the
/// task is the one the pane is linked to now.
#[derive(Debug, Subcommand)]
pub enum CommentCommand {
    /// List a task's comments, oldest first.
    Ls {
        /// The task: its id or a short id. Defaults to the one your pane is linked to.
        #[arg(long, value_name = "T")]
        task: Option<String>,
    },
    /// Add a comment to a task.
    Add {
        /// The text.
        body: String,
        /// The task: its id or a short id. Defaults to the one your pane is linked to.
        #[arg(long, value_name = "T")]
        task: Option<String>,
    },
}

/// `agentdeck instruction …`. Without `--ws`, the workspace is `$AGENTDECK_WORKSPACE`.
#[derive(Debug, Subcommand)]
pub enum InstructionCommand {
    /// List a workspace's instructions, oldest first.
    Ls {
        /// The workspace.
        #[arg(long, value_name = "WS")]
        ws: Option<String>,
    },
    /// Add an instruction; every launch prompt in the workspace carries it.
    Add {
        /// The text.
        body: String,
        /// The workspace.
        #[arg(long, value_name = "WS")]
        ws: Option<String>,
    },
    /// Replace an instruction's text.
    Edit {
        /// The instruction's id.
        id: InstructionId,
        /// The new text.
        body: String,
    },
    /// Delete an instruction.
    Rm {
        /// The instruction's id.
        id: InstructionId,
    },
}

/// `agentdeck agent …`. A pane or task id may be a short id: 6 or more of its hex digits in a
/// row, such as its last 8.
#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    /// List every pane, agent and shell, exited ones too: its id, workspace, tab, agent,
    /// state and linked task.
    Ls {
        /// Only panes in this workspace.
        #[arg(long, value_name = "WS")]
        ws: Option<String>,
    },
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
        /// A task to link; its title, body and new comments go into the launch prompt.
        #[arg(long, value_name = "T")]
        task: Option<String>,
    },
    /// Link a running pane to a task in its workspace, or unlink it. The pane's agent is
    /// told the task, then the task's comments no agent has had yet, once it is idle.
    #[command(group(ArgGroup::new("link").required(true).args(["task", "none"])))]
    Link {
        /// The pane: its id or a short id.
        pane: String,
        /// The task to link it to.
        #[arg(long, value_name = "T")]
        task: Option<String>,
        /// Unlink it from its task.
        #[arg(long)]
        none: bool,
    },
    /// Type a message into a pane and press Enter, once the pane is idle.
    Send {
        /// The pane: its id or a short id.
        pane: String,
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
    /// Whether GitHub takes the token, and the last sync.
    Status,
    /// Close the PRs of PR tasks on GitHub, after asking. Closes only: no comment, no branch
    /// delete.
    Close {
        /// The PR tasks: their ids or short ids.
        #[arg(required = true, value_name = "ID")]
        ids: Vec<String>,
        /// Close them without asking.
        #[arg(long)]
        yes: bool,
    },
}

/// `agentdeck config …`. A `<key>` is dotted: `appearance.theme`, `pr_sync.max_age_days`.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// List every setting with its value.
    Ls,
    /// Print one setting's value.
    Get {
        /// The setting, as `appearance.theme`.
        key: String,
    },
    /// Change a setting and save it in config.toml.
    Set {
        /// The setting, as `pr_sync.max_age_days`.
        key: String,
        /// The new value: a number, `true` or `false`, or else text.
        value: String,
    },
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

        #[test]
        fn should_edit_a_title_and_a_workspace() {
            let cli = parse(&["task", "edit", TASK, "--title", "t", "--ws", "web"]).unwrap();

            let Command::Task(TaskCommand::Edit {
                title, ws, inbox, ..
            }) = cli.command
            else {
                panic!("not task edit: {:?}", cli.command);
            };
            assert_eq!(title.as_deref(), Some("t"));
            assert_eq!(ws.as_deref(), Some("web"));
            assert!(!inbox);
        }

        #[test]
        fn should_need_something_to_edit() {
            assert!(parse(&["task", "edit", TASK]).is_err());
        }

        #[test]
        fn should_not_move_to_a_workspace_and_the_inbox_at_once() {
            assert!(parse(&["task", "edit", TASK, "--ws", "web", "--inbox"]).is_err());
        }
    }

    mod comment {
        use super::*;

        #[test]
        fn should_leave_the_task_to_the_callers_pane_when_not_given() {
            let cli = parse(&["comment", "add", "hello"]).unwrap();

            let Command::Comment(CommentCommand::Add { task, .. }) = cli.command else {
                panic!("not comment add: {:?}", cli.command);
            };
            assert_eq!(task, None);
            assert!(parse(&["comment", "ls"]).is_ok());
        }

        #[test]
        fn should_add_to_a_task() {
            let cli = parse(&["comment", "add", "hello", "--task", TASK]).unwrap();

            let Command::Comment(CommentCommand::Add { body, task }) = cli.command else {
                panic!("not comment add: {:?}", cli.command);
            };
            assert_eq!(body, "hello");
            assert_eq!(task.as_deref(), Some(TASK));
        }
    }

    mod instruction {
        use super::*;

        #[test]
        fn should_take_the_workspace_as_optional_to_add() {
            let cli = parse(&["instruction", "add", "hello"]).unwrap();

            assert!(matches!(
                cli.command,
                Command::Instruction(InstructionCommand::Add { ws: None, .. })
            ));
        }

        #[test]
        fn should_edit_by_id_with_a_new_body() {
            let cli = parse(&["instruction", "edit", TASK, "new text"]).unwrap();

            let Command::Instruction(InstructionCommand::Edit { id, body }) = cli.command else {
                panic!("not instruction edit: {:?}", cli.command);
            };
            assert_eq!(id, TASK.parse().unwrap());
            assert_eq!(body, "new text");
        }

        #[test]
        fn should_reject_an_id_that_is_not_one() {
            assert!(parse(&["instruction", "rm", "nope"]).is_err());
        }
    }

    mod config {
        use super::*;

        #[test]
        fn should_set_a_dotted_key_to_a_value() {
            let cli = parse(&["config", "set", "pr_sync.max_age_days", "30"]).unwrap();

            let Command::Config(ConfigCommand::Set { key, value }) = cli.command else {
                panic!("not config set: {:?}", cli.command);
            };
            assert_eq!(key, "pr_sync.max_age_days");
            assert_eq!(value, "30");
        }

        #[test]
        fn should_require_a_value_to_set() {
            assert!(parse(&["config", "set", "appearance.theme"]).is_err());
        }
    }

    mod agent {
        use super::*;

        #[test]
        fn should_list_every_pane_or_one_workspaces() {
            let all = parse(&["agent", "ls"]).unwrap();
            assert!(matches!(
                all.command,
                Command::Agent(AgentCommand::Ls { ws: None })
            ));

            let one = parse(&["agent", "ls", "--ws", "web", "--json"]).unwrap();
            let Command::Agent(AgentCommand::Ls { ws }) = one.command else {
                panic!("not agent ls: {:?}", one.command);
            };
            assert_eq!(ws.as_deref(), Some("web"));
            assert!(one.json);
        }

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
        fn should_link_to_a_task() {
            let cli = parse(&["agent", "link", TASK, "--task", TASK]).unwrap();

            let Command::Agent(AgentCommand::Link { task, none, .. }) = cli.command else {
                panic!("not agent link: {:?}", cli.command);
            };
            assert_eq!(task.as_deref(), Some(TASK));
            assert!(!none);
        }

        #[test]
        fn should_unlink_with_none() {
            let cli = parse(&["agent", "link", TASK, "--none"]).unwrap();

            assert!(matches!(
                cli.command,
                Command::Agent(AgentCommand::Link {
                    task: None,
                    none: true,
                    ..
                })
            ));
        }

        #[test]
        fn should_need_exactly_one_of_task_and_none_to_link() {
            assert!(parse(&["agent", "link", TASK]).is_err());
            assert!(parse(&["agent", "link", TASK, "--task", TASK, "--none"]).is_err());
        }

        #[test]
        fn should_close_several_prs_and_take_yes() {
            let id = "019a1dd1-5b85-75ae-af0b-6d1a8b211cc0";
            let cli = parse(&["pr", "close", id, id, "--yes"]).unwrap();

            let Command::Pr(PrCommand::Close { ids, yes }) = cli.command else {
                panic!("not pr close: {cli:?}");
            };
            assert_eq!(ids.len(), 2);
            assert!(yes);
        }

        #[test]
        fn should_need_a_task_to_close() {
            assert!(parse(&["pr", "close"]).is_err());
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
