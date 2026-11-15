//! GitHub's GraphQL API (spec §8): the viewer's login and scopes, the list of the viewer's
//! open PRs with light fields, the search for PRs updated since a time, and PRs by number
//! with every field, in batches.

use std::time::Duration;

use agentdeck_core::{ChecksState, PrState, ReviewState};
use serde::Deserialize;
use serde_json::{Value, json};

/// Where the GraphQL API lives unless `AGENTDECK_GITHUB_API` says otherwise.
pub const DEFAULT_ENDPOINT: &str = "https://api.github.com/graphql";

/// The incremental search: every PR the token's user authored, in any state, that changed at
/// or after `since` (RFC 3339).
pub fn updated_search(since: &str) -> String {
    format!("is:pr author:@me updated:>={since}")
}

/// Search results per page, and how many pages one sync reads at most.
pub const PAGE_SIZE: u32 = 50;
pub const MAX_PAGES: u32 = 10;

/// Open PRs per page of the list, and how many pages one sync reads at most.
pub const LIST_PAGE_SIZE: u32 = 100;
pub const MAX_LIST_PAGES: u32 = 10;

/// PRs fetched by number in one request, and how many such requests are in flight at once.
/// On 2026-11-14, 271 PRs took 41 s as 50s one at a time, and 18 s as 25s four at a time.
const BATCH_SIZE: usize = 25;
const BATCHES_IN_FLIGHT: usize = 4;

/// The waits before each retry of a request that failed for a reason that may pass: a 5xx, a
/// failed connection, a body cut short. One try, then one more per wait.
const RETRY_WAITS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(2)];

const PR_FIELDS: &str = "fragment pr on PullRequest {
  number title url state isDraft headRefName updatedAt
  repository { nameWithOwner }
  reviewDecision
  commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
}";

/// The list's light fields: no review and no checks, the costly ones (the rollup made a
/// 100-PR page time out).
const LIGHT_FIELDS: &str = "fragment light on PullRequest {
  number title url state isDraft headRefName updatedAt
  repository { nameWithOwner }
}";

/// A pull request as GitHub reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePr {
    /// `owner/name` of the repo the PR targets.
    pub repo: String,
    /// The PR's number in that repo.
    pub number: i64,
    /// The PR's title.
    pub title: String,
    /// The PR's web URL.
    pub url: String,
    /// Open, merged or closed.
    pub state: PrState,
    /// A draft PR.
    pub draft: bool,
    /// The branch the PR merges from.
    pub head_ref: String,
    /// The head commit's rolled-up checks; `None` when it has none.
    pub checks: Option<ChecksState>,
    /// The review decision; `None` when the repo requires no review.
    pub review: Option<ReviewState>,
    /// When the PR last changed on GitHub.
    pub updated_at: String,
}

/// Who a token belongs to, and what it may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    /// The token's user.
    pub login: String,
    /// A classic token's scopes, from `X-OAuth-Scopes`; `None` when GitHub sends no such
    /// header, as for a fine-grained token.
    pub scopes: Option<Vec<String>>,
}

