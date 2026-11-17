//! The integration harness: the real `agentdeck` binary in temp `$XDG_*` dirs, against the
//! real `agentdeckd` that it starts itself (spec §2, §12). Nothing reaches the real GitHub:
//! `gh` is a logged-out stub and the GraphQL URL points at a closed port, unless a test
//! points it at httpmock with [`TestEnv::with_github`]. `claude` and `codex` are stubs first
//! on `PATH`, so no test runs a real agent.

pub mod github;
pub mod mcp;
pub mod pty;

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use agentdeck_core::DaemonInfo;
use serde_json::Value;
use tempfile::TempDir;

/// How long a daemon gets to exit after SIGTERM.
const TIMEOUT: Duration = Duration::from_secs(10);

/// A GraphQL endpoint nothing listens on.
const UNREACHABLE: &str = "http://127.0.0.1:9/graphql";

/// A stub `claude`: records its argv under `<root>/agents/<pane>/argv` (NUL-separated, ended
/// by `\036` once complete, as the redirect creates the file before anything is in it), sets
/// Claude Code's idle title, then reads what is typed as [`READ_LIKE_A_TUI`] does. Stays in
/// the foreground, so the claude rules apply.
const STUB_CLAUDE: &str = r#"#!/bin/sh
log="$(dirname "$0")/../agents/$AGENTDECK_PANE"
mkdir -p "$log"
{ for arg in "$@"; do printf '%s\0' "$arg"; done; printf '\036'; } > "$log/argv"
stty raw -echo
printf '\033[?2004h\033]0;\342\234\263 Claude Code\007> '
"#;

/// A stub `codex`: records its argv, writes a session file as Codex does, then reads what is
/// typed as [`READ_LIKE_A_TUI`] does.
const STUB_CODEX: &str = r#"#!/bin/sh
log="$(dirname "$0")/../agents/$AGENTDECK_PANE"
mkdir -p "$log"
{ for arg in "$@"; do printf '%s\0' "$arg"; done; printf '\036'; } > "$log/argv"
id=$(cat /proc/sys/kernel/random/uuid)
dir="${CODEX_HOME:-$HOME/.codex}/sessions/2026/11/12"
mkdir -p "$dir"
: > "$dir/rollout-2026-11-12T09-00-00-$id.jsonl"
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
/// Its twin is in `agentdeckd/tests/integration/support`.
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

/// A `gh` that is never logged in.
const STUB_GH: &str = "#!/bin/sh\necho \"no oauth token found for github.com\" >&2\nexit 1\n";

/// A `gh` whose `auth token` prints `<root>/gh-token`.
const STUB_GH_LOGGED_IN: &str = "#!/bin/sh\ncat \"$(dirname \"$0\")/../gh-token\"\n";

/// The `agentdeckd` beside the `agentdeck` under test. `cargo test-integration` builds every
/// workspace binary, so a missing one is a build problem, not a reason to skip.
pub fn daemon_binary() -> PathBuf {
    let cli = Path::new(env!("CARGO_BIN_EXE_agentdeck"));
    let daemon = cli.with_file_name("agentdeckd");
    assert!(
        daemon.is_file(),
        "{} is missing; build the workspace (`cargo build --workspace`) first",
        daemon.display()
    );
    daemon
}

/// One user's world: `$HOME` and every `$XDG_*` dir under a temp root. Dropping it stops the
/// daemon any command started, then removes the dirs, even when the test failed.
pub struct TestEnv {
    // Removed after Drop has stopped the daemon.
    root: TempDir,
    /// The GraphQL URL the daemon calls, when a test set one with [`TestEnv::with_github`].
    github_api: Option<String>,
}

