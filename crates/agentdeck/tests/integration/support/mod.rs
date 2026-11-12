//! The integration harness: the real `agentdeck` binary in temp `$XDG_*` dirs, against the
//! real `agentdeckd` that it starts itself (spec §2, §12). Nothing reaches GitHub: `gh` is a
//! logged-out stub and the GraphQL URL points at a closed port. `claude` and `codex` are stubs
//! first on `PATH`, so no test runs a real agent.

pub mod mcp;

use std::ffi::OsString;
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

/// A stub `claude`: records its argv under `<root>/agents/<pane>/argv` (NUL-separated), sets
/// Claude Code's idle title, then appends whatever is typed to `stdin` there. Stays in the
/// foreground, so the claude rules apply.
const STUB_CLAUDE: &str = r#"#!/bin/sh
log="$(dirname "$0")/../agents/$AGENTDECK_PANE"
mkdir -p "$log"
for arg in "$@"; do printf '%s\0' "$arg"; done > "$log/argv"
printf '\033]0;\342\234\263 Claude Code\007> '
cat >> "$log/stdin"
"#;

/// A stub `codex`: records its argv, writes a session file as Codex does, then appends
/// whatever is typed to `stdin`.
const STUB_CODEX: &str = r#"#!/bin/sh
log="$(dirname "$0")/../agents/$AGENTDECK_PANE"
mkdir -p "$log"
for arg in "$@"; do printf '%s\0' "$arg"; done > "$log/argv"
id=$(cat /proc/sys/kernel/random/uuid)
dir="${CODEX_HOME:-$HOME/.codex}/sessions/2026/11/12"
mkdir -p "$dir"
: > "$dir/rollout-2026-11-12T09-00-00-$id.jsonl"
printf '\033]0;codex\007'
cat >> "$log/stdin"
"#;

/// A `gh` that is never logged in.
const STUB_GH: &str = "#!/bin/sh\necho \"no oauth token found for github.com\" >&2\nexit 1\n";

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
            std::fs::write(&path, script).expect("writing a stub");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("making a stub executable");
        }
        std::fs::create_dir_all(root.path().join("home")).expect("creating $HOME");
        TestEnv { root }
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
            .env("AGENTDECK_GITHUB_API", UNREACHABLE)
            .env("AGENTDECK_DAEMON", daemon_binary())
            .stdin(Stdio::null());
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
    fn drop(&mut self) {
        let Some(info) = self.daemon_info() else {
            return;
        };
        let pid = info.pid.to_string();
        let signal = |name: &str| {
            let _ = Command::new("kill")
                .args([name, &pid])
                .stderr(Stdio::null())
                .status();
        };
        signal("-TERM");
        let deadline = Instant::now() + TIMEOUT;
        while alive(info.pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if alive(info.pid) {
            signal("-KILL");
        }
    }
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
    let text = String::from_utf8(bytes).expect("UTF-8 argv");
    Some(text.split_terminator('\0').map(str::to_owned).collect())
}

/// What has been typed into the stub agent in `pane`.
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
/// workspace), through the daemon's API, since the CLI has no command for shell panes.
/// Returns the pane's id.
pub fn shell_pane(env: &TestEnv, workspace: &Value, script: &str) -> String {
    use agentdeck_client::types::{NewPane, NewTab};
    use agentdeck_core::PaneKind;

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
                title: "tab".into(),
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
        pane.id.to_string()
    })
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
