//! Stub `claude` and `codex` CLIs on the daemon's `PATH`, ahead of any real one, so no test
//! ever runs a real agent. Each records what it was launched with (argv and
//! `$AGENTDECK_TASK`) and what is typed into it under `<root>/agents/<pane id>/`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use agentdeck_core::{Agent, PaneId};

use super::TestDaemon;

/// Records its argv and `$AGENTDECK_TASK`; when its folder holds a file named
/// `report-back`, comments `reported back` on that task through the MCP server its
/// `--mcp-config` names, as an agent would, and records the server's answers in `mcp`; shows
/// a stand-in for the folder trust dialog when its folder holds a file named `untrusted`, and
/// waits for a line there; then sets the idle title and reads what is typed as
/// [`READ_LIKE_A_TUI`] does. It stays in the foreground (no `exec`), so the pane's foreground
/// process is `claude` and the claude rules apply.
const STUB_CLAUDE: &str = r#"#!/bin/sh
log="$(dirname "$0")/../agents/$AGENTDECK_PANE"
mkdir -p "$log"
printf '%s' "${AGENTDECK_TASK-unset}" > "$log/task"
{ for arg in "$@"; do printf '%s\0' "$arg"; done; printf '\036'; } >> "$log/argv"
if [ -e report-back ]; then
  config=; previous=
  for arg in "$@"; do [ "$previous" = --mcp-config ] && config=$arg; previous=$arg; done
  server=$(printf '%s' "$config" | sed 's/.*"command":"\([^"]*\)".*/\1/')
  {
    printf '%s\n' \
      '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"stub-claude","version":"0"}}}' \
      '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
      "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"task_comment\",\"arguments\":{\"task\":\"$AGENTDECK_TASK\",\"body\":\"reported back\"}}}"
    until grep -q '"id":2' "$log/mcp" 2>/dev/null; do sleep 0.1; done
  } | "$server" mcp > "$log/mcp" 2> "$log/mcp.stderr"
fi
if [ -e untrusted ]; then
  printf 'Do you trust the files in this folder?\r\n\r\n> 1. Yes, proceed\r\n  2. No, exit\r\n\r\nEnter to confirm \302\267 Esc to cancel\r\n'
  read -r answer
  printf '%s\n' "$answer" > "$log/trust"
  printf '\033[2J\033[H'
fi
stty raw -echo
printf '\033[?2004h\033]0;\342\234\263 Claude Code\007> '
"#;

/// Records its argv and `$AGENTDECK_TASK`; unless resuming (a `resume` argument), writes a
/// session file as Codex does, under `$CODEX_HOME/sessions` (default `$HOME/.codex/sessions`),
/// and records the id in `session`; then reads what is typed as [`READ_LIKE_A_TUI`] does.
const STUB_CODEX: &str = r#"#!/bin/sh
log="$(dirname "$0")/../agents/$AGENTDECK_PANE"
mkdir -p "$log"
printf '%s' "${AGENTDECK_TASK-unset}" > "$log/task"
{ for arg in "$@"; do printf '%s\0' "$arg"; done; printf '\036'; } >> "$log/argv"
resuming=
for arg in "$@"; do [ "$arg" = resume ] && resuming=1; done
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

/// Every prompt the pane's stub agent submitted, each ended by `\n`, after any trust dialog.
pub fn typed(daemon: &TestDaemon, pane: PaneId) -> String {
    std::fs::read_to_string(log(daemon, pane).join("stdin")).unwrap_or_default()
}

/// The answer typed into the stub claude's trust dialog, if it was answered.
pub fn trust_answer(daemon: &TestDaemon, pane: PaneId) -> Option<String> {
    std::fs::read_to_string(log(daemon, pane).join("trust")).ok()
}

/// `$AGENTDECK_TASK` as the pane's stub agent last started with it: `unset` when it wasn't
/// set; `None` before the stub has started. Each stub writes it before its argv, so it is
/// there once [`launches`] has the launch.
pub fn task_var(daemon: &TestDaemon, pane: PaneId) -> Option<String> {
    std::fs::read_to_string(log(daemon, pane).join("task")).ok()
}

/// The arguments registering the `agentdeck` beside the daemon under test as an agent's MCP
/// server, as the agent's stub sees them: right after the program.
pub fn mcp_flags(agent: Agent) -> Vec<String> {
    let cli = super::cli_binary();
    let cli = cli.to_str().expect("a UTF-8 target dir");
    match agent {
        Agent::Claude => vec![
            "--mcp-config".into(),
            format!(r#"{{"mcpServers":{{"agentdeck":{{"command":"{cli}","args":["mcp"]}}}}}}"#),
        ],
        Agent::Codex => [
            format!(r#"mcp_servers.agentdeck.command="{cli}""#),
            r#"mcp_servers.agentdeck.args=["mcp"]"#.to_owned(),
            "mcp_servers.agentdeck.env_vars=[\"XDG_RUNTIME_DIR\", \"XDG_CONFIG_HOME\", \
             \"XDG_DATA_HOME\", \"XDG_STATE_HOME\", \"AGENTDECK_PANE\", \
             \"AGENTDECK_WORKSPACE\", \"AGENTDECK_TASK\"]"
                .to_owned(),
        ]
        .into_iter()
        .flat_map(|value| ["-c".to_owned(), value])
        .collect(),
        Agent::Other => Vec::new(),
    }
}

/// [`mcp_flags`] for `agent`, then `rest`.
pub fn with_mcp(agent: Agent, rest: &[&str]) -> Vec<String> {
    let mut argv = mcp_flags(agent);
    argv.extend(rest.iter().map(|arg| (*arg).to_owned()));
    argv
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
