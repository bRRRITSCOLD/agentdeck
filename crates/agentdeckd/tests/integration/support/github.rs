//! GitHub for PR sync and sign-in: a stub `gh` on `PATH`, canned GraphQL responses and the
//! device flow's endpoints for httpmock (spec §12). No test reaches the real GitHub.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use httpmock::prelude::*;
use httpmock::{Mock, MockServer};
use serde_json::{Value, json};

/// The signed-in login in these tests: GitHub answers each query for PRs as this viewer.
pub const ME: &str = "octocat";

/// A GraphQL endpoint nothing listens on, for daemons that aren't testing PR sync.
pub const UNREACHABLE: &str = "http://127.0.0.1:9/graphql";

/// The REST API's base URL on the server whose GraphQL endpoint is `graphql`: the same
/// server, so one httpmock answers both.
pub fn rest_api(graphql: &str) -> &str {
    graphql.strip_suffix("/graphql").unwrap_or(graphql)
}

/// A Repository as the REST API's `GET /repos/{owner}/{repo}` answers it.
#[derive(Clone, Copy)]
pub struct RestRepo {
    /// `owner/name`, as GitHub calls it now.
    pub name: &'static str,
    /// GitHub's id for it.
    pub id: u64,
    pub private: bool,
}

impl RestRepo {
    /// Public `name`, with the id [`repository_id`] gives it, as a PR of it carries.
    pub fn new(name: &'static str) -> RestRepo {
        RestRepo {
            name,
            id: repository_id(name),
            private: false,
        }
    }

    pub fn url(&self) -> String {
        format!("https://github.com/{}", self.name)
    }
}

/// `GET /repos/{owner}/{repo}` for `asked` (as typed) with `token`, answered with `repo`: the
/// same Repository, or the one GitHub renamed it to.
pub fn mock_rest_repo<'a>(
    server: &'a MockServer,
    token: &str,
    asked: &str,
    repo: RestRepo,
) -> Mock<'a> {
    server.mock(|when, then| {
        when.method(GET)
            .path(format!("/repos/{asked}"))
            .header("authorization", format!("Bearer {token}"))
            .header("x-github-api-version", "2022-11-28");
        then.status(200).json_body(json!({
            "id": repo.id,
            "node_id": "R_kgDOtest",
            "name": repo.name.split_once('/').expect("owner/name").1,
            "full_name": repo.name,
            "html_url": repo.url(),
            "private": repo.private,
            "archived": false,
            "default_branch": "main"
        }));
    })
}

/// `GET /repos/{owner}/{repo}` for `asked`, answered 404 as GitHub answers for a Repository
/// that doesn't exist or that the token can't see.
pub fn mock_rest_repo_missing<'a>(server: &'a MockServer, asked: &str) -> Mock<'a> {
    server.mock(|when, then| {
        when.method(GET).path(format!("/repos/{asked}"));
        then.status(404).json_body(json!({
            "message": "Not Found",
            "documentation_url": "https://docs.github.com/rest/repos/repos#get-a-repository",
            "status": "404"
        }));
    })
}

/// `count` repositories a user can reach, as `GET /user/repos` sorts them: `acme/repo-000`,
/// `acme/repo-001`…, every 7th private.
pub fn user_repos(count: usize) -> Vec<RestRepo> {
    (0..count)
        .map(|i| RestRepo {
            private: i % 7 == 0,
            ..RestRepo::new(Box::leak(format!("acme/repo-{i:03}").into_boxed_str()))
        })
        .collect()
}

/// The `Link` header GitHub sends with page `page` of `last` of `GET /user/repos` on `server`:
/// `next` and `last` before the last page, `prev` and `first` after the first.
fn user_repos_link(server: &MockServer, page: u32, last: u32) -> String {
    let url = |page: u32| {
        format!(
            "<{}?sort=full_name&direction=asc&per_page=100&page={page}>",
            server.url("/user/repos")
        )
    };
    let mut links = Vec::new();
    if page > 1 {
        links.push(format!("{}; rel=\"prev\"", url(page - 1)));
    }
    if page < last {
        links.push(format!("{}; rel=\"next\"", url(page + 1)));
        links.push(format!("{}; rel=\"last\"", url(last)));
    }
    if page > 1 {
        links.push(format!("{}; rel=\"first\"", url(1)));
    }
    links.join(", ")
}

