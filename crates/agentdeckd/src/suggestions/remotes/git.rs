//! Asking `git` for a folder's remotes (spec §3 Suggestions): one `git -C <folder> remote -v`,
//! which prints each remote's URL as git uses it, after `include`, `includeIf` and
//! `url.<base>.insteadOf` from every config file git reads. No shell runs it, and it gets the
//! daemon's own environment, so the user's git config applies, less the variables that would
//! point git at another repo than the folder's.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// The variables that would point git at another repo than `-C`'s folder.
const REPO_VARS: [&str; 3] = ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR"];

/// Why `git` gave no remotes.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("starting {0:?}: {1}")]
    Start(std::ffi::OsString, std::io::Error),
    #[error("`git remote -v` took longer than {0:?}")]
    TimedOut(Duration),
    #[error("`git remote -v` exited with {0}")]
    Failed(ExitStatus),
    #[error("reading `git remote -v`: {0}")]
    Read(std::io::Error),
}

/// Each remote's fetch URL by its name, as `<program> -C <folder> remote -v` prints them. A
/// `git` still running after `timeout` is killed.
pub fn remote_urls(
    program: &OsStr,
    folder: &Path,
    timeout: Duration,
) -> Result<HashMap<String, String>, GitError> {
    let mut command = Command::new(program);
    command
        .arg("-C")
        .arg(folder)
        .args(["remote", "-v"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for var in REPO_VARS {
        command.env_remove(var);
    }
    let mut child = command
        .spawn()
        .map_err(|err| GitError::Start(program.to_owned(), err))?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    // The output is read on its own thread, so a `git` that hangs is waited for no longer than
    // `timeout`. The thread ends when git's stdout closes, which killing it does.
    let (sent, output) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let read = stdout.read_to_end(&mut out).map(|_| out);
        let _ = sent.send(read);
    });
    let out = match output.recv_timeout(timeout) {
        Ok(read) => read.map_err(GitError::Read)?,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(GitError::TimedOut(timeout));
        }
    };
    let status = child.wait().map_err(GitError::Read)?;
    if !status.success() {
        return Err(GitError::Failed(status));
    }
    Ok(parse(&String::from_utf8_lossy(&out)))
}

/// Each remote's fetch URL in `git remote -v`'s `output`, from its `<name>\t<url> (fetch)`
/// line (which a partial clone's ` [<filter>]` follows).
fn parse(output: &str) -> HashMap<String, String> {
    (output.lines())
        .filter_map(|line| {
            let (name, rest) = line.split_once('\t')?;
            let (url, _) = rest.split_once(" (fetch)")?;
            let url = url.trim();
            (!url.is_empty()).then(|| (name.to_owned(), url.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    mod parse {
        use super::*;

        fn parsed(output: &str) -> Vec<(String, String)> {
            let mut urls: Vec<_> = parse(output).into_iter().collect();
            urls.sort();
            urls
        }

        #[test]
        fn should_read_each_remotes_fetch_url_by_its_name() {
            let output = "origin\thttps://github.com/me/widgets (fetch)\n\
                          origin\thttps://github.com/me/widgets (push)\n\
                          upstream\tgit@github.com:acme/widgets.git (fetch)\n\
                          upstream\tno-pushing (push)\n";

            assert_eq!(
                parsed(output),
                [
                    ("origin".into(), "https://github.com/me/widgets".into()),
                    ("upstream".into(), "git@github.com:acme/widgets.git".into()),
                ]
            );
        }

        #[test]
        fn should_read_a_partial_clones_url_without_its_filter() {
            let output = "origin\thttps://github.com/a/b (fetch) [blob:none]\n";

            assert_eq!(
                parsed(output),
                [("origin".into(), "https://github.com/a/b".into())]
            );
        }

        #[test]
        fn should_skip_a_remote_without_a_url() {
            assert_eq!(parsed("origin\t (fetch)\norigin\t (push)\n"), []);
        }

        #[test]
        fn should_read_nothing_from_a_repo_without_remotes() {
            assert_eq!(parsed(""), []);
        }
    }

    mod remote_urls {
        use super::*;

        #[test]
        fn should_fail_to_start_a_git_that_is_not_there() {
            let dir = tempfile::tempdir().unwrap();
            let missing = dir.path().join("no-git-here");

            let err = remote_urls(missing.as_os_str(), dir.path(), Duration::from_secs(2));

            assert!(matches!(err, Err(GitError::Start(..))), "{err:?}");
        }
    }
}
