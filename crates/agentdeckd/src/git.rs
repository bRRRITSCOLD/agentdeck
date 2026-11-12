//! The dirty report: work under a workspace's dir that deleting it would lose (spec §4).
//!
//! Shells out to the `git` CLI, so it works with whatever git the user has configured.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::model::{DirtyRepo, DirtyReport};

/// How deep under the workspace dir to look for repos: the dir itself, the repos cloned into
/// it and one level of grouping folders. A repo's own subdirs are never searched.
const MAX_DEPTH: usize = 3;

/// Every git repo under `dir` with uncommitted files or branches that are on no remote.
pub fn dirty_report(dir: &Path) -> io::Result<DirtyReport> {
    let mut repos = Vec::new();
    for repo in find_repos(dir, MAX_DEPTH)? {
        let uncommitted = parse_status(&git(&repo, &["status", "--porcelain=v1"])?);
        let mut unpushed_branches = Vec::new();
        let branches = git(
            &repo,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
        )?;
        for branch in branches.lines().filter(|line| !line.is_empty()) {
            let full = format!("refs/heads/{branch}");
            let ahead = git(&repo, &["rev-list", "--count", &full, "--not", "--remotes"])?;
            if ahead.trim() != "0" {
                unpushed_branches.push(branch.to_owned());
            }
        }
        if !uncommitted.is_empty() || !unpushed_branches.is_empty() {
            repos.push(DirtyRepo {
                path: repo.display().to_string(),
                uncommitted,
                unpushed_branches,
            });
        }
    }
    Ok(DirtyReport { repos })
}

/// The dirs at or under `dir`, `depth` levels down at most, that hold a `.git`. Symlinks are
/// not followed and a repo's subdirs are not searched. A missing `dir` has no repos.
pub(crate) fn find_repos(dir: &Path, depth: usize) -> io::Result<Vec<PathBuf>> {
    if dir.join(".git").exists() {
        return Ok(vec![dir.to_path_buf()]);
    }
    let mut repos = Vec::new();
    if depth == 0 {
        return Ok(repos);
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(repos),
        Err(err) => return Err(err),
    };
    let mut children: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            children.push(entry.path());
        }
    }
    children.sort();
    for child in children {
        repos.extend(find_repos(&child, depth - 1)?);
    }
    Ok(repos)
}

/// The paths in `git status --porcelain=v1` output, as git prints them.
fn parse_status(porcelain: &str) -> Vec<String> {
    porcelain
        .lines()
        .filter_map(|line| line.get(3..))
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Runs `git -C repo <args>` and returns its stdout; a non-zero exit is an error.
fn git(repo: &Path, args: &[&str]) -> io::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git {} in {} failed: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    mod parse_status {
        use super::*;

        #[test]
        fn should_list_modified_staged_and_untracked_paths() {
            let porcelain = " M src/main.rs\nA  new.rs\n?? scratch.txt\n";
            assert_eq!(
                parse_status(porcelain),
                vec!["src/main.rs", "new.rs", "scratch.txt"]
            );
        }

        #[test]
        fn should_keep_a_rename_as_git_prints_it() {
            assert_eq!(
                parse_status("R  old.rs -> new.rs\n"),
                vec!["old.rs -> new.rs"]
            );
        }

        #[test]
        fn should_list_nothing_for_a_clean_repo() {
            assert!(parse_status("").is_empty());
        }
    }
}