/// `GET /user/repos?sort=full_name&direction=asc&per_page=100&page=<page>` with `token`,
/// answered after `delay` with `repos`' page `page` (from 1) and its `Link` header.
pub fn mock_user_repos_page<'a>(
    server: &'a MockServer,
    token: &str,
    repos: &[RestRepo],
    page: u32,
    delay: Duration,
) -> Mock<'a> {
    let last = repos.len().div_ceil(100).max(1) as u32;
    let start = (page as usize - 1) * 100;
    let body: Vec<Value> = (repos.iter().skip(start).take(100))
        .map(|repo| {
            let (owner, name) = repo.name.split_once('/').expect("owner/name");
            json!({
                "id": repo.id,
                "node_id": "R_kgDOtest",
                "name": name,
                "full_name": repo.name,
                "owner": { "login": owner },
                "html_url": repo.url(),
                "private": repo.private,
                "archived": false,
                "default_branch": "main"
            })
        })
        .collect();
    let link = user_repos_link(server, page, last);
    server.mock(|when, then| {
        when.method(GET)
            .path("/user/repos")
            .query_param("sort", "full_name")
            .query_param("direction", "asc")
            .query_param("per_page", "100")
            .query_param("page", page.to_string())
            .header("authorization", format!("Bearer {token}"))
            .header("x-github-api-version", "2022-11-28");
        let then = then.status(200).delay(delay).json_body(Value::Array(body));
        if !link.is_empty() {
            then.header("link", link);
        }
    })
}

/// Every page of `repos` for `token`, answered at once, by page from 1.
pub fn mock_user_repos<'a>(
    server: &'a MockServer,
    token: &str,
    repos: &[RestRepo],
) -> Vec<Mock<'a>> {
    let last = repos.len().div_ceil(100).max(1) as u32;
    (1..=last)
        .map(|page| mock_user_repos_page(server, token, repos, page, Duration::ZERO))
        .collect()
}

/// `GET /user/repos` page `page` answered `status` with GitHub's JSON error `message`, and
/// `headers`.
pub fn mock_user_repos_failing<'a>(
    server: &'a MockServer,
    page: u32,
    status: u16,
    message: &str,
    headers: &[(&str, &str)],
) -> Mock<'a> {
    server.mock(|when, then| {
        when.method(GET)
            .path("/user/repos")
            .query_param("page", page.to_string());
        let mut then = then.status(status).json_body(json!({
            "message": message,
            "documentation_url": "https://docs.github.com/rest"
        }));
        for (name, value) in headers {
            then = then.header(*name, *value);
        }
    })
}

/// GitHub's repository search, which agentdeck never calls: any request to it.
pub fn mock_search_repositories(server: &MockServer) -> Mock<'_> {
    server.mock(|when, then| {
        when.path("/search/repositories");
        then.status(200)
            .json_body(json!({ "total_count": 0, "items": [] }));
    })
}

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

/// A PR as GitHub's GraphQL API returns it, through the daemon's fragments.
#[derive(Clone)]
pub struct Pr {
    /// Its Repository's `owner/name` now.
    pub repo: &'static str,
    /// GitHub's id for its Repository; [`repository_id`] of `repo` when `None`, so each name
    /// is its own Repository unless a test says otherwise (a rename).
    pub repo_id: Option<u64>,
    /// Whether its Repository is private.
    pub private: bool,
    pub number: i64,
    pub title: &'static str,
    pub state: &'static str,
    pub head_ref: &'static str,
    pub base_ref: &'static str,
    /// Who opened it; `None` once that account is gone.
    pub author: Option<&'static str>,
    pub draft: bool,
    pub checks: Option<&'static str>,
    pub review: Option<&'static str>,
    pub updated_at: &'static str,
    /// The logins its review requests name (a team's names none).
    pub requested: &'static [&'static str],
    /// Its assignees' logins.
    pub assignees: &'static [&'static str],
}

/// The id GitHub gives Repository `repo` in these tests: the same for the same name.
pub fn repository_id(repo: &str) -> u64 {
    // FNV-1a, kept well inside SQLite's i64.
    let hash = repo.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    hash & 0x7fff_ffff
}

