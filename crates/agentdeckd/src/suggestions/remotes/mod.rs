//! Which Repository a folder's git repo works in (spec §3 Suggestions): its `upstream` remote
//! when that is on GitHub, else its `origin`; any other remote is ignored.
//!
//! `git` itself is asked for the remotes ([`git`]), so each URL is the one git uses, after every
//! config file, include and `insteadOf` rewrite it applies. When `git` is missing, fails or
//! takes longer than [`GIT_TIMEOUT`], the repo's own `config` file is read instead
//! ([`file`](mod@file)).
//! Nothing is cached: Suggestions are worked out only when something they depend on changes,
//! and on a read, so a `git` per Pane folder each time costs little and never goes stale.

mod file;
mod git;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use agentdeck_core::RepositoryName;

/// How long `git remote -v` may take before the repo's `config` file is read instead.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(2);

/// The `git` a folder's remotes are asked of: `git` on the daemon's `PATH`.
pub struct Git {
    program: OsString,
    timeout: Duration,
}

impl Default for Git {
    fn default() -> Git {
        Git {
            program: "git".into(),
            timeout: GIT_TIMEOUT,
        }
    }
}

/// The Repository the git repo holding `folder` works in: its `upstream` remote when that is on
/// GitHub, else its `origin` when that is. `None` outside a git repo, or when neither is.
pub fn repository_of(folder: &Path, git: &Git) -> Option<RepositoryName> {
    let remotes = match git::remote_urls(&git.program, folder, git.timeout) {
        Ok(remotes) => remotes,
        Err(err) => {
            tracing::debug!(
                "asking git for {}'s remotes: {err}; reading its config file instead",
                folder.display()
            );
            file::remote_urls_of(folder)?
        }
    };
    pick(&remotes)
}

/// `upstream` when it is on GitHub, else `origin` when it is.
fn pick(remotes: &HashMap<String, String>) -> Option<RepositoryName> {
    ["upstream", "origin"]
        .into_iter()
        .find_map(|remote| RepositoryName::from_remote_url(remotes.get(remote)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picked(text: &str) -> Option<String> {
        pick(&file::remote_urls(text)).map(|name| name.to_string())
    }

    mod pick {
        use super::*;

        #[test]
        fn should_prefer_upstream_on_github() {
            let config = "[remote \"origin\"]\nurl = git@github.com:me/widgets.git\n\
                          [remote \"upstream\"]\nurl = https://github.com/acme/widgets.git\n";

            assert_eq!(picked(config).as_deref(), Some("acme/widgets"));
        }

        #[test]
        fn should_take_origin_when_upstream_is_not_on_github() {
            let config = "[remote \"origin\"]\nurl = git@github.com:me/widgets.git\n\
                          [remote \"upstream\"]\nurl = https://gitlab.com/acme/widgets.git\n";

            assert_eq!(picked(config).as_deref(), Some("me/widgets"));
        }

        #[test]
        fn should_ignore_every_other_remote() {
            let config = "[remote \"fork\"]\nurl = https://github.com/me/widgets\n\
                          [remote \"origin\"]\nurl = https://gitlab.com/acme/widgets\n";

            assert_eq!(picked(config), None);
        }
    }

    mod repository_of {
        use super::*;

        /// A `git` that can't start, so the repo's `config` file answers.
        fn no_git(dir: &Path) -> Git {
            Git {
                program: dir.join("no-git-here").into(),
                timeout: GIT_TIMEOUT,
            }
        }

        #[test]
        fn should_read_the_repos_config_when_git_cannot_start() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("repo/.git")).unwrap();
            std::fs::write(
                dir.path().join("repo/.git/config"),
                "[remote \"origin\"]\n\turl = https://github.com/acme/widgets\n",
            )
            .unwrap();

            assert_eq!(
                repository_of(&dir.path().join("repo"), &no_git(dir.path()))
                    .map(|n| n.to_string())
                    .as_deref(),
                Some("acme/widgets")
            );
        }

        #[test]
        fn should_find_none_outside_a_repo_when_git_cannot_start() {
            let dir = tempfile::tempdir().unwrap();

            assert_eq!(repository_of(dir.path(), &no_git(dir.path())), None);
        }
    }
}
