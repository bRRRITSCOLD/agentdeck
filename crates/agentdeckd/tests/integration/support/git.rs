//! Temp git repos. Commits use a fixed identity, so the tests don't
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