impl Pr {
    /// An open PR on `acme/widgets` from `feat/sync` into `main`, opened by [`ME`], with no
    /// checks, no review, no review requests and no assignees.
    pub fn open(number: i64) -> Pr {
        Pr {
            repo: "acme/widgets",
            repo_id: None,
            private: false,
            number,
            title: "Add PR sync",
            state: "OPEN",
            head_ref: "feat/sync",
            base_ref: "main",
            author: Some("octocat"),
            draft: false,
            checks: None,
            review: None,
            updated_at: "2026-11-01T10:00:00Z",
            requested: &[],
            assignees: &[],
        }
    }

    /// Open PR `number` that someone else opened and that is yours only as `pr` makes it.
    pub fn theirs(number: i64) -> Pr {
        Pr {
            author: Some("hubot"),
            ..Pr::open(number)
        }
    }

    pub fn url(&self) -> String {
        format!("https://github.com/{}/pull/{}", self.repo, self.number)
    }

    /// GitHub's id for its Repository.
    pub fn repository_id(&self) -> u64 {
        self.repo_id.unwrap_or_else(|| repository_id(self.repo))
    }

    /// The PR's GraphQL node id, as the lookup before a close answers it.
    pub fn node_id(&self) -> String {
        format!("PR_{}_{}", self.repo.replace('/', "_"), self.number)
    }

    /// Every field, as a search or a fetch by number returns it.
    fn node(&self) -> Value {
        let mut node = self.light_node();
        node["reviewDecision"] = json!(self.review);
        node["commits"] = json!({ "nodes": [ {
            "commit": { "statusCheckRollup": self.checks.map(|state| json!({ "state": state })) }
        } ] });
        node
    }

    /// The light fields the list of open PRs returns: no review, no checks.
    fn light_node(&self) -> Value {
        json!({
            "number": self.number,
            "title": self.title,
            "url": self.url(),
            "state": self.state,
            "isDraft": self.draft,
            "headRefName": self.head_ref,
            "baseRefName": self.base_ref,
            "updatedAt": self.updated_at,
            "author": self.author.map(|login| json!({ "login": login })),
            "reviewRequests": { "nodes": self.requested.iter()
                .map(|login| json!({ "requestedReviewer": { "login": login } }))
                .collect::<Vec<_>>() },
            "assignees": { "nodes": self.assignees.iter()
                .map(|login| json!({ "login": login }))
                .collect::<Vec<_>>() },
            "repository": {
                // A `BigInt`, which GraphQL writes as a string.
                "fullDatabaseId": self.repository_id().to_string(),
                "nameWithOwner": self.repo,
                "url": format!("https://github.com/{}", self.repo),
                "isPrivate": self.private,
                "isArchived": false
            }
        })
    }
}

/// The instant [`days_ago`] counts back from: the first time a test asks, then the same for
/// the rest of it (nextest runs each test in its own process). Counting from the clock each
/// time gave the same PR two `updatedAt`s once a test crossed a whole second, so a daemon that
/// stored the first saw the second as an update and fetched it again.
static NOW: LazyLock<DateTime<Utc>> = LazyLock::new(Utc::now);

/// An `updatedAt` `days` before [`NOW`], for tests of the PR age cutoff. Leaked, as [`Pr`]
/// holds `'static` strings; a test makes a handful.
pub fn days_ago(days: i64) -> &'static str {
    let at = *NOW - chrono::TimeDelta::days(days);
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true).leak()
}

/// PRs fetched by number in one request, as the daemon batches them.
pub const BATCH: usize = 25;

/// Mocks that answer together for one set of PRs; [`Listing::delete`] removes them all.
pub struct Listing<'a> {
    /// The list of open PRs, or the search.
    pub list: Mock<'a>,
    /// The other mocks: the incremental search and the fetches of details by number.
    others: Vec<Mock<'a>>,
}

impl Listing<'_> {
    pub fn delete(&mut self) {
        self.list.delete();
        for mock in &mut self.others {
            mock.delete();
        }
    }
}

