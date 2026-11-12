//! GitHub for PR sync: a stub `gh` on `PATH` and canned GraphQL responses for httpmock
//! (spec §12). No test reaches the real GitHub.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use httpmock::prelude::*;
use httpmock::{Mock, MockServer};
use serde_json::{Value, json};

/// A GraphQL endpoint nothing listens on, for daemons that aren't testing PR sync.
pub const UNREACHABLE: &str = "http://127.0.0.1:9/graphql";

/// The stub `gh auth token`: prints `<root>/gh-token`, or fails as a logged-out `gh` does.
const STUB_GH: &str = r#"#!/bin/sh
[ "$1 $2" = "auth token" ] || { echo "stub gh: unsupported: $*" >&2; exit 2; }
token="$(dirname "$0")/../gh-token"
[ -f "$token" ] || { echo "no oauth token found for github.com" >&2; exit 1; }
cat "$token"
"#;

/// Writes the stub `gh` to `<root>/bin` and returns a `PATH` that finds it first.
pub fn path_with_stub_gh(root: &Path) -> OsString {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("creating the stub bin dir");
    let gh = bin.join("gh");
    std::fs::write(&gh, STUB_GH).expect("writing the stub gh");
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755))
        .expect("making the stub gh executable");
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).expect("a PATH")
}

/// Makes the stub `gh` under `root` print `token`, or (with `None`) log out.
pub fn set_gh_token(root: &Path, token: Option<&str>) {
    let file = gh_token_file(root);
    match token {
        Some(token) => std::fs::write(file, format!("{token}\n")).expect("writing the gh token"),
        None => {
            let _ = std::fs::remove_file(file);
        }
    }
}

fn gh_token_file(root: &Path) -> PathBuf {
    root.join("gh-token")
}

/// A PR as GitHub's GraphQL API returns it, through the daemon's `pr` fragment.
#[derive(Clone)]
pub struct Pr {
    pub repo: &'static str,
    pub number: i64,
    pub title: &'static str,
    pub state: &'static str,
    pub head_ref: &'static str,
    pub draft: bool,
    pub checks: Option<&'static str>,
    pub review: Option<&'static str>,
}

impl Pr {
    /// An open PR on `acme/widgets` from `feat/sync`, with no checks and no review.
    pub fn open(number: i64) -> Pr {
        Pr {
            repo: "acme/widgets",
            number,
            title: "Add PR sync",
            state: "OPEN",
            head_ref: "feat/sync",
            draft: false,
            checks: None,
            review: None,
        }
    }

    pub fn url(&self) -> String {
        format!("https://github.com/{}/pull/{}", self.repo, self.number)
    }

    fn node(&self) -> Value {
        json!({
            "number": self.number,
            "title": self.title,
            "url": self.url(),
            "state": self.state,
            "isDraft": self.draft,
            "headRefName": self.head_ref,
            "updatedAt": "2026-11-01T10:00:00Z",
            "repository": { "nameWithOwner": self.repo },
            "reviewDecision": self.review,
            "commits": { "nodes": [ {
                "commit": { "statusCheckRollup": self.checks.map(|state| json!({ "state": state })) }
            } ] }
        })
    }
}

/// The search for the viewer's open PRs, answered with `prs` for `token`.
pub fn mock_search<'a>(server: &'a MockServer, token: &str, prs: &[Pr]) -> Mock<'a> {
    let nodes: Vec<Value> = prs.iter().map(Pr::node).collect();
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", format!("Bearer {token}"))
            .body_includes("search(");
        then.status(200).json_body(json!({
            "data": { "search": {
                "pageInfo": { "hasNextPage": false, "endCursor": null },
                "nodes": nodes
            } }
        }));
    })
}

/// The fetch of `pr` by number, answered with `pr` as it is now.
pub fn mock_fetch<'a>(server: &'a MockServer, pr: &Pr) -> Mock<'a> {
    let (owner, name) = pr.repo.split_once('/').expect("owner/name");
    let node = pr.node();
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("pullRequest(")
            .json_body_includes(
                json!({ "variables": { "owner": owner, "name": name, "number": pr.number } })
                    .to_string(),
            );
        then.status(200)
            .json_body(json!({ "data": { "repository": { "pullRequest": node } } }));
    })
}

/// Every GraphQL call with `token` answered 401.
pub fn mock_unauthorized<'a>(server: &'a MockServer, token: &str) -> Mock<'a> {
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", format!("Bearer {token}"));
        then.status(401)
            .json_body(json!({ "message": "Bad credentials" }));
    })
}
