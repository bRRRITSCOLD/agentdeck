//! GitHub's GraphQL API on httpmock, for `pr ls` and `pr close`: the open PRs a sync stores,
//! with their Repositories, their node ids, and `closePullRequest`. No test reaches the real
//! GitHub.

use httpmock::prelude::*;
use httpmock::{Mock, MockServer};
use serde_json::{Value, json};

/// The signed-in login in these tests: GitHub answers each query for PRs as this viewer.
pub const ME: &str = "octocat";

/// An open PR on `repo`, from `feat/x` into `main`.
#[derive(Clone, Copy)]
pub struct Pr {
    pub repo: &'static str,
    pub number: i64,
    /// Who opened it.
    pub author: &'static str,
    /// Whether its review requests name [`ME`].
    pub requested: bool,
    /// Whether it is assigned to [`ME`].
    pub assigned: bool,
}

impl Pr {
    /// PR `number` of `repo`, opened by [`ME`], with no review requests or assignees.
    pub const fn open(repo: &'static str, number: i64) -> Pr {
        Pr {
            repo,
            number,
            author: ME,
            requested: false,
            assigned: false,
        }
    }

    pub fn node_id(&self) -> String {
        format!("PR_{}_{}", self.repo.replace('/', "_"), self.number)
    }

    /// GitHub's id for its Repository.
    fn repository_id(&self) -> u64 {
        repository_id(self.repo)
    }

    /// The fields a sync reads, light and full alike.
    fn node(&self) -> Value {
        json!({
            "number": self.number,
            "title": format!("PR {}", self.number),
            "url": format!("https://github.com/{}/pull/{}", self.repo, self.number),
            "state": "OPEN",
            "isDraft": false,
            "headRefName": "feat/x",
            "baseRefName": "main",
            "updatedAt": "2026-11-01T10:00:00Z",
            "author": { "login": self.author },
            "reviewRequests": { "nodes": if self.requested {
                json!([{ "requestedReviewer": { "login": ME } }])
            } else {
                json!([])
            } },
            "assignees": { "nodes": if self.assigned { json!([{ "login": ME }]) } else { json!([]) } },
            "repository": {
                "fullDatabaseId": self.repository_id().to_string(),
                "nameWithOwner": self.repo,
                "url": format!("https://github.com/{}", self.repo),
                "isPrivate": false,
                "isArchived": false
            },
            "reviewDecision": null,
            "commits": { "nodes": [] }
        })
    }
}

/// GitHub's id for Repository `repo` in these tests: a different one for each name.
pub fn repository_id(repo: &str) -> u64 {
    repo.bytes().fold(17_u64, |id, byte| {
        id.wrapping_mul(31).wrapping_add(u64::from(byte))
    }) & 0x7fff_ffff
}

/// The REST API's `GET /repos/{owner}/{repo}` for `repo` with `token`, answered with the
/// public Repository GitHub calls `repo`, with its id from [`repository_id`].
pub fn mock_repo<'a>(server: &'a MockServer, token: &str, repo: &str) -> Mock<'a> {
    server.mock(|when, then| {
        when.method(GET)
            .path(format!("/repos/{repo}"))
            .header("authorization", format!("Bearer {token}"));
        then.status(200).json_body(json!({
            "id": repository_id(repo),
            "full_name": repo,
            "html_url": format!("https://github.com/{repo}"),
            "private": false,
            "archived": false
        }));
    })
}

/// The REST API's `GET /repos/{owner}/{repo}` for `repo`, answered 404 as for a Repository
/// that doesn't exist or that the token can't see.
pub fn mock_repo_missing<'a>(server: &'a MockServer, repo: &str) -> Mock<'a> {
    server.mock(|when, then| {
        when.method(GET).path(format!("/repos/{repo}"));
        then.status(404)
            .json_body(json!({ "message": "Not Found" }));
    })
}

/// The mocks [`mock_open_prs`] answers a sync with.
pub struct OpenPrs<'a> {
    /// The list of open PRs you authored, which a full sync reads.
    pub list: Mock<'a>,
    /// The searches for PRs updated since the last sync, which an incremental sync reads.
    pub search: Mock<'a>,
}

