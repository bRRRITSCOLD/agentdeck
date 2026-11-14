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

/// Any search for the viewer's PRs, full or incremental, answered with `prs` for `token`.
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

/// The full search for the viewer's open PRs, answered with `prs` for `token`. Unlike
/// [`mock_search`], an incremental search doesn't match it.
pub fn mock_full_search<'a>(server: &'a MockServer, token: &str, prs: &[Pr]) -> Mock<'a> {
    mock_search_for(
        server,
        token,
        prs,
        json!({ "variables": { "q": "is:pr is:open author:@me" } }),
    )
}

/// The incremental search for the viewer's PRs updated since the last sync, answered with
/// `prs` for `token`.
pub fn mock_updated_search<'a>(server: &'a MockServer, token: &str, prs: &[Pr]) -> Mock<'a> {
    let nodes: Vec<Value> = prs.iter().map(Pr::node).collect();
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", format!("Bearer {token}"))
            .body_includes("search(")
            .body_includes("updated:>=");
        then.status(200).json_body(search_page(nodes));
    })
}

fn mock_search_for<'a>(server: &'a MockServer, token: &str, prs: &[Pr], body: Value) -> Mock<'a> {
    let nodes: Vec<Value> = prs.iter().map(Pr::node).collect();
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", format!("Bearer {token}"))
            .body_includes("search(")
            .json_body_includes(body.to_string());
        then.status(200).json_body(search_page(nodes));
    })
}

fn search_page(nodes: Vec<Value>) -> Value {
    json!({
        "data": { "search": {
            "pageInfo": { "hasNextPage": false, "endCursor": null },
            "nodes": nodes
        } }
    })
}

/// One page of the full search for `token`: the one after cursor `after` (`None` for the
/// first), answered with `prs`, `total` as the search's count, and `next` as the next page's
/// cursor, after `delay`.
pub fn mock_search_page<'a>(
    server: &'a MockServer,
    token: &str,
    prs: &[Pr],
    after: Option<&str>,
    next: Option<&str>,
    total: usize,
    delay: std::time::Duration,
) -> Mock<'a> {
    let nodes: Vec<Value> = prs.iter().map(Pr::node).collect();
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", format!("Bearer {token}"))
            .body_includes("search(")
            .json_body_includes(json!({ "variables": { "after": after } }).to_string());
        then.status(200).delay(delay).json_body(json!({
            "data": { "search": {
                "issueCount": total,
                "pageInfo": { "hasNextPage": next.is_some(), "endCursor": next },
                "nodes": nodes
            } }
        }));
    })
}

/// One batched fetch by number, answered with `prs` as they are now. It matches the batch
/// that starts with `prs[0]`, the daemon asking for each as `p<i>`.
pub fn mock_fetch<'a>(server: &'a MockServer, prs: &[Pr]) -> Mock<'a> {
    let first = &prs[0];
    let (owner, name) = first.repo.split_once('/').expect("owner/name");
    let data: serde_json::Map<String, Value> = prs
        .iter()
        .enumerate()
        .map(|(i, pr)| (format!("p{i}"), json!({ "pullRequest": pr.node() })))
        .collect();
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("pullRequest(")
            .json_body_includes(
                json!({ "variables": { "o0": owner, "r0": name, "n0": first.number } }).to_string(),
            );
        then.status(200).json_body(json!({ "data": data }));
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
