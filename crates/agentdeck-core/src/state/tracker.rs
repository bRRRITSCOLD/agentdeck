//! A pane's published state over time: settling, the done/idle turn and exit (spec §7).

use std::time::Duration;

use super::AgentState;
use super::rules::{Observation, RuleSet};

/// Turns a stream of observations into published states. Time is passed in, as a monotonic
/// offset from any origin the caller picks, so nothing here reads a clock.
///
/// - A new state is published only once it has held for the rules' settle time, so a short
///   flicker publishes nothing.
/// - `working` turning `idle` publishes `done`, which holds until [`StateTracker::focus`] or
///   until the screen shows another state.
/// - [`StateTracker::exit`] publishes `exited` at once, and nothing is published after it.
#[derive(Debug, Clone)]
pub struct StateTracker {
    rules: RuleSet,
    published: AgentState,
    pending: Option<(AgentState, Duration)>,
}

impl StateTracker {
    /// A tracker for a pane that has published nothing yet: its state is `unknown`.
    pub fn new(rules: RuleSet) -> Self {
        Self {
            rules,
            published: AgentState::Unknown,
            pending: None,
        }
    }

    /// The last published state.
    pub fn state(&self) -> AgentState {
        self.published
    }

    /// Reads one observation taken at `now`. Returns the state when it publishes a new one.
    pub fn observe(&mut self, observation: &Observation, now: Duration) -> Option<AgentState> {
        if self.published == AgentState::Exited {
            return None;
        }
        let seen = match self.rules.evaluate(observation) {
            // Idle is what a done pane's screen shows; done holds until something else does.
            AgentState::Idle if self.published == AgentState::Done => AgentState::Done,
            seen => seen,
        };
        if seen == self.published {
            self.pending = None;
            return None;
        }
        let since = match self.pending {
            Some((pending, since)) if pending == seen => since,
            _ => now,
        };
        self.pending = Some((seen, since));
        if now.saturating_sub(since) < self.rules.settle() {
            return None;
        }
        let next = match (self.published, seen) {
            (AgentState::Working, AgentState::Idle) => AgentState::Done,
            (_, seen) => seen,
        };
        Some(self.publish(next))
    }

    /// The user focused the pane: a `done` pane turns `idle`. Returns the state when it changed.
    pub fn focus(&mut self) -> Option<AgentState> {
        (self.published == AgentState::Done).then(|| self.publish(AgentState::Idle))
    }

    /// The process died. Returns `exited` the first time.
    pub fn exit(&mut self) -> Option<AgentState> {
        (self.published != AgentState::Exited).then(|| self.publish(AgentState::Exited))
    }

