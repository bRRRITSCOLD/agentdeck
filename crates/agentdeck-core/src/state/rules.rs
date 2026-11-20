//! The rule file format (spec §7) and the pure evaluator over one [`Observation`].
//!
//! ```toml
//! agent = "claude"
//! process = ["claude"]          # the rules apply only while one of these is in the foreground
//!
//! [[rule]]                      # first match wins; every condition a rule names must hold
//! state = "blocked"             # working | blocked | idle
//! rows = "bottom:6"             # `screen`, or the last N non-blank rows (default `screen`)
//! contains = ["Enter to confirm", "Esc to cancel"]   # every string must appear in `rows`
//!
//! [[rule]]
//! state = "working"
//! title_regex = '^[◐◓◑◒]'       # matched against the OSC 0/2 title
//!
//! [[rule]]
//! state = "working"
//! progress = ["normal", "indeterminate"]      # OSC 9;4: removed | normal | error | indeterminate | paused
//!
//! [[rule]]
//! state = "blocked"
//! program_status = ["blocked"]  # OSC 7501: idle | working | done | blocked | error
//!
//! [settle]
//! min_ms = 400                  # a new state must hold this long before it is published
//! ```

use std::time::Duration;

use regex::Regex;
use serde::Deserialize;
use thiserror::Error;

use super::AgentState;
use super::osc::{ProgramState, Progress};
use crate::text_enum::text_enum;

/// With no rule matching, output this recent means `working` (spec §7, "No match").
pub const RECENT_OUTPUT: Duration = Duration::from_secs(2);

/// Output this soon after a resize is the program redrawing for its new size, not work.
pub const RESIZE_REDRAW: Duration = Duration::from_millis(300);

/// The last output that counts as recent output (spec §7, "No match"), at times passed in as
/// a monotonic offset, as [`StateTracker`](super::StateTracker)'s are. Output within
/// [`RESIZE_REDRAW`] of a resize doesn't count, and neither does what a new shell prints
/// before anything is typed into it: its prompt, drawn and redrawn. Nor does what a shell
/// prints while it is itself in the pane's foreground, running no command: its prompt, the
/// echo of what is typed at it.
#[derive(Debug, Clone, Default)]
pub struct LastOutput {
    at: Option<Duration>,
    resized: Option<Duration>,
    awaiting_input: bool,
    shell_in_front: bool,
}

impl LastOutput {
    /// An agent pane's: its output counts from the start.
    pub fn agent() -> Self {
        Self::default()
    }

    /// A shell pane's: its output counts once something is typed into it.
    pub fn shell() -> Self {
        Self {
            awaiting_input: true,
            ..Self::default()
        }
    }

    /// The pane printed at `now`.
    pub fn printed(&mut self, now: Duration) {
        let redraw = (self.resized).is_some_and(|at| now.saturating_sub(at) < RESIZE_REDRAW);
        if !redraw && !self.awaiting_input && !self.shell_in_front {
            self.at = Some(now);
        }
    }

    /// The pane was resized at `now`.
    pub fn resized(&mut self, now: Duration) {
        self.resized = Some(now);
    }

    /// Something was typed into the pane.
    pub fn typed(&mut self) {
        self.awaiting_input = false;
    }

    /// Whether a shell pane's shell is itself in its foreground, with no command running, as
    /// last read. While it is, what the pane prints doesn't count. Once it is back in front,
    /// what a command printed before it doesn't count either: an agent the shell hosted, say,
    /// whose last output came just before it exited.
    pub fn shell_in_front(&mut self, in_front: bool) {
        self.shell_in_front = in_front;
        if in_front {
            self.at = None;
        }
    }

    /// Time from the last output that counts to `now`; `None` if none has.
    pub fn since(&self, now: Duration) -> Option<Duration> {
        self.at.map(|at| now.saturating_sub(at))
    }
}

/// `[settle] min_ms` when a rule file leaves it out.
pub const DEFAULT_SETTLE: Duration = Duration::from_millis(400);

text_enum! {
    /// The states a rule can name. `done` comes from a working → idle change, `unknown` from
    /// no rule matching, and `exited` from the process, so no rule produces them.
    pub enum RuleState ("rule state") {
        /// The agent is busy.
        Working => "working",
        /// The agent is waiting on the user: a dialog, a permission prompt.
        Blocked => "blocked",
        /// The agent is waiting for a prompt.
        Idle => "idle",
    }
}

