//! One daemon per user: the lock, and the `daemon.json` clients read (spec §2).

use std::fs::{DirBuilder, File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agentdeck_core::DaemonInfo;
use anyhow::{Context, bail};

use crate::paths::Paths;

/// How long a daemon waits for the lock while the daemon holding it shuts down.
const LOCK_WAIT: Duration = Duration::from_secs(10);

const LOCK_POLL: Duration = Duration::from_millis(25);

/// How long the daemon named in `daemon.json` gets to answer `/v1/health`.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(1);

/// Holds the single-instance lock until dropped, and removes `daemon.json` then.
pub struct Instance {
    _lock: File,
    daemon_json: PathBuf,
}

impl Instance {
    /// Takes the lock on `$XDG_RUNTIME_DIR/agentdeck/lock`, creating the dir (0700). Fails at
    /// once when another daemon holds it and the daemon in `daemon.json` answers: that one
    /// serves. Otherwise the holder is shutting down, still starting, or was killed and is
    /// still exiting (its `daemon.json` left behind), so this waits up to [`LOCK_WAIT`] for it.
    pub fn acquire(paths: &Paths) -> anyhow::Result<Instance> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&paths.runtime)
            .with_context(|| format!("creating {}", paths.runtime.display()))?;
        let lock_path = paths.lock_file();
        let daemon_json = paths.daemon_json();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&lock_path)
            .with_context(|| format!("opening {}", lock_path.display()))?;
        let deadline = Instant::now() + LOCK_WAIT;
        let mut waiting = false;
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(err)) => {
                    return Err(err).with_context(|| format!("locking {}", lock_path.display()));
                }
            }
            if serves(&daemon_json) {
                bail!(
                    "another agentdeckd is running (it holds {})",
                    lock_path.display()
                );
            }
            if Instant::now() >= deadline {
                bail!(
                    "the agentdeckd that holds {} hasn't exited after {LOCK_WAIT:?}",
                    lock_path.display()
                );
            }
            if !waiting {
                tracing::info!(
                    "waiting for the agentdeckd that holds {} to exit",
                    lock_path.display()
                );
                waiting = true;
            }
            std::thread::sleep(LOCK_POLL);
        }
        Ok(Instance {
            _lock: lock,
            daemon_json,
        })
    }

    /// Removes `daemon.json` while the lock stays held, as the daemon begins to shut down:
    /// clients stop reaching it, and a daemon one of them starts waits for the lock.
    pub fn withdraw(&self) {
        let _ = std::fs::remove_file(&self.daemon_json);
    }

    /// Writes `daemon.json` with mode 0600, replacing it in one rename so a client never
    /// reads half of it.
    pub fn publish(&self, info: &DaemonInfo) -> anyhow::Result<()> {
        let tmp = self.daemon_json.with_extension("json.tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        file.write_all(serde_json::to_string_pretty(info)?.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, &self.daemon_json)
            .with_context(|| format!("writing {}", self.daemon_json.display()))?;
        Ok(())
    }
}

/// Whether `daemon_json` names a daemon that answers `/v1/health` with its token. A missing
/// or unreadable file, a refused connection and no answer in [`HEALTH_TIMEOUT`] all mean no:
/// a killed daemon leaves its file behind and holds the lock until its exit closes its files.
/// The pid needs no check: a process that has exited holds no lock.
fn serves(daemon_json: &Path) -> bool {
    std::fs::read_to_string(daemon_json)
        .ok()
        .and_then(|text| serde_json::from_str::<DaemonInfo>(&text).ok())
        .is_some_and(|info| answers_health(&info).unwrap_or(false))
}

/// `GET /v1/health` over plain HTTP/1.1, before the daemon has a runtime for a real client:
/// whether the status line is `200`.
fn answers_health(info: &DaemonInfo) -> std::io::Result<bool> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, info.port));
    let mut stream = TcpStream::connect_timeout(&address, HEALTH_TIMEOUT)?;
    stream.set_read_timeout(Some(HEALTH_TIMEOUT))?;
    stream.set_write_timeout(Some(HEALTH_TIMEOUT))?;
    write!(
        stream,
        "GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\n\
         Connection: close\r\n\r\n",
        info.token
    )?;
    let mut status_line = [0; 12];
    stream.read_exact(&mut status_line)?;
    Ok(status_line.starts_with(b"HTTP/1.") && status_line.ends_with(b" 200"))
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.withdraw();
    }
}