/// Why a GitHub call failed.
#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    /// GitHub refused the token (401).
    #[error("GitHub rejected the token")]
    Unauthorized,
    /// Any other non-success HTTP status.
    #[error("GitHub answered {status}{}", message.as_deref().map(|m| format!(": {m}")).unwrap_or_default())]
    Status {
        /// The status code.
        status: u16,
        /// The `message` of a JSON body; `None` for any other body, such as an HTML error page.
        message: Option<String>,
    },
    /// The response held GraphQL errors and no data.
    #[error("GitHub GraphQL errors: {0}")]
    GraphQl(String),
    /// The request failed, or the response wasn't the JSON asked for.
    #[error("calling GitHub: {0}")]
    Http(#[from] reqwest::Error),
    /// The response's data didn't have the expected shape.
    #[error("unexpected GitHub response: {0}")]
    Shape(#[from] serde_json::Error),
}

/// A GraphQL client for one endpoint.
#[derive(Clone)]
pub struct Github {
    http: reqwest::Client,
    endpoint: String,
}

impl Github {
    /// A client for the GraphQL API at `endpoint`.
    pub fn new(endpoint: String) -> anyhow::Result<Github> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("agentdeck/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Github { http, endpoint })
    }

    /// Who `token` belongs to, and its scopes. A token GitHub rejects is
    /// [`GithubError::Unauthorized`].
    pub async fn viewer(&self, token: &str) -> Result<Viewer, GithubError> {
        #[derive(Deserialize)]
        struct Data {
            viewer: Login,
        }
        #[derive(Deserialize)]
        struct Login {
            login: String,
        }
        let (data, scopes) = self
            .query_with_scopes(token, "query { viewer { login } }", json!({}))
            .await?;
        let data: Data = serde_json::from_value(data)?;
        Ok(Viewer {
            login: data.viewer.login,
            scopes: scopes.as_deref().map(parse_scopes),
        })
    }

    /// The page of the search `q` after cursor `after` (`None` for the first page).
    pub async fn search_page(
        &self,
        token: &str,
        q: &str,
        after: Option<&str>,
    ) -> Result<SearchPage, GithubError> {
        let query = format!(
            "query($q: String!, $first: Int!, $after: String) {{
  search(query: $q, type: ISSUE, first: $first, after: $after) {{
    issueCount
    pageInfo {{ hasNextPage endCursor }}
    nodes {{ ...pr }}
  }}
}}
{PR_FIELDS}"
        );
        let variables = json!({ "q": q, "first": PAGE_SIZE, "after": after });
        parse_search(self.query(token, &query, variables).await?)
    }

    /// The page of the viewer's open PRs after cursor `after` (`None` for the first), most
    /// recently updated first, with light fields only: `checks` and `review` are `None`.
    pub async fn list_page(
        &self,
        token: &str,
        after: Option<&str>,
    ) -> Result<ListPage, GithubError> {
        let query = format!(
            "query($first: Int!, $after: String) {{
  viewer {{
    pullRequests(states: OPEN, first: $first, after: $after, orderBy: {{ field: UPDATED_AT, direction: DESC }}) {{
      totalCount
      pageInfo {{ hasNextPage endCursor }}
      nodes {{ ...light }}
    }}
  }}
}}
{LIGHT_FIELDS}"
        );
        let variables = json!({ "first": LIST_PAGE_SIZE, "after": after });
        parse_list(self.query(token, &query, variables).await?)
    }

    /// Each of `wanted` (`(repo, number)`) by number with every field, in order: `BATCH_SIZE`
    /// to a request, `BATCHES_IN_FLIGHT` requests at once. `None` for one GitHub no longer
    /// shows to the token's user.
    pub async fn prs(
        &self,
        token: &str,
        wanted: &[(String, i64)],
    ) -> Result<Vec<Option<RemotePr>>, GithubError> {
        use futures_util::{StreamExt as _, TryStreamExt as _};

        let batches: Vec<Vec<(String, i64)>> =
            wanted.chunks(BATCH_SIZE).map(<[_]>::to_vec).collect();
        let fetches = batches.into_iter().map(|batch| self.batch(token, batch));
        let found: Vec<Vec<Option<RemotePr>>> = futures_util::stream::iter(fetches)
            .buffered(BATCHES_IN_FLIGHT)
            .try_collect()
            .await?;
        Ok(found.into_iter().flatten().collect())
    }

    /// One request for `batch` by number.
    async fn batch(
        &self,
        token: &str,
        batch: Vec<(String, i64)>,
    ) -> Result<Vec<Option<RemotePr>>, GithubError> {
        let (query, variables) = batch_query(&batch);
        let data = self.query(token, &query, variables).await?;
        parse_batch(data, batch.len())
    }

    /// Posts one GraphQL query and returns its `data`, trying again after each of
    /// `RETRY_WAITS` while it fails for a reason that may pass.
    async fn query(
        &self,
        token: &str,
        query: &str,
        variables: Value,
    ) -> Result<Value, GithubError> {
        Ok(self.query_with_scopes(token, query, variables).await?.0)
    }

    /// [`Github::query`], with the response's `X-OAuth-Scopes` header, if it has one.
    async fn query_with_scopes(
        &self,
        token: &str,
        query: &str,
        variables: Value,
    ) -> Result<(Value, Option<String>), GithubError> {
        let mut waits = RETRY_WAITS.iter();
        loop {
            match self.query_once(token, query, variables.clone()).await {
                Err(err) if err.may_pass() => match waits.next() {
                    Some(wait) => {
                        tracing::info!("GitHub call failed, trying again in {wait:?}: {err}");
                        tokio::time::sleep(*wait).await;
                    }
                    None => return Err(err),
                },
                result => return result,
            }
        }
    }

    /// Posts one GraphQL query once and returns its `data` and `X-OAuth-Scopes` header.
    async fn query_once(
        &self,
        token: &str,
        query: &str,
        variables: Value,
    ) -> Result<(Value, Option<String>), GithubError> {
        let response = self
            .http
            .post(&self.endpoint)
            .bearer_auth(token)
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(GithubError::Unauthorized);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(GithubError::Status {
                status: status.as_u16(),
                message: message(&body),
            });
        }
        let scopes = response
            .headers()
            .get("x-oauth-scopes")
            .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());
        Ok((data(response.json().await?)?, scopes))
    }
}

