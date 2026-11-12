//! Temp git repos for the dirty report. Commits use a fixed identity, so the tests don't
//! depend on the machine's git config.

use std::path::Path;
use std::process::Command;

/// Runs `git <args>` in `dir` and panics on failure.
pub fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=agentdeck tests",
            "-c",
            "user.email=tests@agentdeck.invalid",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("running git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A repo at `dir` on `main` with one commit.
pub fn init_with_commit(dir: &Path) {
    std::fs::create_dir_all(dir).expect("creating the repo dir");
    git(dir, &["init", "--quiet"]);
    std::fs::write(dir.join("README.md"), "hello\n").expect("writing a file");
    git(dir, &["add", "README.md"]);
    git(dir, &["commit", "--quiet", "-m", "first"]);
}

/// A bare repo at `remote` and a clone of it at `dir` with one commit pushed to `main`.
pub fn clone_with_pushed_commit(remote: &Path, dir: &Path) {
    std::fs::create_dir_all(remote).expect("creating the remote dir");
    git(remote, &["init", "--quiet", "--bare"]);
    let parent = dir.parent().expect("the clone has a parent");
    std::fs::create_dir_all(parent).expect("creating the clone's parent");
    git(
        parent,
        &[
            "clone",
            "--quiet",
            remote.to_str().expect("a UTF-8 path"),
            dir.to_str().expect("a UTF-8 path"),
        ],
    );
    std::fs::write(dir.join("README.md"), "hello\n").expect("writing a file");
    git(dir, &["add", "README.md"]);
    git(dir, &["commit", "--quiet", "-m", "first"]);
    git(dir, &["push", "--quiet", "origin", "main"]);
}
