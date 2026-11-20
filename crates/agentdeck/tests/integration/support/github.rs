//! GitHub's GraphQL API on httpmock, for `pr close`: the open PRs a sync stores as PR tasks,
//! their node ids, and `closePullRequest`. No test reaches the real GitHub.

use httpmock::prelude::*;
use httpmock::{Mock, MockServer};
use serde_json::{Value, json};

/// An open PR on `repo`.
#[derive(Clone, Copy)]
pub struct Pr {
    pub repo: &'static str,
    pub number: i64,
}

impl Pr {
    pub fn node_id(&self) -> String {
        format!("PR_{}_{}", self.repo.replace('/', "_"), self.number)
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
            "updatedAt": "2026-11-01T10:00:00Z",
            "repository": { "nameWithOwner": self.repo },
            "reviewDecision": null,
            "commits": { "nodes": [] }
        })
    }
}

/// The mocks [`mock_open_prs`] answers a sync with.
pub struct OpenPrs<'a> {
    /// The list of open PRs, which a full sync reads.
    pub list: Mock<'a>,
    /// The search, which an incremental sync reads.
    pub search: Mock<'a>,
}

/// The viewer's open PRs, `prs`, however a sync reads them for `token`: the list (a full
/// sync), the search (an incremental one) and their details by number.
pub fn mock_open_prs<'a>(server: &'a MockServer, token: &str, prs: &[Pr]) -> OpenPrs<'a> {
    let nodes: Vec<Value> = prs.iter().map(Pr::node).collect();
    let list = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", format!("Bearer {token}"))
            .body_includes("pullRequests(");
        then.status(200).json_body(json!({
            "data": { "viewer": { "pullRequests": {
                "totalCount": nodes.len(),
                "pageInfo": { "hasNextPage": false, "endCursor": null },
                "nodes": nodes
            } } }
        }));
    });
    let search = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", format!("Bearer {token}"))
            .body_includes("search(");
        then.status(200).json_body(json!({
            "data": { "search": {
                "issueCount": nodes.len(),
                "pageInfo": { "hasNextPage": false, "endCursor": null },
                "nodes": nodes
            } }
        }));
    });
    by_number(server, "fragment pr on PullRequest", prs, |pr| pr.node());
    OpenPrs { list, search }
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
    let data: serde_json::Map<String, Value> = prs
        .iter()
        .enumerate()
        .map(|(i, pr)| (format!("p{i}"), json!({ "pullRequest": pull_request(pr) })))
        .collect();
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
