//! The rule sets compiled into agentdeck, one TOML file per agent under `rules/`. A file in
//! `$XDG_CONFIG_HOME/agentdeck/rules/<agent>.toml` replaces one; reading it is the daemon's job,
//! through [`RuleSet::from_toml`].

use super::rules::RuleSet;
use crate::pane::Agent;

const CLAUDE: &str = include_str!("../../rules/claude.toml");
const CODEX: &str = include_str!("../../rules/codex.toml");

impl RuleSet {
    /// The embedded rules for `agent`, or `None` when agentdeck ships none for it.
    pub fn embedded(agent: Agent) -> Option<RuleSet> {
        let text = match agent {
            Agent::Claude => CLAUDE,
            Agent::Codex => CODEX,
            Agent::Other => return None,
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

    /// Terminal output replayed through a real `Term` at the size it was made at, read as the
    /// daemon reads a pane: the screen rows, the title, and OSC 9;4 and 7501 from the raw bytes.
    struct Replay {
        term: Term<Titles>,
        parser: Processor<StdSyncHandler>,
        osc: OscScanner,
        titles: Titles,
        process: &'static str,
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

    /// What a fixture set was made with: the terminal size and the foreground process.
    struct Recording {
        columns: usize,
        lines: usize,
        process: &'static str,
    }

    impl Replay {
        fn of(recording: &Recording, fixtures: &[&[u8]]) -> Self {
            let titles = Titles::default();
            let size = TermSize::new(recording.columns, recording.lines);
            let mut replay = Self {
                term: Term::new(Config::default(), &size, titles.clone()),
                parser: Processor::new(),
                osc: OscScanner::new(),
                titles,
                process: recording.process,
            };
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
                process: Some(self.process.to_owned()),
                title: self.titles.0.lock().expect("title lock").clone(),
                rows: screen::rows(&self.term),
                progress: self.osc.progress(),
                program_state: self.osc.program_state(),
                since_output: None,
            }
        }
    }

    mod embedded {
        use super::*;

        #[test]
        fn should_ship_rules_for_claude_and_codex() {
            for (agent, name) in [(Agent::Claude, "claude"), (Agent::Codex, "codex")] {
                let rules = RuleSet::embedded(agent).expect("embedded rules");
                assert_eq!(rules.agent(), name);
            }
        }

        #[test]
        fn should_ship_no_rules_for_other_agents() {
            assert!(RuleSet::embedded(Agent::Other).is_none());
        }

        /// Real recordings of Claude Code v2.1.325 at 120×40 (spec §7, "Recording fixtures"),
        /// each fixture the bytes since the one before it, so a test feeds a chain in order:
        ///
        /// - `trust-dialog`: startup in a folder Claude Code hasn't been told to trust.
        /// - `idle` → `thinking` → `replied` → `permission-prompt` → `command` →
        ///   `command-done` → `quoted` → `exit`: the default inline TUI with a fresh prompt, a
        ///   reply being written, that reply finished, a Bash permission prompt, the command
        ///   running once allowed, that turn finished, a reply quoting a dialog's text, and
        ///   `/exit`.
        /// - `fullscreen-idle` → `fullscreen-working` → `fullscreen-permission-prompt` →
        ///   `fullscreen-declined`: the fullscreen TUI with a status line, a second session
        ///   whose permission prompt is declined with Esc.
        mod claude {
            use super::*;

            const CLAUDE: Recording = Recording {
                columns: 120,
                lines: 40,
                process: "claude",
            };

            const TRUST_DIALOG: &[u8] =
                include_bytes!("../../tests/fixtures/claude/trust-dialog.bytes");
            const IDLE: &[u8] = include_bytes!("../../tests/fixtures/claude/idle.bytes");
            const THINKING: &[u8] = include_bytes!("../../tests/fixtures/claude/thinking.bytes");
            const REPLIED: &[u8] = include_bytes!("../../tests/fixtures/claude/replied.bytes");
            const PERMISSION_PROMPT: &[u8] =
                include_bytes!("../../tests/fixtures/claude/permission-prompt.bytes");
            const COMMAND: &[u8] = include_bytes!("../../tests/fixtures/claude/command.bytes");
            const COMMAND_DONE: &[u8] =
                include_bytes!("../../tests/fixtures/claude/command-done.bytes");
            const QUOTED: &[u8] = include_bytes!("../../tests/fixtures/claude/quoted.bytes");
            const EXIT: &[u8] = include_bytes!("../../tests/fixtures/claude/exit.bytes");

            /// The inline session, in recording order.
            const TURNS: &[&[u8]] = &[
                IDLE,
                THINKING,
                REPLIED,
                PERMISSION_PROMPT,
                COMMAND,
                COMMAND_DONE,
                QUOTED,
                EXIT,
            ];

            fn rules() -> RuleSet {
                RuleSet::embedded(Agent::Claude).expect("claude rules")
            }

            /// The state once the inline session's fixtures up to and including `last` are fed.
            fn state_through(last: &[u8]) -> AgentState {
                rules().evaluate(&Replay::of(&CLAUDE, turns_through(last)).observation())
            }

            fn turns_through(last: &[u8]) -> &'static [&'static [u8]] {
                let end = TURNS
                    .iter()
                    .position(|fixture| std::ptr::eq(*fixture, last))
                    .expect("a fixture of the inline session");
                &TURNS[..=end]
            }

            #[test]
            fn should_be_blocked_on_the_trust_dialog() {
                let observation = Replay::of(&CLAUDE, &[TRUST_DIALOG]).observation();
                assert_eq!(rules().evaluate(&observation), AgentState::Blocked);
            }

            #[test]
            fn should_be_idle_at_a_fresh_prompt() {
                assert_eq!(state_through(IDLE), AgentState::Idle);
            }

            #[test]
            fn should_be_working_while_it_writes_a_reply() {
                assert_eq!(state_through(THINKING), AgentState::Working);
            }

            #[test]
            fn should_be_idle_once_a_reply_is_written() {
                assert_eq!(state_through(REPLIED), AgentState::Idle);
            }

            #[test]
            fn should_be_blocked_on_a_permission_prompt_though_the_title_shows_idle() {
                let observation =
                    Replay::of(&CLAUDE, turns_through(PERMISSION_PROMPT)).observation();
                assert!(
                    observation.title.starts_with('✳'),
                    "{:?}",
                    observation.title
                );
                assert_eq!(rules().evaluate(&observation), AgentState::Blocked);
            }

            #[test]
            fn should_be_working_while_a_command_runs() {
                assert_eq!(state_through(COMMAND), AgentState::Working);
            }

            #[test]
            fn should_be_idle_once_the_command_finishes() {
                assert_eq!(state_through(COMMAND_DONE), AgentState::Idle);
            }

            #[test]
            fn should_stay_idle_when_a_reply_quotes_a_dialog() {
                assert_eq!(state_through(QUOTED), AgentState::Idle);
            }

            #[test]
            fn should_stay_idle_when_a_reply_quotes_the_activity_line() {
                let quoted = Replay::of(&CLAUDE, turns_through(QUOTED)).observation();
                let rows = quoted
                    .rows
                    .iter()
                    .map(|row| row.replace("Esc to cancel", "esc to interrupt"))
                    .collect();
                let observation = Observation { rows, ..quoted };
                assert_eq!(rules().evaluate(&observation), AgentState::Idle);
            }

            #[test]
            fn should_read_working_from_the_footer_without_a_title() {
                let observation = Observation {
                    title: String::new(),
                    ..Replay::of(&CLAUDE, turns_through(THINKING)).observation()
                };
                assert_eq!(rules().evaluate(&observation), AgentState::Working);
            }

            #[test]
            fn should_read_working_from_a_braille_spinner_before_v2_1_228() {
                let observation = Observation {
                    title: "⠋ Claude Code".to_owned(),
                    ..Replay::of(&CLAUDE, &[IDLE]).observation()
                };
                assert_eq!(rules().evaluate(&observation), AgentState::Working);
            }

            #[test]
            fn should_publish_done_when_a_turn_finishes() {
                let mut replay = Replay::of(&CLAUDE, &[IDLE, THINKING]);
                let mut tracker = StateTracker::new(rules());
                let ms = Duration::from_millis;

                tracker.observe(&replay.observation(), ms(0));
                assert_eq!(
                    tracker.observe(&replay.observation(), ms(400)),
                    Some(AgentState::Working)
                );

                replay.feed(REPLIED);
                assert_eq!(tracker.observe(&replay.observation(), ms(1_000)), None);
                assert_eq!(
                    tracker.observe(&replay.observation(), ms(1_400)),
                    Some(AgentState::Done)
                );
            }

            #[test]
            fn should_be_unknown_on_an_empty_screen() {
                let observation = Replay::of(&CLAUDE, &[]).observation();
                assert_eq!(rules().evaluate(&observation), AgentState::Unknown);
            }

            #[test]
            fn should_leave_exited_to_the_process_once_it_quits() {
                assert_eq!(state_through(EXIT), AgentState::Unknown);
            }

            mod fullscreen {
                use super::*;

                const IDLE: &[u8] =
                    include_bytes!("../../tests/fixtures/claude/fullscreen-idle.bytes");
                const WORKING: &[u8] =
                    include_bytes!("../../tests/fixtures/claude/fullscreen-working.bytes");
                const PERMISSION_PROMPT: &[u8] = include_bytes!(
                    "../../tests/fixtures/claude/fullscreen-permission-prompt.bytes"
                );
                const DECLINED: &[u8] =
                    include_bytes!("../../tests/fixtures/claude/fullscreen-declined.bytes");

                fn state_after(fixtures: &[&[u8]]) -> AgentState {
                    rules().evaluate(&Replay::of(&CLAUDE, fixtures).observation())
                }

                #[test]
                fn should_be_idle_at_a_fresh_prompt() {
                    assert_eq!(state_after(&[IDLE]), AgentState::Idle);
                }

                #[test]
                fn should_be_working_while_it_thinks() {
                    assert_eq!(state_after(&[IDLE, WORKING]), AgentState::Working);
                }

                #[test]
                fn should_be_blocked_on_a_permission_prompt() {
                    assert_eq!(
                        state_after(&[IDLE, WORKING, PERMISSION_PROMPT]),
                        AgentState::Blocked
                    );
                }

                #[test]
                fn should_be_idle_once_a_permission_prompt_is_declined() {
                    let declined = [IDLE, WORKING, PERMISSION_PROMPT, DECLINED];
                    assert_eq!(state_after(&declined), AgentState::Idle);
                }
            }
        }

        /// Real recordings of codex-cli 0.169.0 at 120×40 (spec §7, "Recording fixtures"),
        /// each fixture the bytes since the one before it, so a test feeds a chain in order:
        ///
        /// - `trust-dialog`: startup in a folder Codex hasn't been told to trust.
        /// - `idle` → `thinking` → `replied` → `command`: a fresh prompt, a reply being
        ///   written, that reply finished, and a shell command running in the next turn.
        /// - `permission-idle` → `permission-prompt` → `permission-declined` → `exit`: a
        ///   second session that asks to run a command outside the sandbox, has it declined
        ///   with Esc, then quits with `/exit`.
        mod codex {
            use super::*;

            const CODEX: Recording = Recording {
                columns: 120,
                lines: 40,
                process: "codex",
            };

            const TRUST_DIALOG: &[u8] =
                include_bytes!("../../tests/fixtures/codex/trust-dialog.bytes");
            const IDLE: &[u8] = include_bytes!("../../tests/fixtures/codex/idle.bytes");
            const THINKING: &[u8] = include_bytes!("../../tests/fixtures/codex/thinking.bytes");
            const REPLIED: &[u8] = include_bytes!("../../tests/fixtures/codex/replied.bytes");
            const COMMAND: &[u8] = include_bytes!("../../tests/fixtures/codex/command.bytes");
            const PERMISSION_IDLE: &[u8] =
                include_bytes!("../../tests/fixtures/codex/permission-idle.bytes");
            const PERMISSION_PROMPT: &[u8] =
                include_bytes!("../../tests/fixtures/codex/permission-prompt.bytes");
            const PERMISSION_DECLINED: &[u8] =
                include_bytes!("../../tests/fixtures/codex/permission-declined.bytes");
            const EXIT: &[u8] = include_bytes!("../../tests/fixtures/codex/exit.bytes");

            /// The permission session up to its prompt.
            const PERMISSION: &[&[u8]] = &[PERMISSION_IDLE, PERMISSION_PROMPT];

            fn rules() -> RuleSet {
                RuleSet::embedded(Agent::Codex).expect("codex rules")
            }

            fn state_after(fixtures: &[&[u8]]) -> AgentState {
                rules().evaluate(&Replay::of(&CODEX, fixtures).observation())
            }

            /// `rows` with `quoted` written where a reply goes: just above the composer, which
            /// sits four rows from the bottom, with the footer still last.
            fn with_reply(mut rows: Vec<String>, quoted: &[&str]) -> Vec<String> {
                let at = rows.len() - 4 - quoted.len();
                for (row, text) in rows[at..].iter_mut().zip(quoted) {
                    *row = format!("  {text}");
                }
                rows
            }

            #[test]
            fn should_be_blocked_on_the_trust_dialog() {
                assert_eq!(state_after(&[TRUST_DIALOG]), AgentState::Blocked);
            }

            #[test]
            fn should_be_idle_at_a_fresh_prompt() {
                assert_eq!(state_after(&[IDLE]), AgentState::Idle);
            }

            #[test]
            fn should_be_working_while_it_writes_a_reply() {
                assert_eq!(state_after(&[IDLE, THINKING]), AgentState::Working);
            }

            #[test]
            fn should_be_idle_once_a_reply_is_written() {
                assert_eq!(state_after(&[IDLE, THINKING, REPLIED]), AgentState::Idle);
            }

            #[test]
            fn should_be_working_while_a_command_runs() {
                assert_eq!(
                    state_after(&[IDLE, THINKING, REPLIED, COMMAND]),
                    AgentState::Working
                );
            }

            #[test]
            fn should_be_blocked_on_a_permission_prompt() {
                assert_eq!(state_after(PERMISSION), AgentState::Blocked);
            }

            #[test]
            fn should_read_a_permission_prompt_from_the_screen_alone() {
                let observation = Observation {
                    program_state: None,
                    ..Replay::of(&CODEX, PERMISSION).observation()
                };
                assert_eq!(rules().evaluate(&observation), AgentState::Blocked);
            }

            #[test]
            fn should_be_blocked_when_osc_7501_says_so_whatever_the_screen_shows() {
                let observation = Observation {
                    rows: Replay::of(&CODEX, &[IDLE]).observation().rows,
                    ..Replay::of(&CODEX, PERMISSION).observation()
                };
                assert_eq!(rules().evaluate(&observation), AgentState::Blocked);
            }

            #[test]
            fn should_be_idle_once_a_permission_prompt_is_declined() {
                let declined = [PERMISSION_IDLE, PERMISSION_PROMPT, PERMISSION_DECLINED];
                assert_eq!(state_after(&declined), AgentState::Idle);
            }

            #[test]
            fn should_publish_done_when_a_turn_finishes() {
                let mut replay = Replay::of(&CODEX, &[IDLE, THINKING]);
                let mut tracker = StateTracker::new(rules());
                let ms = Duration::from_millis;

                tracker.observe(&replay.observation(), ms(0));
                assert_eq!(
                    tracker.observe(&replay.observation(), ms(400)),
                    Some(AgentState::Working)
                );

                replay.feed(REPLIED);
                assert_eq!(tracker.observe(&replay.observation(), ms(1_000)), None);
                assert_eq!(
                    tracker.observe(&replay.observation(), ms(1_400)),
                    Some(AgentState::Done)
                );
            }

            #[test]
            fn should_stay_idle_when_a_reply_quotes_a_dialog() {
                let replied = Replay::of(&CODEX, &[IDLE, THINKING, REPLIED]).observation();
                let observation = Observation {
                    rows: with_reply(
                        replied.rows.clone(),
                        &[
                            "Would you like to run the following command?",
                            "Press enter to confirm or esc to cancel",
                            "Trust this folder?",
                            "enter continue · esc back",
                        ],
                    ),
                    ..replied
                };
                assert_eq!(rules().evaluate(&observation), AgentState::Idle);
            }

            #[test]
            fn should_be_unknown_on_an_empty_screen() {
                assert_eq!(state_after(&[]), AgentState::Unknown);
            }

            #[test]
            fn should_leave_exited_to_the_process_once_it_quits() {
                let quit = [
                    PERMISSION_IDLE,
                    PERMISSION_PROMPT,
                    PERMISSION_DECLINED,
                    EXIT,
                ];
                assert_eq!(state_after(&quit), AgentState::Unknown);
            }
        }
    }
}
