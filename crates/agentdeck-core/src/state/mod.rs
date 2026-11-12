//! Agent state from the screen (spec §7).
//!
//! - [`rules`]: the rule file format, and [`RuleSet::evaluate`], a pure function from one
//!   [`Observation`] to a state.
//! - [`tracker`]: [`StateTracker`] settles those states over time and adds `done` and
//!   `exited`.
//! - [`osc`]: reads OSC 9;4 and OSC 7501 out of raw output for the observation.
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
