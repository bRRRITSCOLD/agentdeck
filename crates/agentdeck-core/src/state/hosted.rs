//! Agents a shell pane hosts (spec §3, §7): which agent, if any, its foreground process is.
//!
//! A shell pane hosts `claude` or `codex` while the user's own `claude` or `codex` is its
//! foreground process. The agent's rule file says which process names it runs as; a name other
//! than the agent's own (`node`, for an npm install of Claude Code) counts only while the
//! process runs the agent's script, so `node server.js` in a shell stays a shell.

use std::path::Path;

use super::rules::RuleSet;
use crate::pane::Agent;

/// The agents a shell pane can host: the two agentdeck ships rules for.
pub const HOSTABLE: [Agent; 2] = [Agent::Claude, Agent::Codex];

/// The process in a pane's foreground, as `/proc` shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Foreground {
    /// Its name (`/proc/<pid>/comm`).
    pub name: String,
    /// Its command line (`/proc/<pid>/cmdline`), the program first; empty when unreadable.
    pub args: Vec<String>,
}

/// The agent `foreground` is, among `agents` with the rules each runs: the first whose
/// `process` list names it, provided the name is the agent's own or the process runs the
/// agent's script.
pub fn hosted_agent(foreground: &Foreground, agents: &[(Agent, RuleSet)]) -> Option<Agent> {
    agents.iter().find_map(|(agent, rules)| {
        let named = rules.names_process(&foreground.name);
        let own = foreground.name == agent.as_str();
        (named && (own || runs_script(*agent, &foreground.args))).then_some(*agent)
    })
}

/// The npm package an agent's script ships in.
fn package(agent: Agent) -> Option<&'static str> {
    match agent {
        Agent::Claude => Some("@anthropic-ai/claude-code"),
        Agent::Codex => Some("@openai/codex"),
        Agent::Other => None,
    }
}

/// Whether `args`, an interpreter's command line, runs `agent`'s script: its first argument
/// that isn't a flag is a file named after the agent (`…/bin/claude`, `codex.js`), or sits in
/// the agent's npm package.
fn runs_script(agent: Agent, args: &[String]) -> bool {
    let Some(script) = args.iter().skip(1).find(|arg| !arg.starts_with('-')) else {
        return false;
    };
    let named = Path::new(script)
        .file_stem()
        .is_some_and(|stem| stem == agent.as_str());
    let packaged = package(agent).is_some_and(|package| script.contains(&format!("/{package}/")));
    named || packaged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agents() -> Vec<(Agent, RuleSet)> {
        HOSTABLE
            .iter()
            .map(|agent| (*agent, RuleSet::embedded(*agent).expect("embedded rules")))
            .collect()
    }

    fn running(name: &str, args: &[&str]) -> Foreground {
        Foreground {
            name: name.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        }
    }

    mod hosted_agent {
        use super::*;

        #[test]
        fn should_find_claude_and_codex_by_their_own_names() {
            let claude = running("claude", &["claude", "--resume"]);
            let codex = running("codex", &["/usr/bin/codex"]);

            assert_eq!(hosted_agent(&claude, &agents()), Some(Agent::Claude));
            assert_eq!(hosted_agent(&codex, &agents()), Some(Agent::Codex));
        }

        #[test]
        fn should_find_no_agent_in_a_shell_or_another_program() {
            for name in ["zsh", "bash", "sh", "vim", "cargo"] {
                assert_eq!(hosted_agent(&running(name, &[name]), &agents()), None);
            }
        }

        #[test]
        fn should_find_claude_in_node_running_its_bin_script() {
            let npm = running("node", &["node", "/usr/local/bin/claude", "--continue"]);
            assert_eq!(hosted_agent(&npm, &agents()), Some(Agent::Claude));
        }

        #[test]
        fn should_find_claude_in_node_running_a_script_in_its_package() {
            let npm = running(
                "node",
                &[
                    "node",
                    "--no-warnings",
                    "/home/u/.npm/lib/node_modules/@anthropic-ai/claude-code/cli.js",
                ],
            );
            assert_eq!(hosted_agent(&npm, &agents()), Some(Agent::Claude));
        }

        #[test]
        fn should_find_no_agent_in_node_running_another_script() {
            let server = running("node", &["node", "server.js"]);
            let repl = running("node", &["node"]);
            let unread = running("node", &[]);

            for foreground in [server, repl, unread] {
                assert_eq!(hosted_agent(&foreground, &agents()), None, "{foreground:?}");
            }
        }

        #[test]
        fn should_follow_the_process_list_a_rule_file_gives() {
            let rules = RuleSet::from_toml("agent = \"claude\"\nprocess = [\"claude-dev\"]\n")
                .expect("parse");
            let agents = [(Agent::Claude, rules)];

            assert_eq!(
                hosted_agent(&running("claude", &["claude"]), &agents),
                None,
                "the file's list leaves `claude` out"
            );
            assert_eq!(
                hosted_agent(&running("claude-dev", &["claude-dev"]), &agents),
                None,
                "a name not the agent's own needs its script"
            );
            assert_eq!(
                hosted_agent(&running("claude-dev", &["claude-dev", "x/claude"]), &agents),
                Some(Agent::Claude)
            );
        }
    }
}
