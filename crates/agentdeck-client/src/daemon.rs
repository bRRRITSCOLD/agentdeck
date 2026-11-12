//! Reaching the daemon (spec §2): read `daemon.json`, and start `agentdeckd` detached when
//! nothing answers there. The CLI and the GUI both connect through [`connect`].

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use agentdeck_core::DaemonInfo;
use nix::errno::Errno;
use nix::sys::signal::kill;
use nix::unistd::Pid;

use crate::Client;

/// Replaces the `agentdeckd` a client starts, for tests and unusual installs.
pub const DAEMON_VAR: &str = "AGENTDECK_DAEMON";

/// How long a started daemon gets to serve.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to keep looking after the started daemon exited, in case another client's daemon
/// won the lock and is still starting.
const LOST_RACE_GRACE: Duration = Duration::from_secs(2);

const POLL: Duration = Duration::from_millis(25);

/// The running daemon: a client for its API, and the `daemon.json` it was reached through,
/// whose port and token also open its WebSocket.
#[derive(Debug, Clone)]
pub struct Daemon {
    /// A client that sends the daemon's bearer token.
    pub client: Client,
    /// What `daemon.json` held.
    pub info: DaemonInfo,
}

/// Why [`connect`] couldn't reach a daemon.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    /// The daemon refuses to start without an absolute `XDG_RUNTIME_DIR`, so clients do too.
    #[error("XDG_RUNTIME_DIR is not set to an absolute path")]
    NoRuntimeDir,
    /// `agentdeckd` couldn't be run.
    #[error("starting {}", program.display())]
    Spawn {
        /// The program that was run.
        program: PathBuf,
        /// Why it couldn't be.
        #[source]
        source: std::io::Error,
    },
    /// Waiting on the started `agentdeckd` failed.
    #[error("polling {}", program.display())]
    Poll {
        /// The program that was run.
        program: PathBuf,
        /// Why waiting failed.
        #[source]
        source: std::io::Error,
    },
    /// The started `agentdeckd` exited, and no other daemon answered either.
    #[error(
        "{} exited with {status} before it served; its log is \
         $XDG_STATE_HOME/agentdeck/agentdeckd.log",
        program.display()
    )]
    Exited {
        /// The program that was run.
        program: PathBuf,
        /// How it exited.
        status: ExitStatus,
    },
    /// The started `agentdeckd` is still running but never answered.
    #[error("{} didn't write {} in {START_TIMEOUT:?}", program.display(), daemon_json.display())]
    Timeout {
        /// The program that was run.
        program: PathBuf,
        /// Where it should have written `daemon.json`.
        daemon_json: PathBuf,
    },
}

/// The running daemon, started first if nothing answers at `daemon.json`. Polls with
/// `tokio::time`, so it runs inside a tokio runtime.
pub async fn connect() -> Result<Daemon, DaemonError> {
    let daemon_json = daemon_json(std::env::var_os("XDG_RUNTIME_DIR"))?;
    if let Some(daemon) = reachable(&daemon_json).await {
        return Ok(daemon);
    }
    let program = program(
        std::env::var_os(DAEMON_VAR),
        std::env::current_exe().ok(),
        |path| path.is_file(),
    );
    start(&program, &daemon_json).await
}

/// `$XDG_RUNTIME_DIR/agentdeck/daemon.json`.
fn daemon_json(runtime_dir: Option<OsString>) -> Result<PathBuf, DaemonError> {
    match runtime_dir.map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => Ok(dir.join("agentdeck").join("daemon.json")),
        _ => Err(DaemonError::NoRuntimeDir),
    }
}

/// Which `agentdeckd` to start: `$AGENTDECK_DAEMON`, else the one beside this executable,
/// else whichever `agentdeckd` is on `PATH`.
fn program(
    override_var: Option<OsString>,
    current_exe: Option<PathBuf>,
    is_file: impl Fn(&Path) -> bool,
) -> PathBuf {
    if let Some(program) = override_var.filter(|value| !value.is_empty()) {
        return program.into();
    }
    current_exe
        .and_then(|exe| Some(exe.parent()?.join("agentdeckd")))
        .filter(|sibling| is_file(sibling))
        .unwrap_or_else(|| "agentdeckd".into())
}

