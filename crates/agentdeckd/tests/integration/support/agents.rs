//! Stub `claude` and `codex` CLIs on the daemon's `PATH`, ahead of any real one, so no test
//! ever runs a real agent. Each records what it was launched with (argv and
//! `$AGENTDECK_TASK`) and what is typed into it under `<root>/agents/<pane id>/`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use agentdeck_core::PaneId;

use super::TestDaemon;

/// Records its argv and `$AGENTDECK_TASK`; then sets the idle title and reads what is typed as
/// [`READ_LIKE_A_TUI`] does. It stays in the foreground (no `exec`), so the pane's foreground
/// process is `claude` and the claude rules apply.
///
/// When its folder holds a file named `lost-session`, a resume (`--resume <id>`) prints what
/// Claude Code prints for a session it can't find and exits 1 at once; with a file named
/// `broken`, every start does.
const STUB_CLAUDE: &str = r#"#!/bin/sh
log="$(dirname "$0")/../agents/$AGENTDECK_PANE"
mkdir -p "$log"
printf '%s' "${AGENTDECK_TASK-unset}" > "$log/task"
{ for arg in "$@"; do printf '%s\0' "$arg"; done; printf '\036'; } >> "$log/argv"
session=; previous=
for arg in "$@"; do [ "$previous" = --resume ] && session=$arg; previous=$arg; done
if [ -e broken ] || { [ -n "$session" ] && [ -e lost-session ]; }; then
  printf 'No conversation found with session ID: %s\r\n' "$session"
  exit 1
fi
stty raw -echo
printf '\033[?2004h\033]0;\342\234\263 Claude Code\007> '
"#;

/// Records its argv and `$AGENTDECK_TASK`; unless resuming (a `resume` argument), writes a
/// session file as Codex does, under `$CODEX_HOME/sessions` (default `$HOME/.codex/sessions`),
/// and records the id in `session`; then reads what is typed as [`READ_LIKE_A_TUI`] does.
///
/// When its folder holds a file named `lost-session`, a resume (`resume <id>`) does what
/// codex-cli 0.169.0 does for a session it can't find (#262): prints its error, after its
/// TUI's first frame, and exits 1 within a second, rather than showing it in a TUI that stays.
const STUB_CODEX: &str = r#"#!/bin/sh
log="$(dirname "$0")/../agents/$AGENTDECK_PANE"
mkdir -p "$log"
printf '%s' "${AGENTDECK_TASK-unset}" > "$log/task"
{ for arg in "$@"; do printf '%s\0' "$arg"; done; printf '\036'; } >> "$log/argv"
resuming=; session=; previous=
for arg in "$@"; do
  [ "$arg" = resume ] && resuming=1
  [ "$previous" = resume ] && session=$arg
  previous=$arg
done
if [ -n "$session" ] && [ -e lost-session ]; then
  printf '>_ OpenAI Codex\r\nResuming session\342\200\246\r\n'
  printf 'ERROR: No saved session found with ID %s. Run `codex resume` without an ID to choose from existing sessions.\r\n' "$session"
  exit 1
fi
if [ -z "$resuming" ]; then
  id=$(cat /proc/sys/kernel/random/uuid)
  dir="${CODEX_HOME:-$HOME/.codex}/sessions/2026/11/11"
  mkdir -p "$dir"
  : > "$dir/rollout-2026-11-11T09-00-00-$id.jsonl"
  printf '%s' "$id" > "$log/session"
fi
stty raw -echo
printf '\033[?2004h\033]0;codex\007'
"#;