/// What one `agentdeck` run did.
pub struct Run {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl From<Output> for Run {
    fn from(output: Output) -> Run {
        Run {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

impl TestEnv {
    /// A fresh world with no daemon running.
    pub fn new() -> TestEnv {
        let root = tempfile::tempdir().expect("a temp dir");
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).expect("creating the stub bin dir");
        for (name, script) in [
            ("gh", STUB_GH),
            ("claude", STUB_CLAUDE),
            ("codex", STUB_CODEX),
        ] {
            let path = bin.join(name);
            let script = match name {
                "gh" => script.to_owned(),
                _ => format!("{script}{READ_LIKE_A_TUI}"),
            };
            std::fs::write(&path, script).expect("writing a stub");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("making a stub executable");
        }
        std::fs::create_dir_all(root.path().join("home")).expect("creating $HOME");
        TestEnv {
            root,
            github_api: None,
        }
    }

    /// A fresh world whose `gh` is logged in with `token` and whose daemon calls GitHub's
    /// GraphQL API at `github_api` (httpmock), with no PR age cutoff.
    pub fn with_github(github_api: &str, token: &str) -> TestEnv {
        let mut env = TestEnv::new();
        env.github_api = Some(github_api.to_owned());
        let gh = env.root().join("bin/gh");
        std::fs::write(&gh, STUB_GH_LOGGED_IN).expect("writing the stub gh");
        std::fs::write(env.root().join("gh-token"), token).expect("writing the gh token");
        env
    }

    /// The temp root.
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// `$XDG_RUNTIME_DIR/agentdeck/daemon.json`.
    pub fn daemon_json(&self) -> PathBuf {
        self.root().join("run/agentdeck/daemon.json")
    }

    /// What `daemon.json` holds, if it exists.
    pub fn daemon_info(&self) -> Option<DaemonInfo> {
        let text = std::fs::read_to_string(self.daemon_json()).ok()?;
        Some(serde_json::from_str(&text).expect("daemon.json parses"))
    }

    /// Writes `daemon.json` as if a daemon had, for the stale-file cases.
    pub fn write_daemon_info(&self, info: &DaemonInfo) {
        let path = self.daemon_json();
        std::fs::create_dir_all(path.parent().unwrap()).expect("creating the runtime dir");
        std::fs::write(path, serde_json::to_string(info).unwrap()).expect("writing daemon.json");
    }

    /// Takes the daemon's lock, as a daemon that is still shutting down holds it after it
    /// withdrew `daemon.json`. Released when the file is dropped.
    pub fn hold_lock(&self) -> std::fs::File {
        let dir = self.root().join("run/agentdeck");
        std::fs::create_dir_all(&dir).expect("creating the runtime dir");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("lock"))
            .expect("opening the lock file");
        lock.try_lock().expect("taking the daemon's lock");
        lock
    }

    /// The daemon's log, `$XDG_STATE_HOME/agentdeck/agentdeckd.log`; empty until it exists.
    pub fn daemon_log(&self) -> String {
        std::fs::read_to_string(self.root().join("state/agentdeck/agentdeckd.log"))
            .unwrap_or_default()
    }

    /// A fresh directory under the temp root.
    pub fn new_dir(&self, name: &str) -> PathBuf {
        let dir = self.root().join("dirs").join(name);
        std::fs::create_dir_all(&dir).expect("creating a dir");
        dir
    }

    /// `agentdeck` with only `PATH` from the test's environment, behind the stub `gh`; `$HOME`
    /// and the `$XDG_*` dirs under the root; and the daemon under test as `agentdeckd`.
    pub fn command(&self) -> Command {
        let root = self.root();
        let mut path = vec![root.join("bin")];
        path.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let mut command = Command::new(env!("CARGO_BIN_EXE_agentdeck"));
        command
            .env_clear()
            .env("PATH", std::env::join_paths(path).expect("a PATH"))
            .env("HOME", root.join("home"))
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "AGENTDECK_GITHUB_API",
                self.github_api.as_deref().unwrap_or(UNREACHABLE),
            )
            .env("AGENTDECK_DAEMON", daemon_binary())
            .stdin(Stdio::null());
        if self.github_api.is_some() {
            command.env("AGENTDECK_PR_MAX_AGE_DAYS", "0");
        }
        command
    }

    /// Runs `agentdeck <args>` with extra `env` and waits for it.
    pub fn run_with(&self, args: &[&str], env: &[(&str, &OsString)]) -> Run {
        let mut command = self.command();
        command.args(args);
        for (name, value) in env {
            command.env(name, value);
        }
        command.output().expect("running agentdeck").into()
    }

    /// Runs `agentdeck <args>` and waits for it.
    pub fn run(&self, args: &[&str]) -> Run {
        self.run_with(args, &[])
    }

    /// Runs `agentdeck <args>`, which must succeed, and returns its stdout.
    pub fn ok(&self, args: &[&str]) -> String {
        let run = self.run(args);
        assert!(
            run.success,
            "agentdeck {args:?} failed:\n{}{}",
            run.stdout, run.stderr
        );
        run.stdout
    }

    /// Runs `agentdeck <args> --json`, which must succeed, and parses its stdout.
    pub fn json(&self, args: &[&str]) -> Value {
        let args: Vec<&str> = args.iter().copied().chain(["--json"]).collect();
        let stdout = self.ok(&args);
        serde_json::from_str(&stdout)
            .unwrap_or_else(|err| panic!("agentdeck {args:?} printed no JSON ({err}):\n{stdout}"))
    }

