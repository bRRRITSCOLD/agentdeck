//! Where the daemon keeps its files, from the XDG base directories (spec §2).

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, bail};

/// The daemon's directories, each already ending in `agentdeck`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `$XDG_RUNTIME_DIR/agentdeck`: the lock and `daemon.json`.
    pub runtime: PathBuf,
    /// `$XDG_DATA_HOME/agentdeck`: the database and scratch workspaces.
    pub data: PathBuf,
    /// `$XDG_STATE_HOME/agentdeck`: the log.
    pub state: PathBuf,
}

impl Paths {
    /// Reads the process environment.
    pub fn from_env() -> anyhow::Result<Paths> {
        Paths::from_vars(|name| std::env::var_os(name))
    }

    /// Reads the variables through `var`. `XDG_RUNTIME_DIR` is required: there is no safe
    /// per-user default for a 0600 token file. The others fall back to the XDG defaults
    /// under `$HOME`. A relative value is ignored, as the XDG spec requires.
    pub fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> anyhow::Result<Paths> {
        let absolute = |name: &str| {
            var(name)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        };
        let Some(runtime) = absolute("XDG_RUNTIME_DIR") else {
            bail!("XDG_RUNTIME_DIR is not set to an absolute path");
        };
        let home = || absolute("HOME").context("HOME is not set to an absolute path");
        let data = match absolute("XDG_DATA_HOME") {
            Some(dir) => dir,
            None => home()?.join(".local/share"),
        };
        let state = match absolute("XDG_STATE_HOME") {
            Some(dir) => dir,
            None => home()?.join(".local/state"),
        };
        Ok(Paths {
            runtime: runtime.join("agentdeck"),
            data: data.join("agentdeck"),
            state: state.join("agentdeck"),
        })
    }

    /// The single-instance lock.
    pub fn lock_file(&self) -> PathBuf {
        self.runtime.join("lock")
    }

    /// Where clients find the port and token.
    pub fn daemon_json(&self) -> PathBuf {
        self.runtime.join("daemon.json")
    }

    /// The SQLite database.
    pub fn database(&self) -> PathBuf {
        self.data.join("agentdeck.db")
    }

    /// The parent of every scratch workspace dir.
    pub fn scratch_workspaces(&self) -> PathBuf {
        self.data.join("workspaces")
    }

    /// The daemon's log file.
    pub fn log_file(&self) -> PathBuf {
        self.state.join("agentdeckd.log")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn paths(vars: &[(&str, &str)]) -> anyhow::Result<Paths> {
        let vars: HashMap<String, OsString> = vars
            .iter()
            .map(|(name, value)| (name.to_string(), OsString::from(value)))
            .collect();
        Paths::from_vars(|name| vars.get(name).cloned())
    }

    mod from_vars {
        use super::*;

        #[test]
        fn should_put_each_dir_under_its_xdg_variable() {
            let paths = paths(&[
                ("XDG_RUNTIME_DIR", "/run/user/1000"),
                ("XDG_DATA_HOME", "/data"),
                ("XDG_STATE_HOME", "/state"),
            ])
            .unwrap();

            assert_eq!(
                paths,
                Paths {
                    runtime: "/run/user/1000/agentdeck".into(),
                    data: "/data/agentdeck".into(),
                    state: "/state/agentdeck".into(),
                }
            );
        }

        #[test]
        fn should_default_data_and_state_under_home() {
            let paths =
                paths(&[("XDG_RUNTIME_DIR", "/run/user/1000"), ("HOME", "/home/me")]).unwrap();

            assert_eq!(paths.data, PathBuf::from("/home/me/.local/share/agentdeck"));
            assert_eq!(
                paths.state,
                PathBuf::from("/home/me/.local/state/agentdeck")
            );
        }

        #[test]
        fn should_ignore_a_relative_xdg_value() {
            let paths = paths(&[
                ("XDG_RUNTIME_DIR", "/run/user/1000"),
                ("XDG_DATA_HOME", "relative"),
                ("HOME", "/home/me"),
            ])
            .unwrap();

            assert_eq!(paths.data, PathBuf::from("/home/me/.local/share/agentdeck"));
        }

        #[test]
        fn should_fail_without_a_runtime_dir() {
            assert!(paths(&[("HOME", "/home/me")]).is_err());
        }
    }

    mod files {
        use super::*;

        #[test]
        fn should_name_the_files_the_spec_names() {
            let paths = paths(&[
                ("XDG_RUNTIME_DIR", "/run"),
                ("XDG_DATA_HOME", "/data"),
                ("XDG_STATE_HOME", "/state"),
            ])
            .unwrap();

            assert_eq!(paths.lock_file(), PathBuf::from("/run/agentdeck/lock"));
            assert_eq!(
                paths.daemon_json(),
                PathBuf::from("/run/agentdeck/daemon.json")
            );
            assert_eq!(
                paths.database(),
                PathBuf::from("/data/agentdeck/agentdeck.db")
            );
            assert_eq!(
                paths.scratch_workspaces(),
                PathBuf::from("/data/agentdeck/workspaces")
            );
            assert_eq!(
                paths.log_file(),
                PathBuf::from("/state/agentdeck/agentdeckd.log")
            );
        }
    }
}
