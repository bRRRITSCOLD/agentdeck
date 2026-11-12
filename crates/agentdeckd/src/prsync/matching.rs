//! Which workspace a PR belongs to (spec §8): the one whose dir holds a clone of the PR's repo
//! with the PR's branch checked out or present locally.
//!
//! Shells out to the `git` CLI, like the dirty report, so it sees what the user's git sees.

use std::path::{Path, PathBuf};
use std::process::Command;

use agentdeck_core::WorkspaceId;

use crate::git::find_repos;

/// How deep under a workspace's dir to look for clones, as the dirty report does.
const MAX_DEPTH: usize = 3;

/// The GitHub clones under each workspace's dir, read once per sync.
pub struct Clones {
    workspaces: Vec<(WorkspaceId, Vec<RepoClone>)>,
}

struct RepoClone {
    path: PathBuf,
    /// `owner/name` of each GitHub remote, lowercased.
    repos: Vec<String>,
}

impl Clones {
    /// Reads the clones under each `(workspace, dir)`, in the order given: the first workspace
    /// that matches a PR wins. A dir that can't be read holds no clones.
    pub fn scan(workspaces: &[(WorkspaceId, PathBuf)]) -> Clones {
        let workspaces = workspaces
            .iter()
            .map(|(id, dir)| {
                let clones = find_repos(dir, MAX_DEPTH)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|path| RepoClone {
                        repos: remote_urls(&path)
                            .iter()
                            .filter_map(|url| github_repo(url))
                            .collect(),
                        path,
                    })
                    .filter(|clone| !clone.repos.is_empty())
                    .collect();
                (*id, clones)
            })
            .collect();
        Clones { workspaces }
    }

    /// The first workspace holding a clone of `repo` (`owner/name`) that has the local branch
    /// `head_ref`.
    pub fn workspace_for(&self, repo: &str, head_ref: &str) -> Option<WorkspaceId> {
        let repo = repo.to_lowercase();
        self.workspaces.iter().find_map(|(id, clones)| {
            clones
                .iter()
                .any(|clone| clone.repos.contains(&repo) && has_branch(&clone.path, head_ref))
                .then_some(*id)
        })
    }
}

/// Every remote URL configured in `repo`.
fn remote_urls(repo: &Path) -> Vec<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["config", "--get-regexp", r"^remote\..*\.url$"])
        .output();
    match output {
        // Exit 1 means no remotes.
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.split_once(' ').map(|(_, url)| url.to_owned()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether `repo` has the local branch `branch`.
fn has_branch(repo: &Path, branch: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .output()
        .is_ok_and(|output| output.status.success())
}

/// `owner/name`, lowercased, when `url` is a github.com remote in any of git's URL forms.
pub fn github_repo(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = if let Some((scheme, rest)) = url.split_once("://") {
        if !matches!(scheme, "https" | "http" | "ssh" | "git") {
            return None;
        }
        let rest = rest
            .rsplit_once('@')
            .map_or(rest, |(_, host_path)| host_path);
        let (host, path) = rest.split_once('/')?;
        let host = host.split_once(':').map_or(host, |(host, _port)| host);
        if !host.eq_ignore_ascii_case("github.com") {
            return None;
        }
        path
    } else {
        // scp-like: [user@]github.com:owner/name
        let (host, path) = url.split_once(':')?;
        let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
        if !host.eq_ignore_ascii_case("github.com") {
            return None;
        }
        path
    };
    let path = rest.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some(format!("{owner}/{name}").to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    mod github_repo {
        use super::*;

        #[test]
        fn should_read_an_https_url() {
            assert_eq!(
                github_repo("https://github.com/Acme/Widgets.git").as_deref(),
                Some("acme/widgets")
            );
            assert_eq!(
                github_repo("https://github.com/acme/widgets").as_deref(),
                Some("acme/widgets")
            );
        }

        #[test]
        fn should_read_an_scp_like_ssh_url() {
            assert_eq!(
                github_repo("git@github.com:acme/widgets.git").as_deref(),
                Some("acme/widgets")
            );
        }

        #[test]
        fn should_read_an_ssh_url_with_a_port() {
            assert_eq!(
                github_repo("ssh://git@github.com:22/acme/widgets.git").as_deref(),
                Some("acme/widgets")
            );
        }

        #[test]
        fn should_ignore_a_trailing_slash() {
            assert_eq!(
                github_repo("https://github.com/acme/widgets/").as_deref(),
                Some("acme/widgets")
            );
        }

        #[test]
        fn should_reject_another_host() {
            assert_eq!(github_repo("https://gitlab.com/acme/widgets.git"), None);
            assert_eq!(github_repo("git@example.com:acme/widgets.git"), None);
        }

        #[test]
        fn should_reject_a_local_path() {
            assert_eq!(github_repo("/srv/git/widgets.git"), None);
            assert_eq!(github_repo("file:///srv/git/widgets.git"), None);
        }

        #[test]
        fn should_reject_a_path_that_is_not_owner_and_name() {
            assert_eq!(github_repo("https://github.com/acme"), None);
            assert_eq!(github_repo("https://github.com/acme/widgets/pull/7"), None);
        }
    }
}
