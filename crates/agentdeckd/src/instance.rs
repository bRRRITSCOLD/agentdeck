//! One daemon per user: the lock, and the `daemon.json` clients read (spec §2).

use std::fs::{DirBuilder, File, OpenOptions, TryLockError};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;

use agentdeck_core::DaemonInfo;
use anyhow::{Context, bail};

use crate::paths::Paths;

/// Holds the single-instance lock until dropped, and removes `daemon.json` then.
pub struct Instance {
    _lock: File,
    daemon_json: PathBuf,
}

impl Instance {
    /// Takes the lock on `$XDG_RUNTIME_DIR/agentdeck/lock`, creating the dir (0700). Fails
    /// when another daemon holds it.
    pub fn acquire(paths: &Paths) -> anyhow::Result<Instance> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&paths.runtime)
            .with_context(|| format!("creating {}", paths.runtime.display()))?;
        let lock_path = paths.lock_file();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&lock_path)
            .with_context(|| format!("opening {}", lock_path.display()))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                bail!(
                    "another agentdeckd is running (it holds {})",
                    lock_path.display()
                )
            }
            Err(TryLockError::Error(err)) => {
                return Err(err).with_context(|| format!("locking {}", lock_path.display()));
            }
        }
        Ok(Instance {
            _lock: lock,
            daemon_json: paths.daemon_json(),
        })
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

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.daemon_json);
    }
}