/// The daemon, when `daemon.json` names a live process that answers `/v1/health`. A missing
/// or unreadable file, a dead pid and a refused connection all mean no daemon.
async fn reachable(daemon_json: &Path) -> Option<Daemon> {
    let text = std::fs::read_to_string(daemon_json).ok()?;
    let info: DaemonInfo = serde_json::from_str(&text).ok()?;
    if !alive(info.pid) {
        return None;
    }
    let client = crate::authenticated(&info.base_url(), &info.token).ok()?;
    client.health().send().await.ok()?;
    Some(Daemon { client, info })
}

/// Whether a process with this pid exists; one owned by another user still counts.
fn alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    matches!(kill(Pid::from_raw(pid), None), Ok(()) | Err(Errno::EPERM))
}

/// Starts `program` in its own process group, with no terminal, and waits until a daemon
/// answers at `daemon_json`.
async fn start(program: &Path, daemon_json: &Path) -> Result<Daemon, DaemonError> {
    let mut child = Command::new(program)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|source| DaemonError::Spawn {
            program: program.to_owned(),
            source,
        })?;
    let mut deadline = Instant::now() + START_TIMEOUT;
    let mut exited = None;
    loop {
        if let Some(daemon) = reachable(daemon_json).await {
            return Ok(daemon);
        }
        if exited.is_none()
            && let Some(status) = child.try_wait().map_err(|source| DaemonError::Poll {
                program: program.to_owned(),
                source,
            })?
        {
            exited = Some(status);
            deadline = deadline.min(Instant::now() + LOST_RACE_GRACE);
        }
        if Instant::now() >= deadline {
            let program = program.to_owned();
            return Err(match exited {
                Some(status) => DaemonError::Exited { program, status },
                None => DaemonError::Timeout {
                    program,
                    daemon_json: daemon_json.to_owned(),
                },
            });
        }
        tokio::time::sleep(POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod daemon_json {
        use super::*;

        #[test]
        fn should_be_under_the_runtime_dir() {
            assert_eq!(
                daemon_json(Some("/run/user/1000".into())).unwrap(),
                PathBuf::from("/run/user/1000/agentdeck/daemon.json")
            );
        }

        #[test]
        fn should_fail_without_an_absolute_runtime_dir() {
            assert!(matches!(daemon_json(None), Err(DaemonError::NoRuntimeDir)));
            assert!(matches!(
                daemon_json(Some("relative".into())),
                Err(DaemonError::NoRuntimeDir)
            ));
        }
    }

    mod program {
        use super::*;

        const EXE: &str = "/opt/agentdeck/bin/agentdeck";

        #[test]
        fn should_prefer_the_override() {
            let program = program(Some("/custom/agentdeckd".into()), Some(EXE.into()), |_| {
                true
            });

            assert_eq!(program, PathBuf::from("/custom/agentdeckd"));
        }

        #[test]
        fn should_use_the_daemon_beside_the_cli() {
            let program = program(None, Some(EXE.into()), |path| {
                path == Path::new("/opt/agentdeck/bin/agentdeckd")
            });

            assert_eq!(program, PathBuf::from("/opt/agentdeck/bin/agentdeckd"));
        }

        #[test]
        fn should_fall_back_to_path() {
            let program = program(Some(OsString::new()), Some(EXE.into()), |_| false);

            assert_eq!(program, PathBuf::from("agentdeckd"));
        }
    }

    mod alive {
        use super::*;

        #[test]
        fn should_see_this_process() {
            assert!(alive(std::process::id()));
        }

        #[test]
        fn should_not_see_a_pid_out_of_range() {
            assert!(!alive(u32::MAX));
        }
    }
}
