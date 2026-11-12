//! What runs in a pane (spec §3, `panes.kind` and `panes.agent`).

use crate::text_enum::text_enum;

text_enum! {
    /// Whether a pane runs an agent CLI or a plain shell.
    pub enum PaneKind ("pane kind") {
        /// An agent CLI; its [`Agent`] says which.
        Agent => "agent",
        /// A shell or any other command; it has no agent state rules.
        Shell => "shell",
    }
}

text_enum! {
    /// Which agent CLI an agent pane runs. It picks the launch argv and the state rules.
    pub enum Agent ("agent") {
        /// Claude Code.
        Claude => "claude",
        /// Codex CLI.
        Codex => "codex",
        /// Any other agent CLI, launched with the argv as given.
        Other => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod pane_kind {
        use super::*;

        #[test]
        fn should_use_the_spec_column_values() {
            let texts: Vec<_> = PaneKind::ALL.iter().map(|k| k.as_str()).collect();
            assert_eq!(texts, ["agent", "shell"]);
        }
    }

    mod agent {
        use super::*;

        #[test]
        fn should_use_the_spec_column_values() {
            let texts: Vec<_> = Agent::ALL.iter().map(|a| a.as_str()).collect();
            assert_eq!(texts, ["claude", "codex", "other"]);
        }
    }
}