/// The viewer's PRs, however the daemon asks: the list of open PRs (a full sync), the search
/// (an incremental one), and their details by number, all answered with `prs` for `token`.
pub fn mock_search<'a>(server: &'a MockServer, token: &str, prs: &[Pr]) -> Listing<'a> {
    let mut listing = mock_list(server, token, prs);
    listing.others.push(mock_updated_search(server, token, prs));
    listing
}

/// The list of the viewer's open PRs, one page with `prs` for `token`, and their details by
/// number; the full sync's searches for PRs where your review is requested or that are
/// assigned to you find none. An incremental search doesn't match it.
pub fn mock_list<'a>(server: &'a MockServer, token: &str, prs: &[Pr]) -> Listing<'a> {
    let list = mock_list_page(server, token, prs, None, None, prs.len(), Duration::ZERO);
    let mut others: Vec<Mock<'a>> = prs
        .chunks(BATCH)
        .map(|batch| mock_fetch(server, batch))
        .collect();
    others.extend(mock_open_searches(server, token, &[], &[]));
    Listing { list, others }
}

/// One page of the list of the viewer's open PRs for `token`: the one after cursor `after`
/// (`None` for the first), answered with `prs`, `total` as the list's count, and `next` as the
/// next page's cursor, after `delay`.
pub fn mock_list_page<'a>(
    server: &'a MockServer,
    token: &str,
    prs: &[Pr],
    after: Option<&str>,
    next: Option<&str>,
    total: usize,
    delay: Duration,
) -> Mock<'a> {
    let nodes: Vec<Value> = prs.iter().map(Pr::light_node).collect();
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", format!("Bearer {token}"))
            .body_includes("pullRequests(")
            .json_body_includes(json!({ "variables": { "after": after } }).to_string());
        then.status(200).delay(delay).json_body(json!({
            "data": { "viewer": { "login": ME, "pullRequests": {
                "totalCount": total,
                "pageInfo": { "hasNextPage": next.is_some(), "endCursor": next },
                "nodes": nodes
            } } }
        }));
    })
}

/// Every incremental search for the viewer's PRs updated since the last sync (authored,
/// review requested and assigned alike), answered with `prs` for `token`.
pub fn mock_updated_search<'a>(server: &'a MockServer, token: &str, prs: &[Pr]) -> Mock<'a> {
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", format!("Bearer {token}"))
            .body_includes("search(")
            .body_includes("updated:>=");
        then.status(200).json_body(search_body(prs));
    })
}

/// Whose PRs a search asks for, by its qualifier.
#[derive(Clone, Copy, Debug)]
pub enum Whose {
    /// `author:@me`.
    Authored,
    /// `user-review-requested:@me`.
    ReviewRequested,
    /// `assignee:@me`.
    Assigned,
}

impl Whose {
    fn qualifier(self) -> &'static str {
        match self {
            Whose::Authored => "author:@me",
            Whose::ReviewRequested => "user-review-requested:@me",
            Whose::Assigned => "assignee:@me",
        }
    }
}

/// A full sync's search for `whose` open PRs, most recently updated first, answered with
/// `prs` (one page) for `token`.
pub fn mock_open_search<'a>(
    server: &'a MockServer,
    token: &str,
    whose: Whose,
    prs: &[Pr],
) -> Mock<'a> {
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", format!("Bearer {token}"))
            .body_includes("search(")
            .body_includes("is:open")
            .body_includes("sort:updated-desc")
            .body_includes(whose.qualifier());
        then.status(200).json_body(search_body(prs));
    })
}

/// A full sync's searches for open PRs where your review is requested, answered with
/// `requested`, and assigned to you, answered with `assigned`.
pub fn mock_open_searches<'a>(
    server: &'a MockServer,
    token: &str,
    requested: &[Pr],
    assigned: &[Pr],
) -> Vec<Mock<'a>> {
    vec![
        mock_open_search(server, token, Whose::ReviewRequested, requested),
        mock_open_search(server, token, Whose::Assigned, assigned),
    ]
}

/// An incremental search for `whose` PRs updated since the last sync, answered with `prs`
/// for `token`.
pub fn mock_since_search<'a>(
    server: &'a MockServer,
    token: &str,
    whose: Whose,
    prs: &[Pr],
) -> Mock<'a> {
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", format!("Bearer {token}"))
            .body_includes("search(")
            .body_includes("updated:>=")
            .body_includes(whose.qualifier());
        then.status(200).json_body(search_body(prs));
    })
}

