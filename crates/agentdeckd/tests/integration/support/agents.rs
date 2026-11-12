//! Stub `claude` and `codex` CLIs on the daemon's `PATH`, ahead of any real one, so no test
//! ever runs a real agent. Each records what it was launched with and what is typed into it
//! under `<root>/agents/<pane id>/`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use agentdeck_core::PaneId;

use super::TestDaemon;

/// Records its argv; shows a stand-in for the folder trust dialog when its folder holds a
/// file named `untrusted`, and waits for a line there; then sets the idle title and appends
/// every line typed to `stdin`. It stays in the foreground (no `exec`), so the pane's
/// foreground process is `claude` and the claude rules apply.
const STUB_CLAUDE: &str = r#"#!/bin/sh
log="$(dirname "$0")/../agents/$AGENTDECK_PANE"
mkdir -p "$log"
{ for arg in "$@"; do printf '%s\0' "$arg"; done; printf '\036'; } >> "$log/argv"
if [ -e untrusted ]; then
  printf 'Do you trust the files in this folder?\r\n\r\n> 1. Yes, proceed\r\n  2. No, exit\r\n\r\nEnter to confirm \302\267 Esc to cancel\r\n'
  read -r answer
  printf '%s\n' "$answer" > "$log/trust"
  printf '\033[2J\033[H'
fi
printf '\033]0;\342\234\263 Claude Code\007> '
cat >> "$log/stdin"
"#;

/// Records its argv; unless resuming, writes a session file as Codex does, under
/// `$CODEX_HOME/sessions` (default `$HOME/.codex/sessions`), and records the id in
/// `session`; then appends every line typed to `stdin`.
const STUB_CODEX: &str = r#"#!/bin/sh
log="$(dirname "$0")/../agents/$AGENTDECK_PANE"
mkdir -p "$log"
{ for arg in "$@"; do printf '%s\0' "$arg"; done; printf '\036'; } >> "$log/argv"
if [ "$1" != resume ]; then
  id=$(cat /proc/sys/kernel/random/uuid)
  dir="${CODEX_HOME:-$HOME/.codex}/sessions/2026/11/11"
  mkdir -p "$dir"
  : > "$dir/rollout-2026-11-11T09-00-00-$id.jsonl"
  printf '%s' "$id" > "$log/session"
fi
printf '\033]0;codex\007'
cat >> "$log/stdin"
"#;

/// Writes the stub agents into `bin`, the dir the stub `gh` is in.
pub fn install(bin: &Path) {
    for (name, script) in [("claude", STUB_CLAUDE), ("codex", STUB_CODEX)] {
        let path = bin.join(name);
        std::fs::write(&path, script).expect("writing a stub agent");
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
    let text = String::from_utf8(bytes).expect("UTF-8 argv");
    text.split_terminator('\u{1e}')
        .map(|record| record.split_terminator('\0').map(str::to_owned).collect())
        .collect()
}

/// What has been typed into the pane's stub agent, after any trust dialog.
pub fn typed(daemon: &TestDaemon, pane: PaneId) -> String {
    std::fs::read_to_string(log(daemon, pane).join("stdin")).unwrap_or_default()
}

/// The answer typed into the stub claude's trust dialog, if it was answered.
pub fn trust_answer(daemon: &TestDaemon, pane: PaneId) -> Option<String> {
    std::fs::read_to_string(log(daemon, pane).join("trust")).ok()
}

/// The session id the pane's stub codex wrote a session file for.
pub fn codex_session(daemon: &TestDaemon, pane: PaneId) -> Option<String> {
    std::fs::read_to_string(log(daemon, pane).join("session")).ok()
}
