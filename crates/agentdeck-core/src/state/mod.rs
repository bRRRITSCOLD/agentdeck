//! Agent state from the screen (spec §7).
//!
//! - [`rules`]: the rule file format, and [`RuleSet::evaluate`], a pure function from one
//!   [`Observation`] to a state.
//! - [`tracker`]: [`StateTracker`] settles those states over time and adds `done` and
//!   `exited`.
//! - [`osc`]: reads OSC 9;4 and OSC 7501 out of raw output for the observation, and OSC 7.
//! - `screen` (feature `screen`): reads an `alacritty_terminal` screen into the observation's
//!   rows.

use crate::text_enum::text_enum;

mod embedded;
pub mod osc;
pub mod rules;
#[cfg(any(test, feature = "screen"))]
pub mod screen;
pub mod tracker;

pub use osc::{OscScanner, ProgramState, Progress};
pub use rules::{Observation, RuleSet, RuleState, RulesError};
pub use tracker::StateTracker;

text_enum! {
    /// A pane's agent state (spec §3 `panes.state`, §7).
    pub enum AgentState ("agent state") {
        /// The agent is busy.
        Working => "working",
        /// The agent is waiting on the user: a dialog, a permission prompt.
        Blocked => "blocked",
        /// The agent finished a turn that nobody has looked at yet.
        Done => "done",
        /// The agent is waiting for a prompt.
        Idle => "idle",
        /// No rule matched and there is no recent output.
        Unknown => "unknown",
        /// The process is dead.
        Exited => "exited",
    }
}

impl AgentState {
    /// The state a group of panes shows as one, as the GUI's sidebar does for a workspace
    /// (spec §11): any blocked → blocked, else any working → working, else any done → done,
    /// else idle. Idle, unknown and exited panes, or none at all, roll up to idle.
    pub fn rollup(states: impl IntoIterator<Item = AgentState>) -> AgentState {
        let (mut working, mut done) = (false, false);
        for state in states {
            match state {
                AgentState::Blocked => return AgentState::Blocked,
                AgentState::Working => working = true,
                AgentState::Done => done = true,
                AgentState::Idle | AgentState::Unknown | AgentState::Exited => {}
            }
        }
        if working {
            AgentState::Working
        } else if done {
            AgentState::Done
        } else {
            AgentState::Idle
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod rollup {
        use super::*;
        use AgentState::*;

        #[test]
        fn should_be_blocked_when_any_pane_is_blocked() {
            assert_eq!(AgentState::rollup([Done, Working, Blocked, Idle]), Blocked);
        }

        #[test]
        fn should_be_working_when_none_is_blocked_and_any_is_working() {
            assert_eq!(AgentState::rollup([Done, Working, Exited]), Working);
        }

        #[test]
        fn should_be_done_when_none_is_blocked_or_working_and_any_is_done() {
            assert_eq!(AgentState::rollup([Idle, Done, Unknown]), Done);
        }

        #[test]
        fn should_be_idle_otherwise() {
            assert_eq!(AgentState::rollup([Idle, Unknown, Exited]), Idle);
        }

        #[test]
        fn should_be_idle_with_no_panes() {
            assert_eq!(AgentState::rollup([]), Idle);
        }
    }
}