    fn publish(&mut self, state: AgentState) -> AgentState {
        self.published = state;
        self.pending = None;
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULES: &str = r#"
agent = "test"

[[rule]]
state = "blocked"
contains = ["Esc to cancel"]

[[rule]]
state = "working"
title_regex = '^◐'

[[rule]]
state = "idle"
title_regex = '^✳'

[settle]
min_ms = 400
"#;

    fn tracker() -> StateTracker {
        StateTracker::new(RuleSet::from_toml(RULES).expect("parse"))
    }

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    fn working() -> Observation {
        Observation {
            title: "◐ agent".to_owned(),
            ..Observation::default()
        }
    }

    fn idle() -> Observation {
        Observation {
            title: "✳ agent".to_owned(),
            ..Observation::default()
        }
    }

    fn blocked() -> Observation {
        Observation {
            rows: vec!["Enter to confirm · Esc to cancel".to_owned()],
            ..idle()
        }
    }

    /// Holds `observation` from `at` until the settle time has passed, as the daemon's ticks would.
    fn settle(tracker: &mut StateTracker, observation: &Observation, at: u64) -> Option<AgentState> {
        tracker.observe(observation, ms(at));
        tracker.observe(observation, ms(at + 400))
    }

    mod state_tracker {
        use super::*;

        mod settle_time {
            use super::*;

            #[test]
            fn should_start_unknown() {
                assert_eq!(tracker().state(), AgentState::Unknown);
            }

            #[test]
            fn should_not_publish_a_state_younger_than_settle() {
                let mut tracker = tracker();
                assert_eq!(tracker.observe(&working(), ms(0)), None);
                assert_eq!(tracker.observe(&working(), ms(399)), None);
                assert_eq!(tracker.state(), AgentState::Unknown);
            }

            #[test]
            fn should_publish_a_state_once_it_has_held_for_settle() {
                let mut tracker = tracker();
                tracker.observe(&working(), ms(0));
                assert_eq!(tracker.observe(&working(), ms(400)), Some(AgentState::Working));
                assert_eq!(tracker.observe(&working(), ms(900)), None);
            }

            #[test]
            fn should_drop_a_flicker_shorter_than_settle() {
                let mut tracker = tracker();
                settle(&mut tracker, &working(), 0);
                assert_eq!(tracker.observe(&blocked(), ms(1_000)), None);
                assert_eq!(tracker.observe(&working(), ms(1_200)), None);
                assert_eq!(tracker.observe(&blocked(), ms(1_300)), None);
                assert_eq!(tracker.observe(&blocked(), ms(1_699)), None);
                assert_eq!(tracker.state(), AgentState::Working);
            }

            #[test]
            fn should_publish_at_once_with_a_zero_settle() {
                let rules = RULES.replace("min_ms = 400", "min_ms = 0");
                let mut tracker = StateTracker::new(RuleSet::from_toml(&rules).expect("parse"));
                assert_eq!(tracker.observe(&blocked(), ms(0)), Some(AgentState::Blocked));
            }
        }

        mod done {
            use super::*;

            fn done_tracker() -> StateTracker {
                let mut tracker = tracker();
                settle(&mut tracker, &working(), 0);
                tracker
            }

            #[test]
            fn should_publish_done_when_working_turns_idle() {
                let mut tracker = done_tracker();
                assert_eq!(settle(&mut tracker, &idle(), 1_000), Some(AgentState::Done));
            }

            #[test]
            fn should_stay_done_while_the_screen_reads_idle() {
                let mut tracker = done_tracker();
                settle(&mut tracker, &idle(), 1_000);
                assert_eq!(settle(&mut tracker, &idle(), 5_000), None);
                assert_eq!(tracker.state(), AgentState::Done);
            }

            #[test]
            fn should_turn_idle_on_focus() {
                let mut tracker = done_tracker();
                settle(&mut tracker, &idle(), 1_000);
                assert_eq!(tracker.focus(), Some(AgentState::Idle));
                assert_eq!(settle(&mut tracker, &idle(), 2_000), None);
                assert_eq!(tracker.state(), AgentState::Idle);
            }

            #[test]
            fn should_turn_working_on_a_new_prompt() {
                let mut tracker = done_tracker();
                settle(&mut tracker, &idle(), 1_000);
                assert_eq!(settle(&mut tracker, &working(), 2_000), Some(AgentState::Working));
            }

            #[test]
            fn should_ignore_focus_when_not_done() {
                let mut tracker = done_tracker();
                assert_eq!(tracker.focus(), None);
                assert_eq!(tracker.state(), AgentState::Working);
            }

            #[test]
            fn should_publish_idle_not_done_when_blocked_turns_idle() {
                let mut tracker = tracker();
                settle(&mut tracker, &blocked(), 0);
                assert_eq!(settle(&mut tracker, &idle(), 1_000), Some(AgentState::Idle));
            }
        }

        mod exit {
            use super::*;

            #[test]
            fn should_publish_exited_at_once_whatever_the_screen_shows() {
                let mut tracker = tracker();
                settle(&mut tracker, &working(), 0);
                tracker.observe(&idle(), ms(1_000));
                assert_eq!(tracker.exit(), Some(AgentState::Exited));
                assert_eq!(tracker.observe(&idle(), ms(2_000)), None);
                assert_eq!(tracker.state(), AgentState::Exited);
            }

            #[test]
            fn should_publish_exited_only_once() {
                let mut tracker = tracker();
                tracker.exit();
                assert_eq!(tracker.exit(), None);
            }
        }
    }
}
