//! What a program can send besides its title that `alacritty_terminal` ignores: OSC 9;4
//! progress and OSC 7501 program status, for the agent state, and the OSC 7 working directory,
//! for the attach snapshot. [`OscScanner`] reads them from the raw output and keeps the latest
//! of each.

use crate::text_enum::text_enum;

text_enum! {
    /// The state of an OSC 9;4 progress report (`ESC ] 9 ; 4 ; <state> ; <percent> ST`).
    pub enum Progress ("progress state") {
        /// `0`: the program removed its progress.
        Removed => "removed",
        /// `1`: progress at a percentage.
        Normal => "normal",
        /// `2`: progress in an error state.
        Error => "error",
        /// `3`: busy, with no percentage.
        Indeterminate => "indeterminate",
        /// `4`: progress paused.
        Paused => "paused",
    }
}

text_enum! {
    /// A program's own status, from OSC 7501 (`ESC ] 7501 ; state=<s>:kind=…:app=… ST`).
    pub enum ProgramState ("program state") {
        /// Waiting for input.
        Idle => "idle",
        /// Busy.
        Working => "working",
        /// Finished its work.
        Done => "done",
        /// Waiting on the user (a permission, a question, a login).
        Blocked => "blocked",
        /// Failed.
        Error => "error",
    }
}

/// Reads OSC 9;4, OSC 7501 and OSC 7 out of a program's output, across any chunking.
#[derive(Default)]
pub struct OscScanner {
    parser: vte::Parser,
    latest: Latest,
}

#[derive(Default)]
struct Latest {
    progress: Option<Progress>,
    program_state: Option<ProgramState>,
    cwd: Option<String>,
}

impl OscScanner {
    /// A scanner that has seen nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads the next chunk of output.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.latest, bytes);
    }

    /// The last OSC 9;4 state, or `None` if the program never sent one.
    pub fn progress(&self) -> Option<Progress> {
        self.latest.progress
    }

    /// The `file://` URI of the working directory the shell last reported with OSC 7
    /// (`ESC ] 7 ; file://host/path ST`), as it was sent; `None` until it reports one.
    pub fn reported_cwd(&self) -> Option<&str> {
        self.latest.cwd.as_deref()
    }

    /// The last OSC 7501 state, or `None` if the program never sent one or cleared it.
    pub fn program_state(&self) -> Option<ProgramState> {
        self.latest.program_state
    }
}

impl vte::Perform for Latest {
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        match params {
            [b"9", b"4", state, ..] => {
                if let Some(progress) = progress_from_code(state) {
                    self.progress = Some(progress);
                }
            }
            [b"7501", fields, ..] => {
                let Some(state) = program_state_field(fields) else {
                    return;
                };
                if state == "clear" {
                    self.program_state = None;
                } else if let Ok(state) = state.parse() {
                    self.program_state = Some(state);
                }
            }
            [b"7", uri @ ..] => {
                // A `;` in the URI splits it into more params.
                let uri = uri.join(&b';');
                if let Ok(uri) = String::from_utf8(uri)
                    && !uri.is_empty()
                {
                    self.cwd = Some(uri);
                }
            }
            _ => {}
        }
    }
}

fn progress_from_code(code: &[u8]) -> Option<Progress> {
    Some(match code {
        b"0" => Progress::Removed,
        b"1" => Progress::Normal,
        b"2" => Progress::Error,
        b"3" => Progress::Indeterminate,
        b"4" => Progress::Paused,
        _ => return None,
    })
}