/// The viewer's PRs, each way they are yours, however the daemon asks for `token`: the list
/// and the open searches (a full sync) with `authored`, `requested` and `assigned`, every
/// incremental search finding none, and each PR's details by number. Create it before any
/// other mock of the same searches, since the first mock that matches answers.
pub fn mock_yours<'a>(
    server: &'a MockServer,
    token: &str,
    authored: &[Pr],
    requested: &[Pr],
    assigned: &[Pr],
) -> Listing<'a> {
    let mut others = mock_open_searches(server, token, requested, assigned);
    for whose in [Whose::Authored, Whose::ReviewRequested, Whose::Assigned] {
        others.push(mock_since_search(server, token, whose, &[]));
    }
    // The list's new PRs are fetched together, in its order; the rest one at a time.
    others.extend(
        authored
            .chunks(BATCH)
            .map(|batch| mock_fetch(server, batch)),
    );
    for pr in requested.iter().chain(assigned) {
        others.push(mock_fetch(server, std::slice::from_ref(pr)));
    }
    let list = mock_list_page(
        server,
        token,
        authored,
        None,
        None,
        authored.len(),
        Duration::ZERO,
    );
    Listing { list, others }
}

/// A search's response body: one page holding `prs`, as [`ME`] sees them.
fn search_body(prs: &[Pr]) -> Value {
    let nodes: Vec<Value> = prs.iter().map(Pr::node).collect();
    json!({
        "data": {
            "viewer": { "login": ME },
            "search": {
                "issueCount": nodes.len(),
                "pageInfo": { "hasNextPage": false, "endCursor": null },
                "nodes": nodes
            }
        }
    })
}

/// One batched fetch by number, answered with `prs` as they are now. It matches the batch
/// that starts with `prs[0]`, the daemon asking for each as `p<i>`.
pub fn mock_fetch<'a>(server: &'a MockServer, prs: &[Pr]) -> Mock<'a> {
    let first = &prs[0];
    let (owner, name) = first.repo.split_once('/').expect("owner/name");
    let mut data: serde_json::Map<String, Value> = prs
        .iter()
        .enumerate()
        .map(|(i, pr)| (format!("p{i}"), json!({ "pullRequest": pr.node() })))
        .collect();
    data.insert("viewer".into(), json!({ "login": ME }));
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("fragment pr on PullRequest")
            .json_body_includes(
                json!({ "variables": { "o0": owner, "r0": name, "n0": first.number } }).to_string(),
            );
        then.status(200).json_body(json!({ "data": data }));
    })
}

/// A fetch by number of `pr` alone with `token`, answered as GitHub answers for a repo the
/// token can't see: no repository, and a `NOT_FOUND` error on its alias.
pub fn mock_hidden<'a>(server: &'a MockServer, token: &str, pr: &Pr) -> Mock<'a> {
    let (owner, name) = pr.repo.split_once('/').expect("owner/name");
    let message = format!(
        "Could not resolve to a Repository with the name '{}'.",
        pr.repo
    );
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", format!("Bearer {token}"))
            .body_includes("fragment pr on PullRequest")
            .json_body_includes(
                json!({ "variables": { "o0": owner, "r0": name, "n0": pr.number } }).to_string(),
            );
        then.status(200).json_body(json!({
            "data": { "viewer": { "login": ME }, "p0": null },
            "errors": [ { "type": "NOT_FOUND", "path": [ "p0" ], "message": message } ]
        }));
    })
}

/// One batched lookup of node ids before a close, answered with each of `prs`' ids. It
/// matches the batch that starts with `prs[0]`, the daemon asking for each as `p<i>`.
pub fn mock_node_ids<'a>(server: &'a MockServer, prs: &[Pr]) -> Mock<'a> {
    let first = &prs[0];
    let (owner, name) = first.repo.split_once('/').expect("owner/name");
    let data: serde_json::Map<String, Value> = prs
        .iter()
        .enumerate()
        .map(|(i, pr)| {
            (
                format!("p{i}"),
                json!({ "pullRequest": { "id": pr.node_id() } }),
            )
        })
        .collect();
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("fragment nodeId on PullRequest")
            .json_body_includes(
                json!({ "variables": { "o0": owner, "r0": name, "n0": first.number } }).to_string(),
            );
        then.status(200).json_body(json!({ "data": data }));
    })
}

