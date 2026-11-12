//! The integration harness: a real `agentdeckd` in temp `$XDG_*` dirs, driven through a real
//! `agentdeck-client` (spec §12).

pub mod agents;
pub mod git;
pub mod github;
pub mod terminal;
pub mod ws;

use std::fmt::Debug;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use agentdeck_client::{Client, Error, ResponseValue};
use serde::Deserialize;
use tempfile::TempDir;

/// How long a daemon gets to write `daemon.json` or to exit.
const TIMEOUT: Duration = Duration::from_secs(10);

/// `daemon.json`, as a client reads it.
#[derive(Debug, Deserialize)]
pub struct DaemonInfo {
    pub port: u16,
    pub token: String,
    pub pid: u32,
    pub version: String,
}

/// A running daemon on its own temp dirs. Dropping it kills the daemon, then removes the
/// dirs, even when the test failed.
pub struct TestDaemon {
    child: Child,
    pub info: DaemonInfo,
    pub client: Client,
    // Declared last, so the dirs outlive the daemon while it's being killed.
    dirs: TempDir,
}

impl TestDaemon {
    /// Starts a daemon on fresh temp dirs and waits until it serves. Its `gh` is logged out
    /// and its GitHub is unreachable, so it never calls the real one.
    pub async fn start() -> TestDaemon {
        let dirs = tempfile::tempdir().expect("a temp dir");
        TestDaemon::start_in(dirs).await
    }

    /// Starts a daemon on `dirs`, which may hold an earlier daemon's data.
    pub async fn start_in(dirs: TempDir) -> TestDaemon {
        TestDaemon::start_with_github(dirs, github::UNREACHABLE).await
    }

    /// Starts a daemon on `dirs` whose PR sync calls the GraphQL API at `github_api`. Its
    /// `gh` prints the token [`github::set_gh_token`] wrote under `dirs`, if any.
    pub async fn start_with_github(dirs: TempDir, github_api: &str) -> TestDaemon {
        let mut child = spawn(dirs.path(), github_api);
        let daemon_json = daemon_json(dirs.path());
        let deadline = Instant::now() + TIMEOUT;
        let info = loop {
            if let Ok(text) = std::fs::read_to_string(&daemon_json) {
                let info: DaemonInfo = serde_json::from_str(&text).expect("daemon.json parses");
                if info.pid == child.id() {
                    break info;
                }
            }
            if let Some(status) = child.try_wait().expect("polling agentdeckd") {
                panic!(
                    "agentdeckd exited with {status} before serving:\n{}",
                    stderr(dirs.path())
                );
            }
            assert!(
                Instant::now() < deadline,
                "agentdeckd didn't write daemon.json in {TIMEOUT:?}:\n{}",
                stderr(dirs.path())
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let client = agentdeck_client::authenticated(
            &format!("http://127.0.0.1:{}", info.port),
            &info.token,
        )
        .expect("a client");
        TestDaemon {
            child,
            info,
            client,
            dirs,
        }
    }

    /// `http://127.0.0.1:<port>`.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.info.port)
    }

    /// The temp dir holding every `$XDG_*` dir.
    pub fn root(&self) -> &Path {
        self.dirs.path()
    }

    /// `$XDG_RUNTIME_DIR/agentdeck`.
    pub fn runtime_dir(&self) -> PathBuf {
        self.root().join("run/agentdeck")
    }

    /// `$XDG_DATA_HOME/agentdeck`.
    pub fn data_dir(&self) -> PathBuf {
        self.root().join("data/agentdeck")
    }

    /// `$XDG_STATE_HOME/agentdeck`.
    pub fn state_dir(&self) -> PathBuf {
        self.root().join("state/agentdeck")
    }

    /// A fresh directory under the temp root, for attaching as a workspace dir.
    pub fn new_dir(&self, name: &str) -> PathBuf {
        let dir = self.root().join("dirs").join(name);
        std::fs::create_dir_all(&dir).expect("creating a dir");
        dir
    }

    /// Starts another daemon on the same dirs and waits for it to exit.
    pub fn run_another(&self) -> (ExitStatus, String) {
        let stderr_path = self.root().join("another.stderr");
        let status = command(self.root(), &stderr_path, github::UNREACHABLE)
            .status()
            .expect("running agentdeckd");
        (
            status,
            std::fs::read_to_string(stderr_path).unwrap_or_default(),
        )
    }

    /// Sends SIGTERM and waits for the daemon to exit; hands back the dirs for a restart.
    pub fn terminate(mut self) -> (ExitStatus, TempDir) {
        let killed = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .expect("running kill");
        assert!(killed.success(), "kill -TERM failed");
        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("polling agentdeckd") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "agentdeckd didn't exit on SIGTERM"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        // Swap the dirs out so Drop doesn't remove them.
        let dirs = std::mem::replace(&mut self.dirs, tempfile::tempdir().expect("a temp dir"));
        (status, dirs)
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn daemon_json(root: &Path) -> PathBuf {
    root.join("run/agentdeck/daemon.json")
}

fn stderr(root: &Path) -> String {
    std::fs::read_to_string(root.join("agentdeckd.stderr")).unwrap_or_default()
}

fn spawn(root: &Path, github_api: &str) -> Child {
    command(root, &root.join("agentdeckd.stderr"), github_api)
        .spawn()
        .expect("starting agentdeckd")
}

/// `agentdeckd` with only `PATH` from the test's environment, behind a stub `gh`, `claude`
/// and `codex`; `$HOME`
/// and every `$XDG_*` dir under `root`; GitHub at `github_api`; and stderr to a file (a pipe
/// nobody reads would fill and block it).
fn command(root: &Path, stderr: &Path, github_api: &str) -> Command {
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("creating $HOME");
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentdeckd"));
    command
        .env_clear()
        .env("PATH", stub_path(root))
        .env("AGENTDECK_GITHUB_API", github_api)
        .env("HOME", &home)
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .envs(extra_env(root))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(File::create(stderr).expect("creating the stderr file"));
    command
}

/// A `PATH` that finds the stub `gh`, `claude` and `codex` first.
fn stub_path(root: &Path) -> std::ffi::OsString {
    let path = github::path_with_stub_gh(root);
    agents::install(&root.join("bin"));
    path
}

/// Where [`set_daemon_env`] keeps the extra variables, one `NAME=value` per line.
fn extra_env_file(root: &Path) -> PathBuf {
    root.join("daemon.env")
}

/// Starts every daemon on `root` with these variables on top of the harness's own, as if it
/// had been launched from a shell that has them (from inside an agent, say).
pub fn set_daemon_env(root: &Path, vars: &[(&str, &str)]) {
    let lines: String = vars
        .iter()
        .map(|(name, value)| format!("{name}={value}\n"))
        .collect();
    std::fs::write(extra_env_file(root), lines).expect("writing daemon.env");
}

fn extra_env(root: &Path) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(extra_env_file(root)).unwrap_or_default();
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

/// The status and body of a documented error response; panics on success or on any other
/// failure.
pub fn error_response<T: Debug, E>(result: Result<ResponseValue<T>, Error<E>>) -> (u16, E) {
    match result {
        Err(Error::ErrorResponse(response)) => {
            let status = response.status().as_u16();
            (status, response.into_inner())
        }
        Err(err) => panic!("expected an error response, got {}", describe(&err)),
        Ok(response) => panic!(
            "expected an error response, got {:?}",
            response.into_inner()
        ),
    }
}

fn describe<E>(err: &Error<E>) -> String {
    match err {
        Error::ErrorResponse(response) => format!("error response {}", response.status()),
        other => format!("{:?}", other.status()),
    }
}
