//! The command line (spec §10), as clap types.

use std::path::PathBuf;

use agentdeck_core::{Agent, PrRef, RepositoryName};
use clap::{Parser, Subcommand};

/// `agentdeck`: drive agentdeck's workspaces, agents, Repositories and pull requests
/// from a shell or an agent.
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
    /// Agent panes.
    #[command(subcommand)]
    Agent(AgentCommand),
    /// Repositories, and linking them to workspaces.
    #[command(subcommand)]
    Repo(RepoCommand),
    /// Your GitHub pull requests, and syncing them.
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

/// `agentdeck agent …`. A pane id may be a short id: 6 or more of its hex digits in a row,
/// such as its last 8.
#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    /// List every pane, agent and shell, exited ones too: its id, workspace, tab, agent,
    /// state and current folder.
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
        /// Where it runs; the current folder without it.
        #[arg(long, value_name = "DIR")]
        cwd: Option<PathBuf>,
    },
    /// Stop a pane's agent and keep the pane: Restart agent in the app resumes it. In a shell
    /// pane, only the agent it hosts stops; the shell keeps running.
    Stop {
        /// The pane: its id or a short id.
        pane: String,
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

/// `agentdeck repo …`. A Repository is `owner/name` or its GitHub URL; a `WS` is a
/// workspace's id, slug or name. No command reads `$AGENTDECK_WORKSPACE`: pass
/// `--ws "$AGENTDECK_WORKSPACE"` yourself.
#[derive(Debug, Subcommand)]
pub enum RepoCommand {
    /// List the Repositories agentdeck keeps, with the workspaces linking each.
    Ls {
        /// Only the Repositories this workspace links.
        #[arg(long, value_name = "WS")]
        ws: Option<String>,
    },
    /// Link a Repository to a workspace, so its pull requests show there. One agentdeck
    /// doesn't know yet is looked up on GitHub first.
    Link {
        /// The Repository: `owner/name` or its GitHub URL.
        repo: RepositoryName,
        /// The workspace.
        #[arg(long, value_name = "WS")]
        ws: String,
    },
    /// Unlink a Repository from a workspace. agentdeck forgets the Repository when no other
    /// workspace links it and none of your pull requests belongs to it.
    Unlink {
        /// The Repository: `owner/name` or its GitHub URL.
        repo: RepositoryName,
        /// The workspace.
        #[arg(long, value_name = "WS")]
        ws: String,
    },
}

/// `agentdeck pr …`. A pull request is `owner/name#123` or its GitHub URL; a Repository is
/// `owner/name` or its GitHub URL.
#[derive(Debug, Subcommand)]
pub enum PrCommand {
    /// List the pull requests agentdeck keeps that aren't Finished, with their state.
    Ls {
        /// Only this Repository's: `owner/name` or its GitHub URL.
        #[arg(long, value_name = "REPO")]
        repo: Option<RepositoryName>,
        /// Only those of the Repositories this workspace links.
        #[arg(long, value_name = "WS")]
        ws: Option<String>,
    },
    /// Sync your pull requests with GitHub now: a full sync, which reads every open PR again.
    Sync,
    /// Whether GitHub takes the token, and the last sync.
    Status,
    /// Close pull requests on GitHub, after asking. Closes only: no comment, no branch delete.
    Close {
        /// The pull requests: `owner/name#123` or their GitHub URLs.
        #[arg(required = true, value_name = "PR")]
        prs: Vec<PrRef>,
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

    const PANE: &str = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";

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
        fn should_stop_a_pane() {
            let cli = parse(&["agent", "stop", PANE]).unwrap();

            let Command::Agent(AgentCommand::Stop { pane }) = cli.command else {
                panic!("not agent stop: {:?}", cli.command);
            };
            assert_eq!(pane, PANE);
        }

        #[test]
        fn should_need_a_pane_to_stop() {
            assert!(parse(&["agent", "stop"]).is_err());
        }

        #[test]
        fn should_close_prs_named_by_reference_or_url_and_take_yes() {
            let cli = parse(&[
                "pr",
                "close",
                "acme/widgets#7",
                "https://github.com/acme/gadgets/pull/9",
                "--yes",
            ])
            .unwrap();

            let Command::Pr(PrCommand::Close { prs, yes }) = cli.command else {
                panic!("not pr close: {cli:?}");
            };
            let prs: Vec<String> = prs.iter().map(ToString::to_string).collect();
            assert_eq!(prs, ["acme/widgets#7", "acme/gadgets#9"]);
            assert!(yes);
        }

        #[test]
        fn should_need_a_pr_to_close() {
            assert!(parse(&["pr", "close"]).is_err());
        }

        #[test]
        fn should_refuse_to_close_what_names_no_pull_request() {
            let err = parse(&["pr", "close", "019a1dd1-5b85-75ae-af0b-6d1a8b211cc0"]).unwrap_err();

            assert!(err.to_string().contains("isn't a pull request"), "{err}");
        }

        #[test]
        fn should_list_one_repository_named_by_its_url() {
            let cli = parse(&["pr", "ls", "--repo", "https://github.com/acme/widgets"]).unwrap();

            let Command::Pr(PrCommand::Ls { repo, .. }) = cli.command else {
                panic!("not pr ls: {cli:?}");
            };
            assert_eq!(
                repo.map(|repo| repo.to_string()),
                Some("acme/widgets".into())
            );
        }

        #[test]
        fn should_list_one_workspaces_pull_requests() {
            let cli = parse(&["pr", "ls", "--ws", "web"]).unwrap();

            let Command::Pr(PrCommand::Ls { ws, .. }) = cli.command else {
                panic!("not pr ls: {cli:?}");
            };
            assert_eq!(ws.as_deref(), Some("web"));
        }

        #[test]
        fn should_link_a_repository_named_by_its_url_to_a_workspace() {
            let cli = parse(&[
                "repo",
                "link",
                "https://github.com/acme/widgets",
                "--ws",
                "web",
            ])
            .unwrap();

            let Command::Repo(RepoCommand::Link { repo, ws }) = cli.command else {
                panic!("not repo link: {cli:?}");
            };
            assert_eq!(repo.to_string(), "acme/widgets");
            assert_eq!(ws, "web");
        }

        #[test]
        fn should_need_a_workspace_to_link_or_unlink() {
            assert!(parse(&["repo", "link", "acme/widgets"]).is_err());
            assert!(parse(&["repo", "unlink", "acme/widgets"]).is_err());
        }

        #[test]
        fn should_refuse_to_link_what_names_no_repository() {
            let err = parse(&["repo", "link", "widgets", "--ws", "web"]).unwrap_err();

            assert!(err.to_string().contains("isn't a Repository"), "{err}");
        }

        #[test]
        fn should_wait_for_idle_unless_now() {
            let cli = parse(&["agent", "send", PANE, "hi"]).unwrap();

            assert!(matches!(
                cli.command,
                Command::Agent(AgentCommand::Send { now: false, .. })
            ));
        }
    }
}
