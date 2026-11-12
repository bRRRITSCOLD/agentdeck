//! The rule sets compiled into agentdeck, one TOML file per agent under `rules/`. A file in
//! `$XDG_CONFIG_HOME/agentdeck/rules/<agent>.toml` replaces one; reading it is the daemon's job,
//! through [`RuleSet::from_toml`].

use super::rules::RuleSet;
use crate::pane::Agent;

const CLAUDE: &str = include_str!("../../rules/claude.toml");

impl RuleSet {
    /// The embedded rules for `agent`, or `None` when agentdeck ships none for it.
    pub fn embedded(agent: Agent) -> Option<RuleSet> {
        let text = match agent {
            Agent::Claude => CLAUDE,
            Agent::Codex | Agent::Other => return None,
        };
        let rules = RuleSet::from_toml(text);
        // A unit test parses every embedded file, so this can't fail in a build that passed.
        Some(rules.expect("embedded rules parse"))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use alacritty_terminal::Term;
    use alacritty_terminal::event::{Event, EventListener};
    use alacritty_terminal::term::Config;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

    use super::*;
    use crate::state::{AgentState, Observation, OscScanner, StateTracker, screen};

    /// Recorded terminal output replayed through a real `Term` at 100×30, the size the spike
    /// recorded Claude Code at.
    ///
    /// The `tests/fixtures/claude/*.bytes` files are hand-authored stand-ins, not recordings:
    /// the spike kept no byte logs, and running `claude` to record them costs money. They follow
    /// what the spike saw of v2.1.322: an inline TUI that redraws by erasing its last frame, the
    /// OSC 0 title as the state glyph, and an OSC 7501 probe at startup. Replace them with real
    /// recordings when one is made; the expected states below stay.
    struct Replay {
        term: Term<Titles>,
        parser: Processor<StdSyncHandler>,
        osc: OscScanner,
        titles: Titles,
    }

    /// The title the program last set, which `Term` reports as an event.
    #[derive(Clone, Default)]
    struct Titles(Arc<Mutex<String>>);

    impl EventListener for Titles {
        fn send_event(&self, event: Event) {
            let mut title = self.0.lock().expect("title lock");
            match event {
                Event::Title(new) => *title = new,
                Event::ResetTitle => title.clear(),
                _ => {}
            }
        }
    }

    impl Replay {
        fn new() -> Self {
            let titles = Titles::default();
            Self {
                term: Term::new(Config::default(), &TermSize::new(100, 30), titles.clone()),
                parser: Processor::new(),
                osc: OscScanner::new(),
                titles,
            }
        }

        fn of(fixtures: &[&[u8]]) -> Self {
            let mut replay = Self::new();
            for bytes in fixtures {
                replay.feed(bytes);
            }
            replay
        }

        fn feed(&mut self, bytes: &[u8]) {
            self.parser.advance(&mut self.term, bytes);
            self.osc.feed(bytes);
        }

        fn observation(&self) -> Observation {
            Observation {
                process: Some("claude".to_owned()),
                title: self.titles.0.lock().expect("title lock").clone(),
                rows: screen::rows(&self.term),
                progress: self.osc.progress(),
                program_state: self.osc.program_state(),
                since_output: None,
            }
        }
    }

    const IDLE: &[u8] = include_bytes!("../../tests/fixtures/claude/idle.bytes");
    const WORKING: &[u8] = include_bytes!("../../tests/fixtures/claude/working.bytes");
    const DONE: &[u8] = include_bytes!("../../tests/fixtures/claude/done.bytes");
    const PERMISSION_PROMPT: &[u8] =
        include_bytes!("../../tests/fixtures/claude/permission-prompt.bytes");
    const TRUST_DIALOG: &[u8] = include_bytes!("../../tests/fixtures/claude/trust-dialog.bytes");

    fn claude() -> RuleSet {
        RuleSet::embedded(Agent::Claude).expect("claude rules")
    }

    fn state_after(fixtures: &[&[u8]]) -> AgentState {
        claude().evaluate(&Replay::of(fixtures).observation())
    }

    mod embedded {
        use super::*;

        #[test]
        fn should_ship_rules_for_claude() {
            assert_eq!(claude().agent(), "claude");
        }

        #[test]
        fn should_ship_no_rules_for_other_agents_yet() {
            assert!(RuleSet::embedded(Agent::Codex).is_none());
            assert!(RuleSet::embedded(Agent::Other).is_none());
        }

        mod claude {
            use super::*;

            #[test]
            fn should_be_idle_at_a_fresh_prompt() {
                assert_eq!(state_after(&[IDLE]), AgentState::Idle);
            }

            #[test]
            fn should_be_working_while_the_spinner_runs() {
                assert_eq!(state_after(&[IDLE, WORKING]), AgentState::Working);
            }

            #[test]
            fn should_read_working_from_a_braille_spinner_before_v2_1_228() {
                let observation = Observation {
                    title: "⠋ Claude Code".to_owned(),
                    ..Replay::of(&[IDLE]).observation()
                };
                assert_eq!(claude().evaluate(&observation), AgentState::Working);
            }

            #[test]
            fn should_be_blocked_on_the_trust_dialog() {
                assert_eq!(state_after(&[TRUST_DIALOG]), AgentState::Blocked);
            }

            #[test]
            fn should_be_blocked_on_a_permission_prompt() {
                assert_eq!(
                    state_after(&[IDLE, WORKING, PERMISSION_PROMPT]),
                    AgentState::Blocked
                );
            }

            #[test]
            fn should_publish_done_when_a_turn_finishes() {
                let mut replay = Replay::of(&[IDLE, WORKING]);
                let mut tracker = StateTracker::new(claude());
                let ms = Duration::from_millis;

                tracker.observe(&replay.observation(), ms(0));
                assert_eq!(
                    tracker.observe(&replay.observation(), ms(400)),
                    Some(AgentState::Working)
                );

                replay.feed(DONE);
                assert_eq!(tracker.observe(&replay.observation(), ms(1_000)), None);
                assert_eq!(
                    tracker.observe(&replay.observation(), ms(1_400)),
                    Some(AgentState::Done)
                );
            }
        }
    }
}
