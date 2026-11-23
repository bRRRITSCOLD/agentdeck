//! Launching agents (spec §6): the argv each agent CLI starts and resumes with. agentdeck
//! gives an agent no prompt: it reads its own CLAUDE.md or AGENTS.md, and work reaches it by
//! pasting or `agentdeck agent send`.
//!
//! - [`codex`]: finding the session id a Codex pane's CLI chose.
//! - [`restart`]: starting an exited pane again, afresh once when its resume fails at once.
//! - [`resume`]: re-spawning the panes that were alive when the daemon last stopped.

pub mod codex;
pub mod restart;
pub mod resume;

use agentdeck_core::Agent;
use uuid::Uuid;

/// How agentdeck starts an agent CLI.
#[derive(Debug, PartialEq, Eq)]
pub struct AgentLaunch {
    /// The command.
    pub argv: Vec<String>,
    /// The session agentdeck chose for it, when the CLI takes one.
    pub session_id: Option<String>,
}

/// The variable a pane linked to a task at launch has its task's id in (spec §6).
pub const TASK_VAR: &str = "AGENTDECK_TASK";

/// How a new pane of `agent` starts, with no prompt. Claude Code starts on
/// a session id agentdeck chooses; Codex chooses its own, which [`codex`] finds afterwards.
/// `other` panes run their argv as given, so they have none here.
pub fn launch(agent: Agent) -> Option<AgentLaunch> {
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
    use super::*;

    mod launch {
        use super::*;

        #[test]
        fn should_start_claude_on_a_session_it_chooses_with_no_prompt() {
            let launch = launch(Agent::Claude).unwrap();

            let session_id = launch.session_id.expect("a session id");
            assert!(Uuid::parse_str(&session_id).is_ok(), "{session_id}");
            assert_eq!(launch.argv, ["claude", "--session-id", session_id.as_str()]);
        }

        #[test]
        fn should_choose_a_new_session_each_time() {
            let first = launch(Agent::Claude).unwrap();
            let second = launch(Agent::Claude).unwrap();

            assert_ne!(first.session_id, second.session_id);
        }

        #[test]
        fn should_start_codex_with_no_prompt() {
            assert_eq!(
                launch(Agent::Codex).unwrap(),
                AgentLaunch {
                    argv: vec!["codex".to_owned()],
                    session_id: None,
                }
            );
        }

        #[test]
        fn should_leave_other_agents_to_their_own_argv() {
            assert_eq!(launch(Agent::Other), None);
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