/// The scopes in an `X-OAuth-Scopes` header: comma-separated, possibly none.
fn parse_scopes(header: &str) -> Vec<String> {
    header
        .split(',')
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .map(str::to_owned)
        .collect()
}

impl GithubError {
    /// Whether trying again may succeed: a 5xx, or a request that failed in transit.
    fn may_pass(&self) -> bool {
        match self {
            GithubError::Status { status, .. } => *status >= 500,
            GithubError::Http(_) => true,
            GithubError::Unauthorized | GithubError::GraphQl(_) | GithubError::Shape(_) => false,
        }
    }
}

/// The `message` of a JSON error body, as GitHub's REST-style errors carry one.
fn message(body: &str) -> Option<String> {
    let body: Value = serde_json::from_str(body).ok()?;
    body.get("message")?.as_str().map(str::to_owned)
}

/// A GraphQL response body's `data`. Partial errors next to data are GitHub's way of saying
/// "not found", so only errors without data fail.
fn data(body: Value) -> Result<Value, GithubError> {
    match body.get("data") {
        Some(data) if !data.is_null() => Ok(data.clone()),
        _ => {
            let messages: Vec<&str> = body
                .get("errors")
                .and_then(Value::as_array)
                .map(|errors| {
                    errors
                        .iter()
                        .filter_map(|error| error.get("message").and_then(Value::as_str))
                        .collect()
                })
                .unwrap_or_default();
            Err(GithubError::GraphQl(if messages.is_empty() {
                "no data".into()
            } else {
                messages.join("; ")
            }))
        }
    }
}

/// One page of search results.
#[derive(Debug, PartialEq, Eq)]
pub struct SearchPage {
    /// The page's PRs.
    pub prs: Vec<RemotePr>,
    /// The cursor of the next page, if there is one.
    pub next: Option<String>,
    /// How many results the whole search has.
    pub total: u32,
}

/// One page of the list of open PRs.
#[derive(Debug, PartialEq, Eq)]
pub struct ListPage {
    /// The page's PRs, with light fields only.
    pub prs: Vec<RemotePr>,
    /// The cursor of the next page, if there is one.
    pub next: Option<String>,
    /// How many open PRs the whole list has.
    pub total: u32,
}