    /// Runs `agentdeck <args> --json` with extra `env`, which must succeed, and parses its
    /// stdout.
    pub fn json_with(&self, args: &[&str], env: &[(&str, &OsString)]) -> Value {
        let args: Vec<&str> = args.iter().copied().chain(["--json"]).collect();
        let run = self.run_with(&args, env);
        assert!(
            run.success,
            "agentdeck {args:?} failed:\n{}{}",
            run.stdout, run.stderr
        );
        serde_json::from_str(&run.stdout).unwrap_or_else(|err| {
            panic!(
                "agentdeck {args:?} printed no JSON ({err}):\n{}",
                run.stdout
            )
        })
    }

    /// Runs `agentdeck <args>`, which must fail, and returns its stderr.
    pub fn fails(&self, args: &[&str]) -> String {
        let run = self.run(args);
        assert!(
            !run.success,
            "agentdeck {args:?} succeeded:\n{}",
            run.stdout
        );
        run.stderr
    }

    /// Creates a workspace on a fresh dir and returns it as JSON.
    pub fn workspace(&self, name: &str) -> Value {
        let dir = self.new_dir(name);
        self.json(&["ws", "new", name, "--dir", dir.to_str().unwrap()])
    }
}

/// The state letter in `/proc/<pid>/stat` (`R`, `S`, `Z` for a zombie, …); `None` once no
/// process has this pid, a zombie included once it is reaped.
pub fn process_state(pid: u32) -> Option<char> {
    stat_after_comm(pid)?.trim_start().chars().next()
}

/// The parent pid in `/proc/<pid>/stat`, while the process exists.
pub fn parent_pid(pid: u32) -> Option<u32> {
    stat_after_comm(pid)?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// `/proc/<pid>/stat` after the command name, which is in parentheses and may itself hold `)`
/// or spaces.
fn stat_after_comm(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, after_comm) = stat.rsplit_once(')')?;
    Some(after_comm.to_owned())
}

/// Whether a process with this pid exists.
pub fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .expect("running kill")
        .success()
}

impl Drop for TestEnv {
    /// SIGTERM, then waits for the daemon to go; SIGKILL only if it outlives the timeout.
    /// Then SIGKILL to whatever its panes left running.
    fn drop(&mut self) {
        if let Some(info) = self.daemon_info() {
            signal("-TERM", info.pid);
            let deadline = Instant::now() + TIMEOUT;
            while alive(info.pid) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if alive(info.pid) {
                signal("-KILL", info.pid);
            }
        }
        kill_left_running(self.root());
    }
}

fn signal(name: &str, pid: u32) {
    let _ = Command::new("kill")
        .args([name, &pid.to_string()])
        .stderr(Stdio::null())
        .status();
}

/// SIGKILLs every process started under `root`: those whose `$XDG_RUNTIME_DIR` is
/// `<root>/run`, which a pane's environment passes on (spec §6). The PTY's hang-up as the
/// daemon dies ends a pane's shell, but not a job of its own process group, which the
/// stopping daemon doesn't end either (spec §2 Stop); it would run on (issue #274). This
/// also ends what a daemon a test killed left behind. The temp root is unique, so nothing
/// else matches, even a process that reused a pid. Its twin is in
/// `agentdeckd/tests/integration/support`.
fn kill_left_running(root: &Path) {
    let mut marker = b"XDG_RUNTIME_DIR=".to_vec();
    marker.extend_from_slice(root.join("run").as_os_str().as_bytes());
    let pids = std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok());
    for pid in pids {
        let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
        if environ.split(|byte| *byte == 0).any(|var| var == marker) {
            signal("-KILL", pid);
        }
    }
}

/// The short id of `id`, a pane's or task's: its last 8 hex digits (spec §10).
pub fn short(id: &str) -> &str {
    &id[id.len() - 8..]
}

/// A string field of a JSON object.
pub fn text<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field]
        .as_str()
        .unwrap_or_else(|| panic!("no string `{field}` in {value}"))
}

/// The argv a stub agent in `pane` was started with, program name left out; `None` until it
/// has started.
pub fn agent_argv(env: &TestEnv, pane: &str) -> Option<Vec<String>> {
    let bytes = std::fs::read(env.root().join("agents").join(pane).join("argv")).ok()?;
    argv_record(&String::from_utf8(bytes).expect("UTF-8 argv"))
}

/// `argv` less the flags registering agentdeck's MCP server, which the daemon puts first:
/// Claude Code's `--mcp-config <json>` and Codex's `-c <key=value>` pairs.
pub fn without_mcp_flags(argv: &[String]) -> Vec<String> {
    let mut rest = argv;
    while let [flag, _, after @ ..] = rest
        && (flag == "--mcp-config" || flag == "-c")
    {
        rest = after;
    }
    rest.to_vec()
}