/// The `closePullRequest` mutation for `pr`, answered with the PR closed.
pub fn mock_close<'a>(server: &'a MockServer, pr: &Pr) -> Mock<'a> {
    let id = pr.node_id();
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("closePullRequest(")
            .json_body_includes(json!({ "variables": { "id": id } }).to_string());
        then.status(200).json_body(json!({
            "data": { "closePullRequest": { "pullRequest": { "id": id, "state": "CLOSED" } } }
        }));
    })
}

/// The `closePullRequest` mutation for `pr`, refused with GitHub's `message`, as for a token
/// without write access to its repo.
pub fn mock_close_refused<'a>(server: &'a MockServer, pr: &Pr, message: &str) -> Mock<'a> {
    let id = pr.node_id();
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("closePullRequest(")
            .json_body_includes(json!({ "variables": { "id": id } }).to_string());
        then.status(200).json_body(json!({
            "data": { "closePullRequest": null },
            "errors": [ { "type": "FORBIDDEN", "path": [ "closePullRequest" ], "message": message } ]
        }));
    })
}

/// `viewer { login }` for `token`, answered with `login` and, when `scopes` is `Some`, that
/// `X-OAuth-Scopes` header (a classic token's; a fine-grained one sends none).
pub fn mock_viewer<'a>(
    server: &'a MockServer,
    token: &str,
    login: &str,
    scopes: Option<&str>,
) -> Mock<'a> {
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", format!("Bearer {token}"))
            // The token check alone: queries for PRs ask for the viewer's login too.
            .json_body_includes(json!({ "query": "query { viewer { login } }" }).to_string());
        let then = then
            .status(200)
            .json_body(json!({ "data": { "viewer": { "login": login } } }));
        if let Some(scopes) = scopes {
            then.header("x-oauth-scopes", scopes);
        }
    })
}

/// `POST /login/device/code` for `client_id` with the `repo` scope, answered with the user
/// code `ABCD-1234`, a 1 s poll interval and `expires_in` seconds.
pub fn mock_device_code<'a>(server: &'a MockServer, client_id: &str, expires_in: u64) -> Mock<'a> {
    server.mock(|when, then| {
        when.method(POST)
            .path("/login/device/code")
            .header("accept", "application/json")
            .form_urlencoded_tuple("client_id", client_id)
            .form_urlencoded_tuple("scope", "repo");
        then.status(200).json_body(json!({
            "device_code": DEVICE_CODE,
            "user_code": USER_CODE,
            "verification_uri": "https://github.com/login/device",
            "expires_in": expires_in,
            "interval": 1
        }));
    })
}

/// The device code the mocks hand out; it never leaves the daemon.
pub const DEVICE_CODE: &str = "3584d83530557fdd1f46af8289938c8ef79f9dc5";

/// The user code the mocks hand out.
pub const USER_CODE: &str = "ABCD-1234";

/// `POST /login/oauth/access_token` for the mocks' device code, answered with `body`.
pub fn mock_access_token<'a>(server: &'a MockServer, client_id: &str, body: Value) -> Mock<'a> {
    server.mock(|when, then| {
        when.method(POST)
            .path("/login/oauth/access_token")
            .form_urlencoded_tuple("client_id", client_id)
            .form_urlencoded_tuple("device_code", DEVICE_CODE)
            .form_urlencoded_tuple("grant_type", "urn:ietf:params:oauth:grant-type:device_code");
        then.status(200).json_body(body);
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

#[cfg(test)]
mod tests {
    use super::*;

    mod days_ago {
        use super::*;

        #[test]
        fn should_give_the_same_time_for_the_same_days_all_through_a_test() {
            let first = days_ago(1);
            // Past the next whole second, which `updatedAt` is written to.
            std::thread::sleep(Duration::from_millis(1100));

            assert_eq!(days_ago(1), first);
        }
    }
}