/// The `state=` value of an OSC 7501 field list (`key=value` pairs joined by `:`).
fn program_state_field(fields: &[u8]) -> Option<&str> {
    std::str::from_utf8(fields)
        .ok()?
        .split(':')
        .find_map(|field| field.strip_prefix("state="))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scanned(chunks: &[&[u8]]) -> OscScanner {
        let mut scanner = OscScanner::new();
        for chunk in chunks {
            scanner.feed(chunk);
        }
        scanner
    }

    mod osc_scanner {
        use super::*;

        mod progress {
            use super::*;

            #[test]
            fn should_be_none_before_any_report() {
                assert_eq!(scanned(&[b"plain output\r\n"]).progress(), None);
            }

            #[test]
            fn should_read_a_bel_terminated_report() {
                let scanner = scanned(&[b"\x1b]9;4;3;0\x07"]);
                assert_eq!(scanner.progress(), Some(Progress::Indeterminate));
            }

            #[test]
            fn should_read_an_st_terminated_report() {
                let scanner = scanned(&[b"\x1b]9;4;1;42\x1b\\"]);
                assert_eq!(scanner.progress(), Some(Progress::Normal));
            }

            #[test]
            fn should_keep_the_latest_report() {
                let scanner = scanned(&[b"\x1b]9;4;1;10\x07", b"\x1b]9;4;0;0\x07"]);
                assert_eq!(scanner.progress(), Some(Progress::Removed));
            }

            #[test]
            fn should_read_a_report_split_across_chunks() {
                let scanner = scanned(&[b"text \x1b]9;", b"4;2", b";5\x07 more"]);
                assert_eq!(scanner.progress(), Some(Progress::Error));
            }

            #[test]
            fn should_ignore_a_plain_osc_9_notification() {
                assert_eq!(scanned(&[b"\x1b]9;build finished\x07"]).progress(), None);
            }

            #[test]
            fn should_ignore_an_unknown_progress_state() {
                let scanner = scanned(&[b"\x1b]9;4;4;0\x07", b"\x1b]9;4;7;0\x07"]);
                assert_eq!(scanner.progress(), Some(Progress::Paused));
            }
        }

        mod reported_cwd {
            use super::*;

            #[test]
            fn should_be_none_until_the_shell_reports_one() {
                assert_eq!(scanned(&[b"$ "]).reported_cwd(), None);
            }

            #[test]
            fn should_keep_the_last_osc_7_however_it_is_chunked() {
                let scanner = scanned(&[
                    b"\x1b]7;file://box/first\x07$ ",
                    b"\x1b]7;file://box/w/re",
                    b"po\x1b\\$ ",
                ]);
                assert_eq!(scanner.reported_cwd(), Some("file://box/w/repo"));
            }

            #[test]
            fn should_keep_a_semicolon_in_the_uri() {
                let scanner = scanned(&[b"\x1b]7;file://box/a;b\x07"]);
                assert_eq!(scanner.reported_cwd(), Some("file://box/a;b"));
            }

            #[test]
            fn should_ignore_an_empty_report() {
                let scanner = scanned(&[b"\x1b]7;file://box/w\x07", b"\x1b]7;\x07"]);
                assert_eq!(scanner.reported_cwd(), Some("file://box/w"));
            }
        }

        mod program_state {
            use super::*;

            #[test]
            fn should_read_the_state_field() {
                let scanner =
                    scanned(&[b"\x1b]7501;state=blocked:kind=permission:app=claude\x1b\\"]);
                assert_eq!(scanner.program_state(), Some(ProgramState::Blocked));
            }

            #[test]
            fn should_read_the_state_wherever_it_sits() {
                let scanner = scanned(&[b"\x1b]7501;app=claude:state=working\x07"]);
                assert_eq!(scanner.program_state(), Some(ProgramState::Working));
            }

            #[test]
            fn should_forget_the_state_on_clear() {
                let scanner =
                    scanned(&[b"\x1b]7501;state=working\x07", b"\x1b]7501;state=clear\x07"]);
                assert_eq!(scanner.program_state(), None);
            }

            #[test]
            fn should_ignore_a_query() {
                // Claude Code sends this probe at startup (spike #2) and nothing after it.
                let scanner = scanned(&[b"\x1b]7501;state=idle\x07", b"\x1b]7501;?\x07"]);
                assert_eq!(scanner.program_state(), Some(ProgramState::Idle));
            }
        }
    }
}