impl From<RuleState> for AgentState {
    fn from(state: RuleState) -> Self {
        match state {
            RuleState::Working => AgentState::Working,
            RuleState::Blocked => AgentState::Blocked,
            RuleState::Idle => AgentState::Idle,
        }
    }
}

/// Everything the rules can see of a pane at one moment.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Observation {
    /// The foreground process's name, or `None` when it couldn't be read.
    pub process: Option<String>,
    /// The OSC 0/2 window title; empty when none was set.
    pub title: String,
    /// The visible screen, top to bottom, one string per row.
    pub rows: Vec<String>,
    /// The last OSC 9;4 progress state.
    pub progress: Option<Progress>,
    /// The last OSC 7501 program state.
    pub program_state: Option<ProgramState>,
    /// Time since the pane last produced output; `None` if it never has.
    pub since_output: Option<Duration>,
}

/// Why a rule file was rejected. Rules are numbered from 1, in file order.
#[derive(Debug, Error)]
pub enum RulesError {
    /// Not TOML, or not this format (an unknown key, a state no rule may name, …).
    #[error("invalid rule file: {0}")]
    Toml(#[from] toml::de::Error),
    /// A `title_regex` that doesn't compile.
    #[error("rule {rule}: invalid title_regex: {source}")]
    TitleRegex {
        /// The rule's number.
        rule: usize,
        /// What the regex compiler said.
        source: regex::Error,
    },
    /// A `rows` that isn't `screen` or `bottom:<n>` with n ≥ 1.
    #[error("rule {rule}: rows `{rows}` is not `screen` or `bottom:<n>`")]
    Rows {
        /// The rule's number.
        rule: usize,
        /// The value given.
        rows: String,
    },
    /// `rows` with nothing to look for in them.
    #[error("rule {rule}: rows is set but contains is empty")]
    RowsWithoutContains {
        /// The rule's number.
        rule: usize,
    },
    /// A rule with no condition, which would match everything after it.
    #[error("rule {rule} has no condition")]
    NoCondition {
        /// The rule's number.
        rule: usize,
    },
}

/// One agent CLI's rules, parsed and checked.
#[derive(Debug, Clone)]
pub struct RuleSet {
    agent: String,
    process: Vec<String>,
    rules: Vec<Rule>,
    settle: Duration,
}

#[derive(Debug, Clone)]
struct Rule {
    state: RuleState,
    title: Option<Regex>,
    rows: Rows,
    contains: Vec<String>,
    progress: Vec<Progress>,
    program_status: Vec<ProgramState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rows {
    Screen,
    Bottom(usize),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleFile {
    agent: String,
    #[serde(default)]
    process: Vec<String>,
    #[serde(default, rename = "rule")]
    rules: Vec<RuleEntry>,
    #[serde(default)]
    settle: SettleEntry,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleEntry {
    state: RuleState,
    title_regex: Option<String>,
    rows: Option<String>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    progress: Vec<Progress>,
    #[serde(default)]
    program_status: Vec<ProgramState>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettleEntry {
    min_ms: u64,
}

impl Default for SettleEntry {
    fn default() -> Self {
        Self {
            min_ms: DEFAULT_SETTLE.as_millis() as u64,
        }
    }
}

impl RuleSet {
    /// Parses and checks a rule file.
    pub fn from_toml(text: &str) -> Result<Self, RulesError> {
        let file: RuleFile = toml::from_str(text)?;
        let rules = file
            .rules
            .into_iter()
            .enumerate()
            .map(|(index, entry)| Rule::new(index + 1, entry))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            agent: file.agent,
            process: file.process,
            rules,
            settle: Duration::from_millis(file.settle.min_ms),
        })
    }

    /// The agent these rules are for, as the file's `agent` names it.
    pub fn agent(&self) -> &str {
        &self.agent
    }

    /// How long a new state must hold before it is published.
    pub fn settle(&self) -> Duration {
        self.settle
    }

    /// The state this one observation shows, before settling: the first matching rule's,
    /// else `working` if output is recent, else `unknown`. Never `done` or `exited`.
    pub fn evaluate(&self, observation: &Observation) -> AgentState {
        if self.applies_to(observation.process.as_deref())
            && let Some(rule) = self.rules.iter().find(|rule| rule.matches(observation))
        {
            return rule.state.into();
        }
        match observation.since_output {
            Some(since) if since < RECENT_OUTPUT => AgentState::Working,
            _ => AgentState::Unknown,
        }
    }

    /// Whether the file's `process` list names `name`.
    pub fn names_process(&self, name: &str) -> bool {
        self.process.iter().any(|p| p == name)
    }

    /// The rules apply while a listed process is in front, or when the process is unknown.
    fn applies_to(&self, process: Option<&str>) -> bool {
        match process {
            Some(name) if !self.process.is_empty() => self.process.iter().any(|p| p == name),
            _ => true,
        }
    }
}

impl Rule {
    fn new(number: usize, entry: RuleEntry) -> Result<Self, RulesError> {
        let title = entry
            .title_regex
            .map(|pattern| Regex::new(&pattern))
            .transpose()
            .map_err(|source| RulesError::TitleRegex {
                rule: number,
                source,
            })?;
        let rows = match entry.rows {
            None => Rows::Screen,
            Some(_) if entry.contains.is_empty() => {
                return Err(RulesError::RowsWithoutContains { rule: number });
            }
            Some(rows) => Rows::parse(&rows).ok_or(RulesError::Rows { rule: number, rows })?,
        };
        if title.is_none()
            && entry.contains.is_empty()
            && entry.progress.is_empty()
            && entry.program_status.is_empty()
        {
            return Err(RulesError::NoCondition { rule: number });
        }
        Ok(Self {
            state: entry.state,
            title,
            rows,
            contains: entry.contains,
            progress: entry.progress,
            program_status: entry.program_status,
        })
    }

    fn matches(&self, observation: &Observation) -> bool {
        self.title
            .as_ref()
            .is_none_or(|title| title.is_match(&observation.title))
            && self.contains_all(&observation.rows)
            && (self.progress.is_empty()
                || observation
                    .progress
                    .is_some_and(|p| self.progress.contains(&p)))
            && (self.program_status.is_empty()
                || observation
                    .program_state
                    .is_some_and(|s| self.program_status.contains(&s)))
    }

    fn contains_all(&self, screen: &[String]) -> bool {
        if self.contains.is_empty() {
            return true;
        }
        let region = self.rows.of(screen);
        self.contains
            .iter()
            .all(|needle| region.iter().any(|row| row.contains(needle.as_str())))
    }
}

impl Rows {
    fn parse(text: &str) -> Option<Self> {
        if text == "screen" {
            return Some(Rows::Screen);
        }
        let count: usize = text.strip_prefix("bottom:")?.parse().ok()?;
        (count > 0).then_some(Rows::Bottom(count))
    }

    /// The rows this region covers. `bottom:N` skips blank rows, so an inline TUI's prompt
    /// counts the same whether or not empty lines sit under it.
    fn of(self, screen: &[String]) -> Vec<&str> {
        match self {
            Rows::Screen => screen.iter().map(String::as_str).collect(),
            Rows::Bottom(count) => {
                let mut rows: Vec<&str> = screen
                    .iter()
                    .rev()
                    .map(String::as_str)
                    .filter(|row| !row.trim().is_empty())
                    .take(count)
                    .collect();
                rows.reverse();
                rows
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_EXAMPLE: &str = r#"
agent = "claude"
process = ["claude"]

[[rule]]
state = "blocked"
rows = "bottom:6"
contains = ["Enter to confirm", "Esc to cancel"]

[[rule]]
state = "working"
title_regex = '^[◐◓◑◒]'

[[rule]]
state = "idle"
title_regex = '^✳'

[settle]
min_ms = 400
"#;

    fn spec_rules() -> RuleSet {
        RuleSet::from_toml(SPEC_EXAMPLE).expect("the spec's example parses")
    }

    fn titled(title: &str) -> Observation {
        Observation {
            title: title.to_owned(),
            ..Observation::default()
        }
    }

    fn rows(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| (*line).to_owned()).collect()
    }

    mod rule_set {
        use super::*;

        mod from_toml {
            use super::*;

            #[test]
            fn should_read_the_agent_and_settle_time() {
                let rules = spec_rules();
                assert_eq!(rules.agent(), "claude");
                assert_eq!(rules.settle(), Duration::from_millis(400));
            }

            #[test]
            fn should_default_settle_to_400_ms() {
                let rules = RuleSet::from_toml(
                    "agent = \"x\"\n[[rule]]\nstate = \"idle\"\ntitle_regex = 'x'\n",
                )
                .expect("parse");
                assert_eq!(rules.settle(), DEFAULT_SETTLE);
            }

            #[test]
            fn should_reject_a_key_it_does_not_know() {
                let err =
                    RuleSet::from_toml("agent = \"x\"\n[[rule]]\nstate = \"idle\"\ntitle = 'x'\n")
                        .unwrap_err();
                assert!(matches!(err, RulesError::Toml(_)), "{err}");
            }

            #[test]
            fn should_reject_a_state_no_rule_may_name() {
                let err = RuleSet::from_toml(
                    "agent = \"x\"\n[[rule]]\nstate = \"done\"\ntitle_regex = 'x'\n",
                )
                .unwrap_err();
                assert!(
                    err.to_string().contains("unknown rule state `done`"),
                    "{err}"
                );
            }

            #[test]
            fn should_name_the_rule_with_a_bad_regex() {
                let err = RuleSet::from_toml(
                    "agent = \"x\"\n[[rule]]\nstate = \"idle\"\ntitle_regex = 'x'\n[[rule]]\nstate = \"idle\"\ntitle_regex = '['\n",
                )
                .unwrap_err();
                assert!(
                    matches!(err, RulesError::TitleRegex { rule: 2, .. }),
                    "{err}"
                );
            }

            #[test]
            fn should_reject_rows_that_are_not_screen_or_bottom_n() {
                for bad in ["top:3", "bottom:0", "bottom:x", "bottom"] {
                    let text = format!(
                        "agent = \"x\"\n[[rule]]\nstate = \"idle\"\nrows = \"{bad}\"\ncontains = [\"a\"]\n"
                    );
                    let err = RuleSet::from_toml(&text).unwrap_err();
                    assert!(
                        matches!(err, RulesError::Rows { rule: 1, .. }),
                        "{bad}: {err}"
                    );
                }
            }

            #[test]
            fn should_reject_rows_with_nothing_to_look_for() {
                let err = RuleSet::from_toml(
                    "agent = \"x\"\n[[rule]]\nstate = \"idle\"\nrows = \"screen\"\n",
                )
                .unwrap_err();
                assert!(
                    matches!(err, RulesError::RowsWithoutContains { rule: 1 }),
                    "{err}"
                );
            }

            #[test]
            fn should_reject_a_rule_with_no_condition() {
                let err =
                    RuleSet::from_toml("agent = \"x\"\n[[rule]]\nstate = \"idle\"\n").unwrap_err();
                assert!(matches!(err, RulesError::NoCondition { rule: 1 }), "{err}");
            }
        }

        mod evaluate {
            use super::*;

            #[test]
            fn should_read_working_from_a_spinner_title() {
                assert_eq!(
                    spec_rules().evaluate(&titled("◐ Claude Code")),
                    AgentState::Working
                );
            }

            #[test]
            fn should_read_idle_from_the_idle_title() {
                assert_eq!(
                    spec_rules().evaluate(&titled("✳ Claude Code")),
                    AgentState::Idle
                );
            }

            #[test]
            fn should_take_the_first_matching_rule() {
                let observation = Observation {
                    rows: rows(&[
                        "Do you trust this folder?",
                        "Enter to confirm · Esc to cancel",
                    ]),
                    ..titled("✳ Claude Code")
                };
                assert_eq!(spec_rules().evaluate(&observation), AgentState::Blocked);
            }

            #[test]
            fn should_need_every_string_in_contains() {
                let observation = Observation {
                    rows: rows(&["Enter to confirm"]),
                    ..titled("✳ Claude Code")
                };
                assert_eq!(spec_rules().evaluate(&observation), AgentState::Idle);
            }

            #[test]
            fn should_count_bottom_rows_skipping_blank_ones() {
                let mut screen = rows(&["Enter to confirm", "Esc to cancel"]);
                screen.extend(rows(&["", "   ", "a", "b", "", "c", "d", "e", ""]));
                let observation = Observation {
                    rows: screen.clone(),
                    ..titled("✳ Claude Code")
                };
                // Five non-blank rows sit below the dialog text, so `bottom:6` holds only one
                // of its two strings.
                assert_eq!(spec_rules().evaluate(&observation), AgentState::Idle);

                screen.truncate(screen.len() - 5);
                let observation = Observation {
                    rows: screen,
                    ..titled("✳ Claude Code")
                };
                assert_eq!(spec_rules().evaluate(&observation), AgentState::Blocked);
            }

            #[test]
            fn should_match_osc_progress_and_program_status() {
                let rules = RuleSet::from_toml(
                    "agent = \"x\"\n\
                     [[rule]]\nstate = \"blocked\"\nprogram_status = [\"blocked\"]\n\
                     [[rule]]\nstate = \"working\"\nprogress = [\"normal\", \"indeterminate\"]\n",
                )
                .expect("parse");
                let blocked = Observation {
                    program_state: Some(ProgramState::Blocked),
                    progress: Some(Progress::Indeterminate),
                    ..Observation::default()
                };
                assert_eq!(rules.evaluate(&blocked), AgentState::Blocked);
                let working = Observation {
                    progress: Some(Progress::Normal),
                    ..Observation::default()
                };
                assert_eq!(rules.evaluate(&working), AgentState::Working);
                let removed = Observation {
                    progress: Some(Progress::Removed),
                    ..Observation::default()
                };
                assert_eq!(rules.evaluate(&removed), AgentState::Unknown);
            }

            #[test]
            fn should_need_every_condition_of_a_rule() {
                let rules = RuleSet::from_toml(
                    "agent = \"x\"\n[[rule]]\nstate = \"blocked\"\ntitle_regex = '^✳'\ncontains = [\"proceed?\"]\n",
                )
                .expect("parse");
                assert_eq!(
                    rules.evaluate(&titled("✳ Claude Code")),
                    AgentState::Unknown
                );
                let both = Observation {
                    rows: rows(&["Do you want to proceed?"]),
                    ..titled("✳ Claude Code")
                };
                assert_eq!(rules.evaluate(&both), AgentState::Blocked);
            }

            mod foreground_process {
                use super::*;

                #[test]
                fn should_skip_the_rules_while_another_process_is_in_front() {
                    let observation = Observation {
                        process: Some("vim".to_owned()),
                        ..titled("✳ Claude Code")
                    };
                    assert_eq!(spec_rules().evaluate(&observation), AgentState::Unknown);
                }

                #[test]
                fn should_apply_the_rules_to_a_listed_process() {
                    let observation = Observation {
                        process: Some("claude".to_owned()),
                        ..titled("✳ Claude Code")
                    };
                    assert_eq!(spec_rules().evaluate(&observation), AgentState::Idle);
                }

                #[test]
                fn should_apply_the_rules_when_the_process_is_unknown() {
                    assert_eq!(
                        spec_rules().evaluate(&titled("✳ Claude Code")),
                        AgentState::Idle
                    );
                }
            }

            mod no_match {
                use super::*;

                #[test]
                fn should_be_unknown_without_output() {
                    assert_eq!(spec_rules().evaluate(&titled("zsh")), AgentState::Unknown);
                }

                #[test]
                fn should_be_working_with_output_in_the_last_two_seconds() {
                    let observation = Observation {
                        since_output: Some(Duration::from_millis(1_999)),
                        ..titled("zsh")
                    };
                    assert_eq!(spec_rules().evaluate(&observation), AgentState::Working);
                }

                #[test]
                fn should_be_unknown_once_output_is_two_seconds_old() {
                    let observation = Observation {
                        since_output: Some(RECENT_OUTPUT),
                        ..titled("zsh")
                    };
                    assert_eq!(spec_rules().evaluate(&observation), AgentState::Unknown);
                }
            }
        }
    }

    mod last_output {
        use super::*;

        fn no_rules() -> RuleSet {
            RuleSet::from_toml("agent = \"shell\"").expect("parse")
        }

        /// The state `output` makes a pane with no rules read as at `now`.
        fn state_at(output: &LastOutput, now: Duration) -> AgentState {
            no_rules().evaluate(&Observation {
                since_output: output.since(now),
                ..titled("zsh")
            })
        }

        fn ms(millis: u64) -> Duration {
            Duration::from_millis(millis)
        }

        #[test]
        fn should_count_an_agent_panes_output_from_the_start() {
            let mut output = LastOutput::agent();
            output.printed(ms(50));
            assert_eq!(output.since(ms(150)), Some(ms(100)));
            assert_eq!(state_at(&output, ms(150)), AgentState::Working);
        }

        #[test]
        fn should_be_none_before_anything_counts() {
            assert_eq!(LastOutput::agent().since(ms(10)), None);
        }

        mod resize {
            use super::*;

            #[test]
            fn should_not_count_the_redraw_right_after_a_resize() {
                let mut output = LastOutput::agent();
                output.resized(ms(5_000));
                output.printed(ms(5_050));
                assert_eq!(state_at(&output, ms(5_100)), AgentState::Unknown);
            }

            #[test]
            fn should_count_output_400_ms_after_a_resize() {
                let mut output = LastOutput::agent();
                output.resized(ms(5_000));
                output.printed(ms(5_400));
                assert_eq!(state_at(&output, ms(5_450)), AgentState::Working);
            }

            #[test]
            fn should_keep_output_from_before_the_resize() {
                let mut output = LastOutput::agent();
                output.printed(ms(4_900));
                output.resized(ms(5_000));
                output.printed(ms(5_050));
                assert_eq!(output.since(ms(5_100)), Some(ms(200)));
            }
        }

        mod shell {
            use super::*;

            #[test]
            fn should_not_count_the_prompt_a_new_shell_prints() {
                let mut output = LastOutput::shell();
                output.printed(ms(30));
                output.printed(ms(900));
                assert_eq!(state_at(&output, ms(1_000)), AgentState::Unknown);
            }

            #[test]
            fn should_count_output_once_something_is_typed() {
                let mut output = LastOutput::shell();
                output.printed(ms(30));
                output.typed();
                output.printed(ms(3_000));
                assert_eq!(state_at(&output, ms(3_100)), AgentState::Working);
            }

            #[test]
            fn should_not_count_a_redraw_after_a_resize_once_typed_into() {
                let mut output = LastOutput::shell();
                output.typed();
                output.resized(ms(5_000));
                output.printed(ms(5_200));
                assert_eq!(state_at(&output, ms(5_300)), AgentState::Unknown);
            }

            /// Whether the shell itself or a command it runs is in the pane's foreground, as
            /// the daemon reads it on each tick (spec §7).
            mod foreground {
                use super::*;
                use crate::state::StateTracker;

                /// A shell that has been typed into, so only the foreground decides.
                fn typed_into() -> LastOutput {
                    let mut output = LastOutput::shell();
                    output.typed();
                    output
                }

                #[test]
                fn should_not_count_what_the_shell_prints_while_it_is_in_front() {
                    let mut output = typed_into();
                    output.shell_in_front(true);

                    output.printed(ms(1_000));

                    assert_eq!(state_at(&output, ms(1_100)), AgentState::Unknown);
                }

                #[test]
                fn should_count_what_a_command_prints_while_it_is_in_front() {
                    let mut output = typed_into();
                    output.shell_in_front(false);

                    output.printed(ms(1_000));

                    assert_eq!(state_at(&output, ms(1_100)), AgentState::Working);
                }

                #[test]
                fn should_count_a_commands_output_after_the_shell_was_in_front() {
                    let mut output = typed_into();
                    output.shell_in_front(true);
                    output.printed(ms(1_000));

                    output.shell_in_front(false);
                    output.printed(ms(2_000));

                    assert_eq!(state_at(&output, ms(2_100)), AgentState::Working);
                }

                #[test]
                fn should_drop_what_a_command_printed_once_the_shell_is_back_in_front() {
                    let mut output = typed_into();
                    output.shell_in_front(false);
                    output.printed(ms(1_000));

                    output.shell_in_front(true);

                    assert_eq!(output.since(ms(1_100)), None);
                }

                /// The states a shell publishes over the 3 s after an agent it hosted exits:
                /// the agent's last output, then the shell's prompt, reach the pane on either
                /// side of the tick that sees the shell back in front. Ticks are 100 ms apart,
                /// as the daemon's are.
                fn published_after_an_agent_exits(prompt_before_the_tick: bool) -> Vec<AgentState> {
                    let mut output = typed_into();
                    let mut tracker = StateTracker::new(no_rules());
                    output.shell_in_front(false);
                    output.printed(ms(1_000));
                    if prompt_before_the_tick {
                        output.printed(ms(1_050));
                    }
                    output.shell_in_front(true);
                    if !prompt_before_the_tick {
                        output.printed(ms(1_150));
                    }
                    (1..=30)
                        .filter_map(|tick| {
                            let now = ms(1_000 + tick * 100);
                            let observation = Observation {
                                since_output: output.since(now),
                                ..titled("zsh")
                            };
                            tracker.observe(&observation, now)
                        })
                        .collect()
                }

                #[test]
                fn should_not_read_working_when_the_prompt_comes_back_after_the_switch() {
                    assert_eq!(published_after_an_agent_exits(false), []);
                }

                #[test]
                fn should_not_read_working_when_the_prompt_comes_back_before_the_switch() {
                    assert_eq!(published_after_an_agent_exits(true), []);
                }
            }
        }
    }
}
