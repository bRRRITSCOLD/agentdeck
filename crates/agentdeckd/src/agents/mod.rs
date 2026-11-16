//! Launching agents (spec §6, §9): the argv each agent CLI starts and resumes with, which
//! registers `agentdeck mcp` as its `agentdeck` MCP server, and the launch prompt that carries
//! a pane's task, its workspace's instructions and the task's comments waiting for it.
//!
//! - [`codex`]: finding the session id a Codex pane's CLI chose.
//! - [`delivery`]: typing the user's new task comment into the task's live agent pane.
//! - [`resume`]: re-spawning the panes that were alive when the daemon last stopped.

pub mod codex;
pub mod delivery;
pub mod resume;

use std::path::{Path, PathBuf};

use agentdeck_core::Agent;
use uuid::Uuid;

use crate::model::{Comment, Instruction, Task};

/// How agentdeck starts an agent CLI.
#[derive(Debug, PartialEq, Eq)]
pub struct AgentLaunch {
    /// The command, prompt included.
    pub argv: Vec<String>,
    /// The session agentdeck chose for it, when the CLI takes one.
    pub session_id: Option<String>,
}

/// The variable a pane linked to a task at launch has its task's id in (spec §6).
pub const TASK_VAR: &str = "AGENTDECK_TASK";

/// What `agentdeck mcp` reads from its environment: where the daemon is, and who is calling.
/// Codex passes an MCP server only a few variables of its own (`HOME`, `PATH`, …) plus those
/// named in its `env_vars`; Claude Code passes its whole environment.
const MCP_ENV_VARS: &[&str] = &[
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "AGENTDECK_PANE",
    "AGENTDECK_WORKSPACE",
    TASK_VAR,
];

/// The `agentdeck` CLI the agents run as their MCP server: the one beside this daemon's
/// executable, else `agentdeck` from the pane's `PATH`, with a warning.
pub fn cli() -> PathBuf {
    cli_beside(std::env::current_exe().ok(), Path::is_file).unwrap_or_else(|| {
        tracing::warn!("no agentdeck beside agentdeckd; agents run `agentdeck mcp` from PATH");
        "agentdeck".into()
    })
}

/// The `agentdeck` in the same directory as `exe`, when it is a file.
fn cli_beside(exe: Option<PathBuf>, is_file: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    exe.and_then(|exe| Some(exe.parent()?.join("agentdeck")))
        .filter(|cli| is_file(cli))
}

/// The flags registering `<cli> mcp` as `agent`'s `agentdeck` MCP server, on top of the
/// user's own servers. Claude Code's `--mcp-config` takes every value up to the next flag, so
/// a flag must follow it, never the prompt.
fn mcp_flags(agent: Agent, cli: &Path) -> Vec<String> {
    let command = cli.to_string_lossy().into_owned();
    match agent {
        Agent::Claude => {
            // Written out rather than built as a `Value`: its key order would hang on whether
            // a feature elsewhere in the workspace turns serde_json's `preserve_order` on.
            let command = serde_json::Value::String(command);
            let config = format!(
                r#"{{"mcpServers":{{"agentdeck":{{"command":{command},"args":["mcp"]}}}}}}"#
            );
            vec!["--mcp-config".into(), config]
        }
        Agent::Codex => {
            let env_vars = MCP_ENV_VARS
                .iter()
                .map(|name| format!("\"{name}\""))
                .collect::<Vec<_>>()
                .join(", ");
            [
                format!(
                    "mcp_servers.agentdeck.command={}",
                    toml::Value::String(command)
                ),
                r#"mcp_servers.agentdeck.args=["mcp"]"#.to_owned(),
                format!("mcp_servers.agentdeck.env_vars=[{env_vars}]"),
            ]
            .into_iter()
            .flat_map(|value| ["-c".to_owned(), value])
            .collect()
        }
        Agent::Other => Vec::new(),
    }
}

/// What an agent is told about `task` (spec §6, §9): its title, its id and how to report back
/// on it, then its body after a blank line when it has one. It opens a launch prompt, and is
/// typed into an agent whose pane is linked to the task later, or that starts in a shell pane
/// linked to it.
pub fn task_brief(task: &Task) -> String {
    let id = task.id;
    let header = format!(
        "Task: {}\nTask id: {id}\n\
         Report back with the agentdeck MCP tools: task_comment to comment on this task, \
         task_update to set its status (or run `agentdeck comment add --task {id} <text>`).",
        task.title
    );
    match task.body.trim() {
        "" => header,
        body => format!("{header}\n\n{body}"),
    }
}

