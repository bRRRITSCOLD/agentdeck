//! Launching agents (spec §6, §9): the argv each agent CLI starts and resumes with, and the
//! launch prompt that carries a pane's task, its workspace's instructions and the task's
//! comments waiting for it.
//!
//! - [`codex`]: finding the session id a Codex pane's CLI chose.
//! - [`delivery`]: typing the user's new task comment into the task's live agent pane.
//! - [`restart`]: starting an exited pane again, afresh once when its resume fails at once.
//! - [`resume`]: re-spawning the panes that were alive when the daemon last stopped.

pub mod codex;
pub mod delivery;
pub mod restart;
pub mod resume;

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

/// What an agent is told about `task` (spec §6, §9): its title, its id and how to report back
/// on it, then its body after a blank line when it has one. It opens a launch prompt, and is
/// typed into an agent whose pane is linked to the task later, or that starts in a shell pane
/// linked to it.
pub fn task_brief(task: &Task) -> String {
    let id = task.id;
    let header = format!(
        "Task: {}\nTask id: {id}\n\
         Report back with the agentdeck CLI: `agentdeck comment add --task {id} <text>` to \
         comment on this task, `agentdeck task set {id} --status <status>` to set its status.",
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

/// How a new pane of `agent` starts, with `prompt` as its first prompt. Claude Code starts on
/// a session id agentdeck chooses; Codex chooses its own, which [`codex`] finds afterwards.
/// `other` panes run their argv as given, so they have none here.
pub fn launch(agent: Agent, prompt: Option<String>) -> Option<AgentLaunch> {
    let mut argv = vec![agent.as_str().to_owned()];
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
/// command on its session. `None` (a fresh shell) for every other pane, and for an agent pane
/// whose session id was never known.
pub fn resume_argv(agent: Option<Agent>, session_id: Option<&str>) -> Option<Vec<String>> {
    let session_id = session_id?.to_owned();
    let agent = agent?;
    let resume = match agent {
        Agent::Claude => "--resume",
        Agent::Codex => "resume",
        Agent::Other => return None,
    };
    Some(vec![
        agent.as_str().to_owned(),
        resume.to_owned(),
        session_id,
    ])
}

#[cfg(test)]
mod tests {
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
                 Report back with the agentdeck CLI: `agentdeck comment add --task {id} \
                 <text>` to comment on this task, `agentdeck task set {id} --status <status>` \
                 to set its status.",
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

    mod launch {
        use super::*;

        #[test]
        fn should_start_claude_on_a_session_it_chooses_with_the_prompt() {
            let launch = launch(Agent::Claude, Some("Task: x".into())).unwrap();

            let session_id = launch.session_id.expect("a session id");
            assert!(Uuid::parse_str(&session_id).is_ok(), "{session_id}");
            assert_eq!(
                launch.argv,
                ["claude", "--session-id", session_id.as_str(), "Task: x"]
            );
        }

        #[test]
        fn should_choose_a_new_session_each_time() {
            let first = launch(Agent::Claude, None).unwrap();
            let second = launch(Agent::Claude, None).unwrap();

            assert_ne!(first.session_id, second.session_id);
            assert_eq!(first.argv.len(), 3, "no prompt argument: {:?}", first.argv);
        }

        #[test]
        fn should_start_codex_with_the_prompt() {
            assert_eq!(
                launch(Agent::Codex, Some("Task: x".into())).unwrap(),
                AgentLaunch {
                    argv: vec!["codex".to_owned(), "Task: x".to_owned()],
                    session_id: None,
                }
            );
        }

        #[test]
        fn should_leave_other_agents_to_their_own_argv() {
            assert_eq!(launch(Agent::Other, None), None);
        }
    }

    mod resume_argv {
        use super::*;

        #[test]
        fn should_resume_claude_on_its_session() {
            assert_eq!(
                resume_argv(Some(Agent::Claude), Some("s-1")).unwrap(),
                ["claude", "--resume", "s-1"]
            );
        }

        #[test]
        fn should_resume_codex_on_its_session() {
            assert_eq!(
                resume_argv(Some(Agent::Codex), Some("s-1")).unwrap(),
                ["codex", "resume", "s-1"]
            );
        }

        #[test]
        fn should_give_a_shell_to_an_agent_without_a_session() {
            assert_eq!(resume_argv(Some(Agent::Codex), None), None);
        }

        #[test]
        fn should_give_a_shell_to_other_agents_and_shells() {
            assert_eq!(resume_argv(Some(Agent::Other), Some("s-1")), None);
            assert_eq!(resume_argv(None, None), None);
        }
    }
}
