//! A pane's environment (spec §6): built from an allowlist of the daemon's own variables,
//! never the daemon's whole environment. A daemon started from inside an agent would
//! otherwise pass on that agent's variables (`CLAUDECODE`, `CLAUDE_CODE_*`), and every pane
//! would run as a nested session.
//!
//! Every pane's `PATH` ends with the folder holding the running `agentdeckd`, where
//! `agentdeck` is installed beside it, so an agent in a pane can run `agentdeck` without that
//! folder shadowing anything earlier on the user's `PATH` (spec §6).

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Variables passed on by exact name.
const NAMES: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "SHELL",
    "LANG",
    "TERM",
    "COLORTERM",
    "SSH_AUTH_SOCK",
    "EDITOR",
    // Where Codex keeps its sessions: the daemon reads them from the same place (spec §6).
    "CODEX_HOME",
    // Where Claude Code keeps its config and sessions.
    "CLAUDE_CONFIG_DIR",
];

/// Variables passed on by prefix.
const PREFIXES: &[&str] = &["LC_", "XDG_", "GH_"];

/// What every pane gets whatever the daemon has.
const FIXED: &[(&str, &str)] = &[("TERM", "xterm-256color"), ("COLORTERM", "truecolor")];

/// The shell a shell pane runs when it names no command: the daemon's `$SHELL` when it is
/// an absolute path, else `/bin/sh`.
pub fn default_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|shell| shell.starts_with('/'))
        .unwrap_or_else(|| "/bin/sh".to_owned())
}