/// The viewer's open PRs, `prs`, however a sync reads them for `token`: the list of those
/// [`ME`] authored and the searches for those where your review is requested and those
/// assigned to you (a full sync), the searches for PRs updated since (an incremental one, all
/// of `prs`), and the authored ones' details by number.
pub fn mock_open_prs<'a>(server: &'a MockServer, token: &str, prs: &[Pr]) -> OpenPrs<'a> {
    let authored: Vec<Pr> = prs.iter().copied().filter(|pr| pr.author == ME).collect();
    let list_nodes: Vec<Value> = authored.iter().map(Pr::node).collect();
    let list = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", format!("Bearer {token}"))
            .body_includes("pullRequests(");
        then.status(200).json_body(json!({
            "data": { "viewer": { "login": ME, "pullRequests": {
                "totalCount": list_nodes.len(),
                "pageInfo": { "hasNextPage": false, "endCursor": null },
                "nodes": list_nodes
            } } }
        }));
    });
    for (qualifier, whose) in [
        (
            "user-review-requested:@me",
            (|pr: &Pr| pr.requested) as fn(&Pr) -> bool,
        ),
        ("assignee:@me", |pr: &Pr| pr.assigned),
    ] {
        let found: Vec<Pr> = prs.iter().copied().filter(whose).collect();
        server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .header("authorization", format!("Bearer {token}"))
                .body_includes("search(")
                .body_includes("is:open")
                .body_includes(qualifier);
            then.status(200).json_body(search_body(&found));
        });
    }
    let search = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", format!("Bearer {token}"))
            .body_includes("search(")
            .body_includes("updated:>=");
        then.status(200).json_body(search_body(prs));
    });
    if !authored.is_empty() {
        by_number(server, "fragment pr on PullRequest", &authored, |pr| {
            pr.node()
        });
    }
    OpenPrs { list, search }
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

/// The lookup of `prs`' node ids before a close, in one request.
pub fn mock_node_ids<'a>(server: &'a MockServer, prs: &[Pr]) -> Mock<'a> {
    by_number(
        server,
        "fragment nodeId on PullRequest",
        prs,
        |pr| json!({ "id": pr.node_id() }),
    )
}

/// One batched request by number, matched by `fragment` and its first PR, answering each PR
/// `p<i>` with `pull_request`.
fn by_number<'a>(
    server: &'a MockServer,
    fragment: &str,
    prs: &[Pr],
    pull_request: impl Fn(&Pr) -> Value,
) -> Mock<'a> {
    let first = prs[0];
    let (owner, name) = first.repo.split_once('/').expect("owner/name");
    let mut data: serde_json::Map<String, Value> = prs
        .iter()
        .enumerate()
        .map(|(i, pr)| (format!("p{i}"), json!({ "pullRequest": pull_request(pr) })))
        .collect();
    data.insert("viewer".into(), json!({ "login": ME }));
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes(fragment)
            .json_body_includes(
                json!({ "variables": { "o0": owner, "r0": name, "n0": first.number } }).to_string(),
            );
        then.status(200).json_body(json!({ "data": data }));
    })
}

/// `closePullRequest` for `pr`, answered with the PR closed, or refused with `refusal`.
pub fn mock_close<'a>(server: &'a MockServer, pr: Pr, refusal: Option<&str>) -> Mock<'a> {
    let id = pr.node_id();
    let body = match refusal {
        None => json!({
            "data": { "closePullRequest": { "pullRequest": { "id": id, "state": "CLOSED" } } }
        }),
        Some(message) => json!({
            "data": { "closePullRequest": null },
            "errors": [ { "type": "FORBIDDEN", "path": [ "closePullRequest" ], "message": message } ]
        }),
    };
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("closePullRequest(")
            .json_body_includes(json!({ "variables": { "id": id } }).to_string());
        then.status(200).json_body(body);
    })
}