/// The end of both stubs: reads what is typed as Claude Code's and Codex's TUIs do. Each stub
/// turns raw mode and bracketed paste on before its idle title, so nothing reaches it
/// earlier, cooked. A read that is a lone CR is Enter: it submits the prompt, appending it and
/// a newline to `stdin`. Any other read, CRs included, is a paste: it goes into the prompt,
/// less the paste's brackets, and submits nothing, so text typed with its Enter in one write
/// is never submitted (issue #184). A submitted `/exit` ends it as it ends Claude Code and
/// Codex, handing the terminal back cooked, so a shell it was started from carries on (#183).
/// Its twin is in `agentdeck/tests/integration/support`.
const READ_LIKE_A_TUI: &str = r#"
: > "$log/prompt"
while dd bs=65536 count=1 of="$log/read" 2>/dev/null && [ -s "$log/read" ]; do
  if [ "$(od -An -tx1 "$log/read" | tr -d ' \n')" = 0d ]; then
    [ "$(cat "$log/prompt")" = /exit ] && break
    { cat "$log/prompt"; echo; } >> "$log/stdin"
    : > "$log/prompt"
  else
    sed 's/\x1b\[20[01]~//g' "$log/read" >> "$log/prompt"
  fi
done
stty sane 2>/dev/null
printf '\033[?2004l\033]0;\007' 2>/dev/null
"#;

/// Writes the stub agents into `bin`, the dir the stub `gh` is in.
pub fn install(bin: &Path) {
    for (name, script) in [("claude", STUB_CLAUDE), ("codex", STUB_CODEX)] {
        let path = bin.join(name);
        std::fs::write(&path, format!("{script}{READ_LIKE_A_TUI}")).expect("writing a stub agent");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("making a stub agent executable");
    }
}

fn log(daemon: &TestDaemon, pane: PaneId) -> PathBuf {
    daemon.root().join("agents").join(pane.to_string())
}

/// Every argv the pane's stub agent was started with (program name left out), oldest first.
pub fn launches(daemon: &TestDaemon, pane: PaneId) -> Vec<Vec<String>> {
    let bytes = std::fs::read(log(daemon, pane).join("argv")).unwrap_or_default();
    argv_records(&String::from_utf8(bytes).expect("UTF-8 argv"))
}

/// The complete records in a stub's argv file. Whatever follows the last `\036` is a launch
/// the stub is still writing, so it is left out until its terminator lands.
fn argv_records(text: &str) -> Vec<Vec<String>> {
    let complete = text.rfind('\u{1e}').map_or("", |end| &text[..=end]);
    complete
        .split_terminator('\u{1e}')
        .map(|record| record.split_terminator('\0').map(str::to_owned).collect())
        .collect()
}

/// Every prompt the pane's stub agent submitted, each ended by `\n`.
pub fn typed(daemon: &TestDaemon, pane: PaneId) -> String {
    std::fs::read_to_string(log(daemon, pane).join("stdin")).unwrap_or_default()
}

/// `$AGENTDECK_TASK` as the pane's stub agent last started with it: `unset` when it wasn't
/// set; `None` before the stub has started. Each stub writes it before its argv, so it is
/// there once [`launches`] has the launch.
pub fn task_var(daemon: &TestDaemon, pane: PaneId) -> Option<String> {
    std::fs::read_to_string(log(daemon, pane).join("task")).ok()
}

/// An argv as the stub agents record it, program name left out.
pub fn args(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

/// The session id the pane's stub codex wrote a session file for.
pub fn codex_session(daemon: &TestDaemon, pane: PaneId) -> Option<String> {
    std::fs::read_to_string(log(daemon, pane).join("session")).ok()
}

mod tests {
    use super::*;

    mod argv_records {
        use super::*;

        #[test]
        fn should_leave_out_a_record_the_stub_is_still_writing() {
            assert_eq!(
                argv_records("a\0\u{1e}--session-id\0"),
                vec![vec!["a".to_owned()]]
            );
        }

        #[test]
        fn should_give_every_terminated_record_oldest_first() {
            assert_eq!(
                argv_records("a\0\u{1e}\u{1e}"),
                vec![vec!["a".to_owned()], Vec::new()]
            );
        }
    }
}