fn parse_list(data: Value) -> Result<ListPage, GithubError> {
    #[derive(Deserialize)]
    struct Data {
        viewer: Viewer,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Viewer {
        pull_requests: List,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct List {
        total_count: u32,
        page_info: PageInfo,
        nodes: Vec<LightNode>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct PageInfo {
        has_next_page: bool,
        end_cursor: Option<String>,
    }

    let list = serde_json::from_value::<Data>(data)?.viewer.pull_requests;
    Ok(ListPage {
        prs: list.nodes.into_iter().map(RemotePr::from).collect(),
        next: list
            .page_info
            .end_cursor
            .filter(|_| list.page_info.has_next_page),
        total: list.total_count,
    })
}

fn parse_search(data: Value) -> Result<SearchPage, GithubError> {
    #[derive(Deserialize)]
    struct Data {
        search: Search,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Search {
        #[serde(default)]
        issue_count: u32,
        page_info: PageInfo,
        nodes: Vec<SearchNode>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct PageInfo {
        has_next_page: bool,
        end_cursor: Option<String>,
    }
    /// The search is typed ISSUE, so a node that isn't a PR comes back as `{}`.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum SearchNode {
        Pr(PrNode),
        Other(serde::de::IgnoredAny),
    }

    let data: Data = serde_json::from_value(data)?;
    let prs = data
        .search
        .nodes
        .into_iter()
        .filter_map(|node| match node {
            SearchNode::Pr(pr) => Some(pr.into()),
            SearchNode::Other(_) => None,
        })
        .collect();
    let info = data.search.page_info;
    Ok(SearchPage {
        prs,
        next: info.end_cursor.filter(|_| info.has_next_page),
        total: data.search.issue_count,
    })
}

/// The query fetching `batch` by number: PR `i` as alias `p<i>`, its owner, name and number
/// in variables `o<i>`, `r<i>` and `n<i>`. A repo that isn't `owner/name` is asked for under
/// an empty owner, which GitHub doesn't show.
fn batch_query(batch: &[(String, i64)]) -> (String, Value) {
    let mut params = Vec::with_capacity(batch.len());
    let mut fields = Vec::with_capacity(batch.len());
    let mut variables = serde_json::Map::new();
    for (i, (repo, number)) in batch.iter().enumerate() {
        let (owner, name) = repo.split_once('/').unwrap_or(("", repo));
        params.push(format!("$o{i}: String!, $r{i}: String!, $n{i}: Int!"));
        fields.push(format!(
            "  p{i}: repository(owner: $o{i}, name: $r{i}) {{ pullRequest(number: $n{i}) {{ ...pr }} }}"
        ));
        variables.insert(format!("o{i}"), owner.into());
        variables.insert(format!("r{i}"), name.into());
        variables.insert(format!("n{i}"), (*number).into());
    }
    let query = format!(
        "query({}) {{\n{}\n}}\n{PR_FIELDS}",
        params.join(", "),
        fields.join("\n")
    );
    (query, Value::Object(variables))
}

/// The `len` PRs of a batch's `data`, in order; `None` for each GitHub doesn't show.
fn parse_batch(data: Value, len: usize) -> Result<Vec<Option<RemotePr>>, GithubError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Repository {
        pull_request: Option<PrNode>,
    }

    let Value::Object(mut data) = data else {
        return Err(GithubError::Shape(serde::de::Error::custom(
            "a batch's data isn't an object",
        )));
    };
    (0..len)
        .map(|i| {
            let repository: Option<Repository> =
                serde_json::from_value(data.remove(&format!("p{i}")).unwrap_or(Value::Null))?;
            Ok(repository
                .and_then(|repo| repo.pull_request)
                .map(RemotePr::from))
        })
        .collect()
}

/// The `light` fragment.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LightNode {
    number: i64,
    title: String,
    url: String,
    state: GqlPrState,
    is_draft: bool,
    head_ref_name: String,
    updated_at: String,
    repository: NameWithOwner,
}

impl From<LightNode> for RemotePr {
    fn from(node: LightNode) -> RemotePr {
        RemotePr {
            repo: node.repository.name_with_owner,
            number: node.number,
            title: node.title,
            url: node.url,
            state: node.state.into(),
            draft: node.is_draft,
            head_ref: node.head_ref_name,
            checks: None,
            review: None,
            updated_at: node.updated_at,
        }
    }
}

/// The `pr` fragment.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrNode {
    number: i64,
    title: String,
    url: String,
    state: GqlPrState,
    is_draft: bool,
    head_ref_name: String,
    updated_at: String,
    repository: NameWithOwner,
    review_decision: Option<GqlReview>,
    commits: Commits,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NameWithOwner {
    name_with_owner: String,
}

#[derive(Deserialize)]
struct Commits {
    nodes: Vec<CommitNode>,
}

#[derive(Deserialize)]
struct CommitNode {
    commit: Commit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Commit {
    status_check_rollup: Option<Rollup>,
}

#[derive(Deserialize)]
struct Rollup {
    state: GqlRollup,
}

#[derive(Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum GqlPrState {
    Open,
    Merged,
    Closed,
}

impl From<GqlPrState> for PrState {
    fn from(state: GqlPrState) -> PrState {
        match state {
            GqlPrState::Open => PrState::Open,
            GqlPrState::Merged => PrState::Merged,
            GqlPrState::Closed => PrState::Closed,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum GqlReview {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

#[derive(Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum GqlRollup {
    Success,
    Failure,
    Error,
    Pending,
    Expected,
}

impl From<PrNode> for RemotePr {
    fn from(node: PrNode) -> RemotePr {
        let checks = node
            .commits
            .nodes
            .into_iter()
            .last()
            .and_then(|node| node.commit.status_check_rollup)
            .map(|rollup| match rollup.state {
                GqlRollup::Success => ChecksState::Success,
                GqlRollup::Failure | GqlRollup::Error => ChecksState::Failure,
                GqlRollup::Pending | GqlRollup::Expected => ChecksState::Pending,
            });
        RemotePr {
            repo: node.repository.name_with_owner,
            number: node.number,
            title: node.title,
            url: node.url,
            state: node.state.into(),
            draft: node.is_draft,
            head_ref: node.head_ref_name,
            checks,
            review: node.review_decision.map(|review| match review {
                GqlReview::Approved => ReviewState::Approved,
                GqlReview::ChangesRequested => ReviewState::ChangesRequested,
                GqlReview::ReviewRequired => ReviewState::ReviewRequired,
            }),
            updated_at: node.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(state: &str, review: Value, rollup: Value) -> Value {
        json!({
            "number": 7,
            "title": "Add PR sync",
            "url": "https://github.com/acme/widgets/pull/7",
            "state": state,
            "isDraft": false,
            "headRefName": "feat/sync",
            "updatedAt": "2026-11-01T10:00:00Z",
            "repository": { "nameWithOwner": "acme/widgets" },
            "reviewDecision": review,
            "commits": { "nodes": [ { "commit": { "statusCheckRollup": rollup } } ] }
        })
    }

    fn search(nodes: Vec<Value>, next: Option<&str>) -> Value {
        json!({
            "search": {
                "pageInfo": { "hasNextPage": next.is_some(), "endCursor": next.unwrap_or("end") },
                "nodes": nodes
            }
        })
    }

    mod parse_scopes {
        use super::*;

        #[test]
        fn should_split_the_header_on_commas() {
            assert_eq!(parse_scopes("repo, read:org"), ["repo", "read:org"]);
        }

        #[test]
        fn should_read_an_empty_header_as_no_scopes() {
            assert!(parse_scopes("").is_empty());
            assert!(parse_scopes(" ").is_empty());
        }
    }

    mod parse_search {
        use super::*;

        #[test]
        fn should_read_every_field_of_a_pr() {
            let page = parse_search(search(
                vec![node(
                    "OPEN",
                    json!("APPROVED"),
                    json!({ "state": "SUCCESS" }),
                )],
                None,
            ))
            .unwrap();

            assert_eq!(
                page.prs,
                vec![RemotePr {
                    repo: "acme/widgets".into(),
                    number: 7,
                    title: "Add PR sync".into(),
                    url: "https://github.com/acme/widgets/pull/7".into(),
                    state: PrState::Open,
                    draft: false,
                    head_ref: "feat/sync".into(),
                    checks: Some(ChecksState::Success),
                    review: Some(ReviewState::Approved),
                    updated_at: "2026-11-01T10:00:00Z".into(),
                }]
            );
        }

        #[test]
        fn should_read_how_many_results_the_whole_search_has() {
            let mut data = search(vec![], Some("abc"));
            data["search"]["issueCount"] = json!(271);

            assert_eq!(parse_search(data).unwrap().total, 271);
        }

        #[test]
        fn should_skip_a_node_that_is_not_a_pr() {
            let page = parse_search(search(vec![json!({})], None)).unwrap();
            assert!(page.prs.is_empty());
        }

        #[test]
        fn should_give_the_next_cursor_only_when_there_is_a_next_page() {
            let more = parse_search(search(vec![], Some("abc"))).unwrap();
            let last = parse_search(search(vec![], None)).unwrap();

            assert_eq!(more.next.as_deref(), Some("abc"));
            assert_eq!(last.next, None);
        }
    }

    mod checks {
        use super::*;

        fn checks(rollup: Value) -> Option<ChecksState> {
            let page = parse_search(search(vec![node("OPEN", Value::Null, rollup)], None));
            page.unwrap().prs[0].checks
        }

        #[test]
        fn should_fold_error_into_failure() {
            assert_eq!(
                checks(json!({ "state": "ERROR" })),
                Some(ChecksState::Failure)
            );
            assert_eq!(
                checks(json!({ "state": "FAILURE" })),
                Some(ChecksState::Failure)
            );
        }

        #[test]
        fn should_fold_expected_into_pending() {
            assert_eq!(
                checks(json!({ "state": "EXPECTED" })),
                Some(ChecksState::Pending)
            );
            assert_eq!(
                checks(json!({ "state": "PENDING" })),
                Some(ChecksState::Pending)
            );
        }

        #[test]
        fn should_be_none_for_a_commit_without_checks() {
            assert_eq!(checks(Value::Null), None);
        }
    }

    mod review {
        use super::*;

        #[test]
        fn should_map_each_review_decision() {
            let review = |decision: Value| {
                parse_search(search(vec![node("OPEN", decision, Value::Null)], None))
                    .unwrap()
                    .prs[0]
                    .review
            };

            assert_eq!(
                review(json!("CHANGES_REQUESTED")),
                Some(ReviewState::ChangesRequested)
            );
            assert_eq!(
                review(json!("REVIEW_REQUIRED")),
                Some(ReviewState::ReviewRequired)
            );
            assert_eq!(review(Value::Null), None);
        }
    }

    mod parse_list {
        use super::*;

        #[test]
        fn should_read_light_fields_without_review_or_checks() {
            let mut light = node("OPEN", json!("APPROVED"), json!({ "state": "SUCCESS" }));
            light.as_object_mut().unwrap().remove("reviewDecision");
            light.as_object_mut().unwrap().remove("commits");
            let data = json!({ "viewer": { "pullRequests": {
                "totalCount": 271,
                "pageInfo": { "hasNextPage": true, "endCursor": "c1" },
                "nodes": [ light ]
            } } });

            let page = parse_list(data).unwrap();

            assert_eq!(page.total, 271);
            assert_eq!(page.next.as_deref(), Some("c1"));
            assert_eq!(page.prs.len(), 1);
            let pr = &page.prs[0];
            assert_eq!((pr.repo.as_str(), pr.number), ("acme/widgets", 7));
            assert_eq!((pr.checks, pr.review), (None, None));
        }
    }

    mod parse_batch {
        use super::*;

        #[test]
        fn should_read_each_pr_in_order() {
            let data = json!({
                "p0": { "pullRequest": node("MERGED", Value::Null, Value::Null) },
                "p1": { "pullRequest": node("OPEN", Value::Null, Value::Null) }
            });

            let prs = parse_batch(data, 2).unwrap();

            let states: Vec<_> = prs
                .iter()
                .map(|pr| pr.as_ref().map(|pr| pr.state))
                .collect();
            assert_eq!(states, [Some(PrState::Merged), Some(PrState::Open)]);
        }

        #[test]
        fn should_be_none_where_the_repo_or_the_pr_is_gone() {
            let data = json!({ "p0": null, "p1": { "pullRequest": null } });

            assert_eq!(parse_batch(data, 3).unwrap(), [None, None, None]);
        }
    }

    mod batch_query {
        use super::*;

        #[test]
        fn should_ask_for_each_pr_under_its_own_alias_and_variables() {
            let (query, variables) =
                batch_query(&[("acme/widgets".into(), 7), ("acme/gadgets".into(), 9)]);

            assert!(query.contains(
                "p1: repository(owner: $o1, name: $r1) { pullRequest(number: $n1) { ...pr } }"
            ));
            assert_eq!(
                variables,
                json!({
                    "o0": "acme", "r0": "widgets", "n0": 7,
                    "o1": "acme", "r1": "gadgets", "n1": 9
                })
            );
        }
    }

    mod data {
        use super::*;

        #[test]
        fn should_fail_with_the_messages_when_there_is_no_data() {
            let err = data(json!({
                "data": null,
                "errors": [ { "message": "Bad credentials" }, { "message": "Try again" } ]
            }))
            .unwrap_err();

            assert_eq!(
                err.to_string(),
                "GitHub GraphQL errors: Bad credentials; Try again"
            );
        }

        #[test]
        fn should_keep_data_next_to_partial_errors() {
            let data = data(json!({
                "data": { "repository": null },
                "errors": [ { "message": "Could not resolve to a Repository" } ]
            }))
            .unwrap();

            assert_eq!(data, json!({ "repository": null }));
        }
    }
}