/// The launch prompt for an agent pane (spec §6): the linked task's title, id, how to report
/// back on it and body, then every instruction for the pane's workspace and the task's
/// comments no agent has had yet, oldest first. `None` when there is none of them. It never
/// starts with `-`, so the CLI can't read it as a flag.
pub fn launch_prompt(
    task: Option<&Task>,
    instructions: &[Instruction],
    comments: &[Comment],
) -> Option<String> {
    let mut parts = Vec::new();
    parts.extend(task.map(task_brief));
    let blocks: Vec<String> = [
        (
            "Instructions for this workspace:",
            bullets(instructions.iter().map(|i| &i.body)),
        ),
        (
            "Comments since your last session:",
            bullets(comments.iter().map(|c| &c.body)),
        ),
    ]
    .into_iter()
    .filter(|(_, list)| !list.is_empty())
    .map(|(heading, list)| format!("{heading}\n{list}"))
    .collect();
    if !blocks.is_empty() {
        parts.push(blocks.join("\n"));
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// One `  - <text>` line per item, continuation lines indented under the text.
fn bullets<'a>(items: impl Iterator<Item = &'a String>) -> String {
    items
        .map(|text| format!("  - {}", text.trim().replace('\n', "\n    ")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `agent`'s program and the flags registering `<cli> mcp` as its MCP server.
fn program(agent: Agent, cli: &Path) -> Vec<String> {
    let mut argv = vec![agent.as_str().to_owned()];
    argv.extend(mcp_flags(agent, cli));
    argv
}

/// How a new pane of `agent` starts, with `prompt` as its first prompt and `<cli> mcp` as its
/// `agentdeck` MCP server. Claude Code starts on a session id agentdeck chooses; Codex chooses
/// its own, which [`codex`] finds afterwards. `other` panes run their argv as given, so they
/// have none here.
pub fn launch(agent: Agent, prompt: Option<String>, cli: &Path) -> Option<AgentLaunch> {
    let mut argv = program(agent, cli);
    let session_id = match agent {
        Agent::Claude => {
            let session_id = Uuid::new_v4().hyphenated().to_string();
            argv.extend(["--session-id".into(), session_id.clone()]);
            Some(session_id)
        }
        Agent::Codex => None,
        Agent::Other => return None,
    };
    argv.extend(prompt);
    Some(AgentLaunch { argv, session_id })
}

/// What a pane that was alive when the daemon stopped resumes with: its agent's resume
/// command on its session, with `<cli> mcp` as its MCP server. `None` (a fresh shell) for
/// every other pane, and for an agent pane whose session id was never known.
pub fn resume_argv(
    agent: Option<Agent>,
    session_id: Option<&str>,
    cli: &Path,
) -> Option<Vec<String>> {
    let session_id = session_id?.to_owned();
    let agent = agent?;
    let resume = match agent {
        Agent::Claude => "--resume",
        Agent::Codex => "resume",
        Agent::Other => return None,
    };
    let mut argv = program(agent, cli);
    argv.extend([resume.to_owned(), session_id]);
    Some(argv)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use agentdeck_core::{CommentAuthor, TaskKind, TaskStatus};

    use super::*;

    fn task(title: &str, body: &str) -> Task {
        Task {
            id: uuid::Uuid::now_v7().into(),
            title: title.into(),
            body: body.into(),
            status: TaskStatus::Todo,
            kind: TaskKind::Plain,
            workspace_id: None,
            created_at: String::new(),
            updated_at: String::new(),
            done_at: None,
            pr: None,
            unseen_agent_comments: 0,
        }
    }

    fn instruction(body: &str) -> Instruction {
        Instruction {
            id: uuid::Uuid::now_v7().into(),
            workspace_id: uuid::Uuid::now_v7().into(),
            body: body.into(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn comment(body: &str) -> Comment {
        Comment {
            id: uuid::Uuid::now_v7().into(),
            task_id: uuid::Uuid::now_v7().into(),
            body: body.into(),
            author: CommentAuthor::User,
            author_agent: None,
            author_pane: None,
            created_at: String::new(),
            delivered_at: None,
            delivered_to_pane: None,
            seen_at: None,
        }
    }

    mod launch_prompt {
        use super::*;

        /// The prompt's first lines for `task`, written out in full.
        fn header(task: &Task) -> String {
            let id = task.id;
            format!(
                "Task: {}\nTask id: {id}\n\
                 Report back with the agentdeck MCP tools: task_comment to comment on this \
                 task, task_update to set its status (or run \
                 `agentdeck comment add --task {id} <text>`).",
                task.title
            )
        }

        #[test]
        fn should_be_none_without_a_task_or_instructions() {
            assert_eq!(launch_prompt(None, &[], &[]), None);
        }

        #[test]
        fn should_carry_the_tasks_title_id_how_to_report_back_and_body() {
            let task = task("Fix the login bug", "It fails on Safari.\n");

            assert_eq!(
                launch_prompt(Some(&task), &[], &[]).unwrap(),
                format!("{}\n\nIt fails on Safari.", header(&task))
            );
        }

        #[test]
        fn should_leave_out_an_empty_body() {
            let task = task("Fix the login bug", "  ");

            assert_eq!(launch_prompt(Some(&task), &[], &[]).unwrap(), header(&task));
        }

        #[test]
        fn should_list_the_instructions_then_the_comments_after_the_task() {
            let task = task("Fix the login bug", "It fails on Safari.");
            let instructions = [instruction("Use the staging DB"), instruction("Two\nlines")];
            let comments = [comment("Repro in issue 7")];

            assert_eq!(
                launch_prompt(Some(&task), &instructions, &comments).unwrap(),
                format!(
                    "{}\n\nIt fails on Safari.\n\n\
                     Instructions for this workspace:\n  - Use the staging DB\n  - Two\n    lines\n\
                     Comments since your last session:\n  - Repro in issue 7",
                    header(&task)
                )
            );
        }

        #[test]
        fn should_leave_out_an_empty_block() {
            let task = task("Fix the login bug", "");

            assert_eq!(
                launch_prompt(Some(&task), &[], &[comment("Also Firefox")]).unwrap(),
                format!(
                    "{}\n\nComments since your last session:\n  - Also Firefox",
                    header(&task)
                )
            );
        }

        #[test]
        fn should_carry_instructions_alone_when_there_is_no_task() {
            assert_eq!(
                launch_prompt(None, &[instruction("hello")], &[]).unwrap(),
                "Instructions for this workspace:\n  - hello"
            );
        }

        #[test]
        fn should_never_start_with_a_dash() {
            let task = task("--dangerously-skip-permissions", "");

            assert!(
                !launch_prompt(Some(&task), &[], &[])
                    .unwrap()
                    .starts_with('-')
            );
        }
    }

    /// Where the tests' daemon finds `agentdeck`.
    const CLI: &str = "/opt/agentdeck/bin/agentdeck";

    /// The `--mcp-config` value registering [`CLI`] as Claude Code's `agentdeck` server.
    const CLAUDE_MCP: &str =
        r#"{"mcpServers":{"agentdeck":{"command":"/opt/agentdeck/bin/agentdeck","args":["mcp"]}}}"#;

    /// The `-c` values registering [`CLI`] as Codex's `agentdeck` server.
    fn codex_mcp() -> Vec<String> {
        [
            "-c",
            r#"mcp_servers.agentdeck.command="/opt/agentdeck/bin/agentdeck""#,
            "-c",
            r#"mcp_servers.agentdeck.args=["mcp"]"#,
            "-c",
            "mcp_servers.agentdeck.env_vars=[\"XDG_RUNTIME_DIR\", \"XDG_CONFIG_HOME\", \
             \"XDG_DATA_HOME\", \"XDG_STATE_HOME\", \"AGENTDECK_PANE\", \
             \"AGENTDECK_WORKSPACE\", \"AGENTDECK_TASK\"]",
        ]
        .map(String::from)
        .to_vec()
    }

    mod launch {
        use super::*;

        #[test]
        fn should_start_claude_on_a_session_it_chooses_with_the_mcp_server_and_the_prompt() {
            let launch = launch(Agent::Claude, Some("Task: x".into()), Path::new(CLI)).unwrap();

            let session_id = launch.session_id.expect("a session id");
            assert!(Uuid::parse_str(&session_id).is_ok(), "{session_id}");
            assert_eq!(
                launch.argv,
                [
                    "claude",
                    "--mcp-config",
                    CLAUDE_MCP,
                    "--session-id",
                    session_id.as_str(),
                    "Task: x"
                ]
            );
        }

        #[test]
        fn should_choose_a_new_session_each_time() {
            let first = launch(Agent::Claude, None, Path::new(CLI)).unwrap();
            let second = launch(Agent::Claude, None, Path::new(CLI)).unwrap();

            assert_ne!(first.session_id, second.session_id);
            assert_eq!(first.argv.len(), 5, "no prompt argument: {:?}", first.argv);
        }

        #[test]
        fn should_start_codex_with_the_mcp_server_and_the_prompt() {
            let mut argv = vec!["codex".to_owned()];
            argv.extend(codex_mcp());
            argv.push("Task: x".into());

            assert_eq!(
                launch(Agent::Codex, Some("Task: x".into()), Path::new(CLI)).unwrap(),
                AgentLaunch {
                    argv,
                    session_id: None,
                }
            );
        }

        #[test]
        fn should_leave_other_agents_to_their_own_argv() {
            assert_eq!(launch(Agent::Other, None, Path::new(CLI)), None);
        }
    }

    mod mcp_flags {
        use super::*;

        #[test]
        fn should_quote_a_path_for_claudes_json_and_codexs_toml() {
            let cli = Path::new(r#"/home/a "b"\c/agentdeck"#);

            let claude = mcp_flags(Agent::Claude, cli);
            let codex = mcp_flags(Agent::Codex, cli);

            let config: serde_json::Value = serde_json::from_str(&claude[1]).expect("JSON");
            assert_eq!(
                config["mcpServers"]["agentdeck"]["command"],
                cli.to_str().unwrap()
            );
            let (key, value) = codex[1].split_once('=').expect("key=value");
            assert_eq!(key, "mcp_servers.agentdeck.command");
            let parsed: toml::Table = toml::from_str(&format!("v = {value}")).expect("TOML");
            assert_eq!(parsed["v"].as_str(), cli.to_str());
        }
    }

    mod cli_beside {
        use super::*;

        #[test]
        fn should_be_the_agentdeck_beside_the_daemon() {
            assert_eq!(
                cli_beside(Some("/opt/bin/agentdeckd".into()), |_| true),
                Some(PathBuf::from("/opt/bin/agentdeck"))
            );
        }

        #[test]
        fn should_be_none_when_there_is_no_agentdeck_beside_it() {
            assert_eq!(
                cli_beside(Some("/opt/bin/agentdeckd".into()), |_| false),
                None
            );
            assert_eq!(cli_beside(None, |_| true), None);
        }
    }

    mod resume_argv {
        use super::*;

        #[test]
        fn should_resume_claude_on_its_session_with_the_mcp_server() {
            assert_eq!(
                resume_argv(Some(Agent::Claude), Some("s-1"), Path::new(CLI)).unwrap(),
                ["claude", "--mcp-config", CLAUDE_MCP, "--resume", "s-1"]
            );
        }

        #[test]
        fn should_resume_codex_on_its_session_with_the_mcp_server() {
            let mut argv = vec!["codex".to_owned()];
            argv.extend(codex_mcp());
            argv.extend(["resume".to_owned(), "s-1".to_owned()]);

            assert_eq!(
                resume_argv(Some(Agent::Codex), Some("s-1"), Path::new(CLI)).unwrap(),
                argv
            );
        }

        #[test]
        fn should_give_a_shell_to_an_agent_without_a_session() {
            assert_eq!(resume_argv(Some(Agent::Codex), None, Path::new(CLI)), None);
        }

        #[test]
        fn should_give_a_shell_to_other_agents_and_shells() {
            assert_eq!(
                resume_argv(Some(Agent::Other), Some("s-1"), Path::new(CLI)),
                None
            );
            assert_eq!(resume_argv(None, None, Path::new(CLI)), None);
        }
    }
}