/// Whether a variable of the daemon's may reach a pane.
fn allowed(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    NAMES.contains(&name) || PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

/// The folder holding the running `agentdeckd`, which every pane's `PATH` ends with; `None`,
/// with a warning, when the OS can't say.
pub fn daemon_dir() -> Option<PathBuf> {
    match std::env::current_exe() {
        Ok(exe) => exe.parent().map(Path::to_path_buf),
        Err(err) => {
            tracing::warn!("panes' PATH can't end with agentdeckd's folder: {err}");
            None
        }
    }
}

/// `path` with `dir` after its last entry, or `dir` alone when `path` is unset or empty. A
/// `dir` that can't be a `PATH` entry (it holds a `:`) leaves `path` as it was.
fn path_ending_with(path: Option<OsString>, dir: &Path) -> Option<OsString> {
    if dir.as_os_str().as_encoded_bytes().contains(&b':') {
        tracing::warn!(
            "panes' PATH can't end with {}: it holds a ':'",
            dir.display()
        );
        return path;
    }
    Some(match path {
        Some(mut path) if !path.is_empty() => {
            path.push(":");
            path.push(dir);
            path
        }
        _ => dir.as_os_str().to_owned(),
    })
}

/// A pane's whole environment: the allowlisted part of `daemon` with `path_end` (the folder
/// holding `agentdeckd`, from [`daemon_dir`]) after the last entry of its `PATH`, then `TERM`
/// and `COLORTERM`, then `extra` (the caller's own variables, such as `AGENTDECK_PANE`), each
/// overriding what came before.
pub fn pane_env(
    daemon: impl IntoIterator<Item = (OsString, OsString)>,
    path_end: Option<&Path>,
    extra: &[(String, String)],
) -> BTreeMap<OsString, OsString> {
    let mut env: BTreeMap<OsString, OsString> = daemon
        .into_iter()
        .filter(|(name, _)| allowed(name))
        .collect();
    if let Some(dir) = path_end
        && let Some(path) = path_ending_with(env.remove(OsStr::new("PATH")), dir)
    {
        env.insert("PATH".into(), path);
    }
    let fixed = FIXED.iter().map(|(name, value)| (*name, *value));
    let extra = extra
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()));
    for (name, value) in fixed.chain(extra) {
        env.insert(name.into(), value.into());
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into()))
            .collect()
    }

    fn names(env: &BTreeMap<OsString, OsString>) -> Vec<&str> {
        env.keys().map(|name| name.to_str().unwrap()).collect()
    }

    mod pane_env {
        use super::*;

        #[test]
        fn should_pass_on_every_allowlisted_variable() {
            let daemon = vars(&[
                ("PATH", "/bin"),
                ("HOME", "/home/me"),
                ("USER", "me"),
                ("SHELL", "/bin/zsh"),
                ("LANG", "en_GB.UTF-8"),
                ("LC_ALL", "C"),
                ("XDG_CONFIG_HOME", "/cfg"),
                ("SSH_AUTH_SOCK", "/run/ssh"),
                ("GH_TOKEN", "t"),
                ("EDITOR", "vi"),
                ("CODEX_HOME", "/codex"),
                ("CLAUDE_CONFIG_DIR", "/claude"),
            ]);
            let env = pane_env(daemon.clone(), None, &[]);
            for (name, value) in daemon {
                assert_eq!(env.get(&name), Some(&value), "{name:?}");
            }
        }

        #[test]
        fn should_drop_everything_else() {
            let env = pane_env(
                vars(&[
                    ("PATH", "/bin"),
                    ("CLAUDECODE", "1"),
                    ("CLAUDE_CODE_ENTRYPOINT", "cli"),
                    ("AWS_SECRET_ACCESS_KEY", "s"),
                    ("LD_PRELOAD", "/evil.so"),
                    ("PATHX", "/x"),
                    ("XLC_FOO", "1"),
                ]),
                None,
                &[],
            );
            assert_eq!(names(&env), ["COLORTERM", "PATH", "TERM"]);
        }

        #[test]
        fn should_set_term_and_colorterm_over_the_daemons() {
            let env = pane_env(vars(&[("TERM", "dumb"), ("COLORTERM", "no")]), None, &[]);
            assert_eq!(env[OsStr::new("TERM")], "xterm-256color");
            assert_eq!(env[OsStr::new("COLORTERM")], "truecolor");
        }

        #[test]
        fn should_add_the_callers_variables_last() {
            let env = pane_env(
                vars(&[("HOME", "/home/me")]),
                None,
                &[
                    ("AGENTDECK_PANE".into(), "p".into()),
                    ("HOME".into(), "/elsewhere".into()),
                ],
            );
            assert_eq!(env[OsStr::new("AGENTDECK_PANE")], "p");
            assert_eq!(env[OsStr::new("HOME")], "/elsewhere");
        }

        mod path {
            use super::*;

            const DAEMON_DIR: &str = "/opt/agentdeck/bin";

            fn path(daemon: &[(&str, &str)]) -> Option<OsString> {
                pane_env(vars(daemon), Some(Path::new(DAEMON_DIR)), &[]).remove(OsStr::new("PATH"))
            }

            #[test]
            fn should_end_with_the_daemons_folder_after_the_daemons_path() {
                assert_eq!(
                    path(&[("PATH", "/home/me/bin:/usr/bin")]),
                    Some("/home/me/bin:/usr/bin:/opt/agentdeck/bin".into())
                );
            }

            #[test]
            fn should_be_the_daemons_folder_alone_without_a_path() {
                assert_eq!(path(&[]), Some(DAEMON_DIR.into()));
                assert_eq!(path(&[("PATH", "")]), Some(DAEMON_DIR.into()));
            }

            #[test]
            fn should_leave_the_path_alone_for_a_folder_holding_a_colon() {
                let env = pane_env(
                    vars(&[("PATH", "/usr/bin")]),
                    Some(Path::new("/opt/a:b")),
                    &[],
                );
                assert_eq!(env[OsStr::new("PATH")], "/usr/bin");
            }

            #[test]
            fn should_leave_the_path_alone_without_a_folder() {
                let env = pane_env(vars(&[("PATH", "/usr/bin")]), None, &[]);
                assert_eq!(env[OsStr::new("PATH")], "/usr/bin");
            }
        }
    }
}