/// The arguments in a stub's argv file, or `None` until the stub has written its terminator:
/// the file exists from the moment the redirect opens it, empty or half written.
fn argv_record(text: &str) -> Option<Vec<String>> {
    let args = text.strip_suffix('\u{1e}')?;
    Some(args.split_terminator('\0').map(str::to_owned).collect())
}

/// Every prompt the stub agent in `pane` submitted, each ended by `\n`.
pub fn agent_typed(env: &TestEnv, pane: &str) -> String {
    std::fs::read_to_string(env.root().join("agents").join(pane).join("stdin")).unwrap_or_default()
}

/// The workspace's tabs, through the daemon's API.
pub fn tabs(env: &TestEnv, workspace: &Value) -> Vec<agentdeck_client::types::Tab> {
    let info = env.daemon_info().expect("a running daemon");
    let client = agentdeck_client::authenticated(&info.base_url(), &info.token).expect("a client");
    let workspace_id: agentdeck_core::WorkspaceId = text(workspace, "id").parse().unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(client.tab_list().id(workspace_id).send())
        .expect("list tabs")
        .into_inner()
}

/// Spawns a shell pane running `sh -c <script>` in a new tab of `workspace` (a JSON
/// workspace), titled `tab`, and makes it the tab's layout, as the GUI does, through the
/// daemon's API, since the CLI has no command for shell panes. Returns the pane's id.
pub fn shell_pane(env: &TestEnv, workspace: &Value, script: &str) -> String {
    shell_pane_in(env, workspace, "tab", script)
}

/// [`shell_pane`] in a tab titled `tab`.
pub fn shell_pane_in(env: &TestEnv, workspace: &Value, tab: &str, script: &str) -> String {
    use agentdeck_client::types::{NewPane, NewTab, TabPatch};
    use agentdeck_core::{Layout, PaneKind};

    let info = env.daemon_info().expect("a running daemon");
    let client = agentdeck_client::authenticated(&info.base_url(), &info.token).expect("a client");
    let workspace_id: agentdeck_core::WorkspaceId = text(workspace, "id").parse().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let tab = client
            .tab_create()
            .id(workspace_id)
            .body(NewTab {
                title: tab.into(),
                layout: None,
            })
            .send()
            .await
            .expect("create a tab");
        let pane = client
            .pane_create()
            .body(NewPane {
                agent: None,
                argv: Some(vec!["sh".into(), "-c".into(), script.into()]),
                cwd: text(workspace, "dir").into(),
                kind: PaneKind::Shell,
                tab_id: tab.id,
                task_id: None,
            })
            .send()
            .await
            .expect("spawn a pane");
        client
            .tab_update()
            .id(tab.id)
            .body(TabPatch {
                layout: Some(Layout::single(pane.id)),
                ..TabPatch::default()
            })
            .send()
            .await
            .expect("lay the pane out");
        pane.id.to_string()
    })
}

/// Spawns a shell pane in `workspace` whose foreground process is the stub `agent` (`claude`
/// or `codex`), as when the user runs it at a shell's prompt, and waits until the daemon has
/// the pane hosting it (spec §7). Returns the pane's id.
pub fn hosting_pane(env: &TestEnv, workspace: &Value, agent: agentdeck_core::Agent) -> String {
    let pane = shell_pane(env, workspace, &format!("exec {agent}"));
    let info = env.daemon_info().expect("a running daemon");
    let client = agentdeck_client::authenticated(&info.base_url(), &info.token).expect("a client");
    let id: agentdeck_core::PaneId = pane.parse().expect("a pane id");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    eventually(&format!("the shell pane to host {agent}"), || {
        let read = runtime.block_on(client.pane_get().id(id).send());
        (read.expect("read the pane").hosted_agent == Some(agent)).then_some(())
    });
    pane
}

/// Polls `check` until it returns `Some`, or panics with `what` after 30 s.
pub fn eventually<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

mod tests {
    use super::*;

    mod argv_record {
        use super::*;

        #[test]
        fn should_be_none_while_the_stub_is_still_writing_it() {
            assert_eq!(argv_record(""), None);
            assert_eq!(argv_record("--session-id\0"), None);
        }

        #[test]
        fn should_give_the_arguments_once_the_record_is_terminated() {
            assert_eq!(
                argv_record("--session-id\0abc\0\u{1e}"),
                Some(vec!["--session-id".to_owned(), "abc".to_owned()])
            );
        }

        #[test]
        fn should_give_no_arguments_for_a_terminated_empty_record() {
            assert_eq!(argv_record("\u{1e}"), Some(Vec::new()));
        }
    }
}
