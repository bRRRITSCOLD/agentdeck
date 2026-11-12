//! Which state rules a pane runs (spec §7): `$XDG_CONFIG_HOME/agentdeck/rules/<name>.toml`
//! when it exists, else the rules agentdeck embeds for that agent, else none.
//!
//! An agent pane's rules are named for its agent (`claude`, `codex`, `other`). A shell pane's
//! are named `shell`; agentdeck ships none, so a shell is `working` while it prints and
//! `unknown` otherwise, unless a `shell.toml` says more.

use std::path::Path;

use agentdeck_core::Agent;
use agentdeck_core::state::RuleSet;

/// The rule set for `name`, reading its override file from `dir`.
pub fn load(dir: &Path, name: &str) -> RuleSet {
    let path = dir.join(format!("{name}.toml"));
    let file = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!("reading {}: {err}; using the default rules", path.display());
            None
        }
    };
    resolve(name, file.as_deref())
}

/// The rule set for `name`, given its override file's text if there is one. A file that
/// doesn't parse is logged and ignored, so a typo can't leave every pane without rules.
pub fn resolve(name: &str, file: Option<&str>) -> RuleSet {
    if let Some(text) = file {
        match RuleSet::from_toml(text) {
            Ok(rules) => return rules,
            Err(err) => tracing::warn!("rules/{name}.toml: {err}; using the default rules"),
        }
    }
    name.parse::<Agent>()
        .ok()
        .and_then(RuleSet::embedded)
        .unwrap_or_else(|| none(name))
}

/// A rule set with no rules: only recent output (working) and silence (unknown).
fn none(name: &str) -> RuleSet {
    let text = format!("agent = {}", toml_string(name));
    RuleSet::from_toml(&text).expect("a rule file with no rules parses")
}

fn toml_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use agentdeck_core::AgentState;
    use agentdeck_core::state::Observation;

    use super::*;

    fn titled(title: &str) -> Observation {
        Observation {
            title: title.to_owned(),
            ..Observation::default()
        }
    }

    mod resolve {
        use super::*;

        #[test]
        fn should_use_the_embedded_rules_for_an_agent_without_a_file() {
            let rules = resolve("claude", None);
            assert_eq!(rules.agent(), "claude");
            assert_eq!(rules.evaluate(&titled("✳ Claude Code")), AgentState::Idle);
        }

        #[test]
        fn should_prefer_the_override_file() {
            let file = "agent = \"claude\"\n[[rule]]\nstate = \"blocked\"\ntitle_regex = '^✳'\n";
            let rules = resolve("claude", Some(file));
            assert_eq!(
                rules.evaluate(&titled("✳ Claude Code")),
                AgentState::Blocked
            );
        }

        #[test]
        fn should_fall_back_when_the_override_file_is_invalid() {
            let rules = resolve("claude", Some("not = [toml"));
            assert_eq!(rules.evaluate(&titled("✳ Claude Code")), AgentState::Idle);
        }

        #[test]
        fn should_give_a_shell_no_rules_but_recent_output() {
            let rules = resolve("shell", None);
            assert_eq!(rules.evaluate(&titled("✳ anything")), AgentState::Unknown);
            let printing = Observation {
                since_output: Some(Duration::from_millis(10)),
                ..Observation::default()
            };
            assert_eq!(rules.evaluate(&printing), AgentState::Working);
        }

        #[test]
        fn should_read_a_shell_rules_file() {
            let file = "agent = \"shell\"\n[[rule]]\nstate = \"idle\"\ntitle_regex = '^idle$'\n";
            let rules = resolve("shell", Some(file));
            assert_eq!(rules.evaluate(&titled("idle")), AgentState::Idle);
        }
    }
}
