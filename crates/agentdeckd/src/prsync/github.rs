//! GitHub's GraphQL API (spec §8): the viewer's login and scopes, the list of the viewer's
//! open PRs with light fields, the search for PRs updated since a time, PRs by number with
//! every field, in batches, and closing a PR: its node id by number, in batches, then one
//! `closePullRequest` each.

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

/// How long one request may take before the client gives up on it, unless
/// [`super::TIMEOUT_VAR`] says otherwise.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// The waits before each retry of a query that failed for a reason that may pass: a 5xx, a
/// failed connection, a timeout, a body cut short. One try, then one more per wait. A
/// mutation is retried only when its connection failed, so it never reached GitHub.
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

/// Only the node id, which `closePullRequest` takes.
const NODE_ID_FIELDS: &str = "fragment nodeId on PullRequest { id }";

/// Closes one PR by node id; GitHub has no batch close.
const CLOSE_MUTATION: &str = "mutation($id: ID!) {
  closePullRequest(input: { pullRequestId: $id }) { pullRequest { id state } }
}";

/// One PR's state by node id: whether a close GitHub didn't answer took effect.
const STATE_QUERY: &str = "query($id: ID!) { node(id: $id) { ... on PullRequest { state } } }";

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
    /// A close failed in a way that may have reached GitHub, and reading the PR's state after
    /// failed too, so whether it closed is unknown.
    #[error("GitHub didn't answer; check the PR")]
    NoAnswer,
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
    /// The request failed, or the response wasn't the JSON asked for. Its message says why:
    /// a timeout, a failed connection and their causes.
    #[error("{}", transit(.0))]
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
    /// A client for the GraphQL API at `endpoint` whose requests give up after `timeout`
    /// ([`TIMEOUT`] unless a test says otherwise).
    pub fn new(endpoint: String, timeout: Duration) -> anyhow::Result<Github> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("agentdeck/", env!("CARGO_PKG_VERSION")))
            .timeout(timeout)
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
        let (query, variables) = batch_query(&batch, "pr", PR_FIELDS);
        let data = self.query(token, &query, variables).await?;
        parse_batch(data, batch.len())
    }

    /// The node id of each of `wanted` (`(repo, number)`), in order, looked up as fetches by
    /// number go: `BATCH_SIZE` to a request, `BATCHES_IN_FLIGHT` at once. A failure is each
    /// PR's own: GitHub's message for one it doesn't show, or why its batch's request failed;
    /// only GitHub refusing the token fails the whole lookup.
    pub async fn node_ids(
        &self,
        token: &str,
        wanted: &[(String, i64)],
    ) -> Result<Vec<Result<String, String>>, GithubError> {
        use futures_util::{StreamExt as _, TryStreamExt as _};

        let batches: Vec<Vec<(String, i64)>> =
            wanted.chunks(BATCH_SIZE).map(<[_]>::to_vec).collect();
        let lookups = batches
            .into_iter()
            .map(|batch| self.node_id_batch(token, batch));
        let found: Vec<Vec<Result<String, String>>> = futures_util::stream::iter(lookups)
            .buffered(BATCHES_IN_FLIGHT)
            .try_collect()
            .await?;
        Ok(found.into_iter().flatten().collect())
    }

    /// One request for `batch`'s node ids; an error only when GitHub refuses the token.
    async fn node_id_batch(
        &self,
        token: &str,
        batch: Vec<(String, i64)>,
    ) -> Result<Vec<Result<String, String>>, GithubError> {
        let (query, variables) = batch_query(&batch, "nodeId", NODE_ID_FIELDS);
        match self.query_body(token, &query, variables).await {
            Ok((body, _)) => Ok(parse_node_ids(&body, batch.len())),
            Err(GithubError::Unauthorized) => Err(GithubError::Unauthorized),
            Err(err) => Ok(vec![Err(err.to_string()); batch.len()]),
        }
    }

    /// Closes the PR with node id `id`. A refusal (no write access, say) is GitHub's message.
    /// The mutation is sent again only after a failed connection: after a timeout or a 5xx it
    /// may have taken effect, so the PR's state is read once instead (spec §8).
    pub async fn close(&self, token: &str, id: &str) -> Result<(), GithubError> {
        let variables = json!({ "id": id });
        let body = match self
            .post_retrying(token, CLOSE_MUTATION, &variables, GithubError::not_sent)
            .await
        {
            Ok((body, _)) => body,
            Err(err) if err.may_pass() && !err.not_sent() => {
                return self.closed_after(token, &variables, err).await;
            }
            Err(err) => return Err(err),
        };
        let closed = body
            .pointer("/data/closePullRequest/pullRequest")
            .is_some_and(|pr| !pr.is_null());
        if closed {
            Ok(())
        } else {
            Err(GithubError::GraphQl(
                error_messages(&body, |_| true)
                    .unwrap_or_else(|| "GitHub didn't close the PR".into()),
            ))
        }
    }

    /// After a close that failed with `err` but may have reached GitHub: `Ok` when the PR
    /// reads closed, `err` when it doesn't, [`GithubError::NoAnswer`] when the read fails. One
    /// read, not retried, so a GitHub that stopped answering costs one more timeout at most.
    async fn closed_after(
        &self,
        token: &str,
        variables: &Value,
        err: GithubError,
    ) -> Result<(), GithubError> {
        tracing::info!("GitHub close failed, reading the PR's state: {err}");
        match self.query_once(token, STATE_QUERY, variables).await {
            Ok((body, _)) if body.pointer("/data/node/state") == Some(&json!("CLOSED")) => Ok(()),
            Ok(_) => Err(err),
            Err(read) => {
                tracing::warn!("reading the PR's state after a failed close: {read}");
                Err(GithubError::NoAnswer)
            }
        }
    }

    /// Posts one GraphQL query and returns its `data`, as [`Github::query_body`] does.
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
        let (body, scopes) = self.query_body(token, query, variables).await?;
        Ok((data(body)?, scopes))
    }

    /// Posts one GraphQL query and returns the whole response body, `data` and `errors`,
    /// with its `X-OAuth-Scopes` header, trying again after each of `RETRY_WAITS` while it
    /// fails for a reason that may pass.
    async fn query_body(
        &self,
        token: &str,
        query: &str,
        variables: Value,
    ) -> Result<(Value, Option<String>), GithubError> {
        self.post_retrying(token, query, &variables, GithubError::may_pass)
            .await
    }

    /// Posts `query`, trying again after each of `RETRY_WAITS` while it fails with an error
    /// `retry` accepts.
    async fn post_retrying(
        &self,
        token: &str,
        query: &str,
        variables: &Value,
        retry: fn(&GithubError) -> bool,
    ) -> Result<(Value, Option<String>), GithubError> {
        let mut waits = RETRY_WAITS.iter();
        loop {
            match self.query_once(token, query, variables).await {
                Err(err) if retry(&err) => match waits.next() {
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

    /// Posts one GraphQL query once and returns the response body and its `X-OAuth-Scopes`
    /// header.
    async fn query_once(
        &self,
        token: &str,
        query: &str,
        variables: &Value,
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
        Ok((response.json().await?, scopes))
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
            GithubError::Unauthorized
            | GithubError::NoAnswer
            | GithubError::GraphQl(_)
            | GithubError::Shape(_) => false,
        }
    }

    /// Whether the request surely never reached GitHub: its connection failed (refused, or
    /// the host not found), so sending it again can't act twice.
    fn not_sent(&self) -> bool {
        matches!(self, GithubError::Http(err) if err.is_connect())
    }
}

/// What kind of failure a request met on its way to GitHub or back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transit {
    /// GitHub didn't answer within the client's timeout.
    Timeout,
    /// No connection: refused, the host not found, a failed TLS handshake.
    Connect,
    /// The response arrived but its body couldn't be read or wasn't the JSON asked for.
    Response,
    /// Anything else, such as a request reqwest couldn't build.
    Other,
}

impl Transit {
    fn of(err: &reqwest::Error) -> Transit {
        if err.is_timeout() {
            Transit::Timeout
        } else if err.is_connect() {
            Transit::Connect
        } else if err.is_body() || err.is_decode() {
            Transit::Response
        } else {
            Transit::Other
        }
    }

    fn words(self) -> &'static str {
        match self {
            Transit::Timeout => "GitHub didn't answer in time",
            Transit::Connect => "couldn't connect to GitHub",
            Transit::Response => "couldn't read GitHub's response",
            Transit::Other => "calling GitHub failed",
        }
    }
}

/// How hyper wraps every failed connection; [`Transit::Connect`]'s words already say it.
const HYPER_CONNECT: &str = "client error (Connect)";

/// Why a request failed in transit (spec §8): its kind, then the causes reqwest wraps, such
/// as `operation timed out` or `Connection refused (os error 111)`. reqwest's own words
/// (`error sending request for url (…)`) say nothing the kind doesn't. The token travels only
/// in a header, so no cause carries it.
fn transit(err: &reqwest::Error) -> String {
    let mut causes = Vec::new();
    let mut cause = std::error::Error::source(err);
    while let Some(err) = cause {
        causes.push(err.to_string());
        cause = err.source();
    }
    describe(Transit::of(err), err.url(), &causes)
}

/// `kind`'s words, the endpoint `url` without credentials, query or fragment, then each of
/// `causes` (outermost first) but hyper's connect wrapper and a repeat of the one before.
fn describe(kind: Transit, url: Option<&reqwest::Url>, causes: &[String]) -> String {
    let mut message = kind.words().to_owned();
    if let Some(url) = url {
        let mut url = url.clone();
        // Neither fails on an http(s) URL, the only kind a request has.
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        message.push_str(&format!(" ({url})"));
    }
    let mut last: Option<&str> = None;
    for cause in causes {
        if cause == HYPER_CONNECT || last == Some(cause.as_str()) {
            continue;
        }
        message.push_str(": ");
        message.push_str(cause);
        last = Some(cause);
    }
    message
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
        _ => Err(GithubError::GraphQl(
            error_messages(&body, |_| true).unwrap_or_else(|| "no data".into()),
        )),
    }
}

/// The messages of a response body's `errors` whose `path` passes `on_path` (`None` for an
/// error with no path), joined; `None` when there are none.
fn error_messages(body: &Value, on_path: impl Fn(Option<&Value>) -> bool) -> Option<String> {
    let messages: Vec<&str> = body
        .get("errors")
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter(|error| on_path(error.get("path")))
                .filter_map(|error| error.get("message").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    (!messages.is_empty()).then(|| messages.join("; "))
}

/// The `len` node ids of a lookup's response body, in order. One GitHub doesn't show fails
/// with the message of the error on its alias `p<i>`.
fn parse_node_ids(body: &Value, len: usize) -> Vec<Result<String, String>> {
    (0..len)
        .map(|i| {
            let alias = format!("p{i}");
            body.pointer(&format!("/data/{alias}/pullRequest/id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    error_messages(body, |path| {
                        path.and_then(|path| path.get(0)).and_then(Value::as_str)
                            == Some(alias.as_str())
                    })
                    .unwrap_or_else(|| "GitHub doesn't show this PR".into())
                })
        })
        .collect()
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

/// The query fetching `batch` by number with fragment `fragment` (named `spread`): PR `i` as
/// alias `p<i>`, its owner, name and number in variables `o<i>`, `r<i>` and `n<i>`. A repo
/// that isn't `owner/name` is asked for under an empty owner, which GitHub doesn't show.
fn batch_query(batch: &[(String, i64)], spread: &str, fragment: &str) -> (String, Value) {
    let mut params = Vec::with_capacity(batch.len());
    let mut fields = Vec::with_capacity(batch.len());
    let mut variables = serde_json::Map::new();
    for (i, (repo, number)) in batch.iter().enumerate() {
        let (owner, name) = repo.split_once('/').unwrap_or(("", repo));
        params.push(format!("$o{i}: String!, $r{i}: String!, $n{i}: Int!"));
        fields.push(format!(
            "  p{i}: repository(owner: $o{i}, name: $r{i}) {{ pullRequest(number: $n{i}) {{ ...{spread} }} }}"
        ));
        variables.insert(format!("o{i}"), owner.into());
        variables.insert(format!("r{i}"), name.into());
        variables.insert(format!("n{i}"), (*number).into());
    }
    let query = format!(
        "query({}) {{\n{}\n}}\n{fragment}",
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

    mod transit {
        use super::*;

        fn endpoint() -> reqwest::Url {
            reqwest::Url::parse(DEFAULT_ENDPOINT).expect("a URL")
        }

        fn causes(causes: &[&str]) -> Vec<String> {
            causes.iter().map(|cause| (*cause).to_owned()).collect()
        }

        #[test]
        fn should_say_github_did_not_answer_in_time_and_why() {
            let message = describe(
                Transit::Timeout,
                Some(&endpoint()),
                &causes(&["operation timed out"]),
            );

            assert_eq!(
                message,
                "GitHub didn't answer in time (https://api.github.com/graphql): operation timed out"
            );
        }

        #[test]
        fn should_say_it_could_not_connect_with_each_cause_but_hypers_wrapper() {
            let message = describe(
                Transit::Connect,
                Some(&endpoint()),
                &causes(&[
                    "client error (Connect)",
                    "tcp connect error",
                    "Connection refused (os error 111)",
                ]),
            );

            assert_eq!(
                message,
                "couldn't connect to GitHub (https://api.github.com/graphql): tcp connect error: \
                 Connection refused (os error 111)"
            );
        }

        #[test]
        fn should_name_any_other_failure_with_its_causes() {
            let message = describe(
                Transit::Other,
                Some(&endpoint()),
                &causes(&["connection closed before message completed"]),
            );

            assert_eq!(
                message,
                "calling GitHub failed (https://api.github.com/graphql): \
                 connection closed before message completed"
            );
        }

        #[test]
        fn should_say_the_response_could_not_be_read() {
            let message = describe(
                Transit::Response,
                Some(&endpoint()),
                &causes(&["expected ident at line 1 column 2"]),
            );

            assert_eq!(
                message,
                "couldn't read GitHub's response (https://api.github.com/graphql): \
                 expected ident at line 1 column 2"
            );
        }

        #[test]
        fn should_leave_out_the_urls_credentials_query_and_fragment() {
            let url = reqwest::Url::parse(
                "https://octocat:gho_secret@ghe.example.com:8443/api/graphql?access_token=gho_x#f",
            )
            .expect("a URL");

            let message = describe(Transit::Timeout, Some(&url), &[]);

            assert_eq!(
                message,
                "GitHub didn't answer in time (https://ghe.example.com:8443/api/graphql)"
            );
        }

        #[test]
        fn should_say_a_cause_repeated_back_to_back_once() {
            let message = describe(
                Transit::Other,
                None,
                &causes(&["connection closed", "connection closed"]),
            );

            assert_eq!(message, "calling GitHub failed: connection closed");
        }

        #[tokio::test]
        async fn should_name_the_kind_and_cause_of_a_request_reqwest_could_not_build() {
            // No I/O: reqwest refuses the URL before sending anything.
            let github = Github::new("not a url".into(), TIMEOUT).expect("a client");

            let err = github.viewer("gho_test").await.unwrap_err();

            assert_eq!(
                err.to_string(),
                "calling GitHub failed: relative URL without a base"
            );
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
            let (query, variables) = batch_query(
                &[("acme/widgets".into(), 7), ("acme/gadgets".into(), 9)],
                "pr",
                PR_FIELDS,
            );

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

    mod parse_node_ids {
        use super::*;

        #[test]
        fn should_read_each_node_id_in_order() {
            let body = json!({ "data": {
                "p0": { "pullRequest": { "id": "PR_a" } },
                "p1": { "pullRequest": { "id": "PR_b" } }
            } });

            assert_eq!(
                parse_node_ids(&body, 2),
                [Ok("PR_a".to_owned()), Ok("PR_b".to_owned())]
            );
        }

        #[test]
        fn should_fail_a_missing_pr_with_the_error_on_its_alias() {
            let body = json!({
                "data": { "p0": { "pullRequest": { "id": "PR_a" } }, "p1": { "pullRequest": null } },
                "errors": [ { "path": [ "p1", "pullRequest" ], "message": "Could not resolve" } ]
            });

            assert_eq!(
                parse_node_ids(&body, 2),
                [Ok("PR_a".to_owned()), Err("Could not resolve".to_owned())]
            );
        }

        #[test]
        fn should_say_github_does_not_show_a_missing_pr_without_an_error() {
            let body = json!({ "data": { "p0": null } });

            assert_eq!(
                parse_node_ids(&body, 1),
                [Err("GitHub doesn't show this PR".to_owned())]
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

/// [`Github`] against httpmock as GitHub, over real HTTP.
#[cfg(test)]
mod integration_tests {
    use super::*;
    use httpmock::prelude::*;
    use httpmock::{Mock, MockServer};
    use std::time::Instant;

    const TOKEN: &str = "gho_test";
    const ID: &str = "PR_kwDOtest7";
    /// Short, so a request that outlasts it fails fast.
    const SHORT: Duration = Duration::from_millis(500);
    /// Longer than [`SHORT`]: how long a slow GitHub holds a request.
    const SLOW: Duration = Duration::from_secs(3);

    fn github(server: &MockServer) -> Github {
        Github::new(server.url("/graphql"), SHORT).expect("a client")
    }

    /// The close mutation, answered `then` after `delay`.
    fn mock_close<'a>(server: &'a MockServer, delay: Duration, status: u16) -> Mock<'a> {
        server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("closePullRequest(");
            then.status(status).delay(delay).json_body(json!({
                "data": { "closePullRequest": { "pullRequest": { "id": ID, "state": "CLOSED" } } }
            }));
        })
    }

    /// The PR's state by node id, answered with `state`.
    fn mock_state<'a>(server: &'a MockServer, state: &str) -> Mock<'a> {
        server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("... on PullRequest { state }")
                .json_body_includes(json!({ "variables": { "id": ID } }).to_string());
            then.status(200)
                .json_body(json!({ "data": { "node": { "state": state } } }));
        })
    }

    mod close {
        use super::*;

        #[tokio::test]
        async fn should_send_a_close_that_outlasts_the_timeout_only_once() {
            let server = MockServer::start_async().await;
            let close = mock_close(&server, SLOW, 200);
            mock_state(&server, "OPEN");

            let result = github(&server).close(TOKEN, ID).await;

            assert!(result.is_err(), "{result:?}");
            close.assert_calls(1);
        }

        #[tokio::test]
        async fn should_succeed_when_the_pr_reads_closed_after_a_timed_out_close() {
            let server = MockServer::start_async().await;
            mock_close(&server, SLOW, 200);
            let state = mock_state(&server, "CLOSED");

            let result = github(&server).close(TOKEN, ID).await;

            assert!(result.is_ok(), "{result:?}");
            state.assert_calls(1);
        }

        #[tokio::test]
        async fn should_fail_with_why_when_the_pr_still_reads_open_after_a_timed_out_close() {
            let server = MockServer::start_async().await;
            mock_close(&server, SLOW, 200);
            mock_state(&server, "OPEN");

            let err = github(&server).close(TOKEN, ID).await.unwrap_err();

            assert!(
                matches!(&err, GithubError::Http(err) if err.is_timeout()),
                "{err:?}"
            );
            assert_eq!(
                err.to_string(),
                format!(
                    "GitHub didn't answer in time ({}): operation timed out",
                    server.url("/graphql")
                )
            );
        }

        #[tokio::test]
        async fn should_say_github_did_not_answer_when_the_state_read_fails_too() {
            let server = MockServer::start_async().await;
            mock_close(&server, SLOW, 200);
            let state = server.mock(|when, then| {
                when.method(POST)
                    .path("/graphql")
                    .body_includes("... on PullRequest { state }");
                then.status(502);
            });

            let err = github(&server).close(TOKEN, ID).await.unwrap_err();

            assert_eq!(err.to_string(), "GitHub didn't answer; check the PR");
            state.assert_calls(1);
        }

        #[tokio::test]
        async fn should_not_send_a_close_github_answers_502_again() {
            let server = MockServer::start_async().await;
            let close = mock_close(&server, Duration::ZERO, 502);
            mock_state(&server, "CLOSED");

            let result = github(&server).close(TOKEN, ID).await;

            assert!(result.is_ok(), "{result:?}");
            close.assert_calls(1);
        }

        #[tokio::test]
        async fn should_try_a_close_whose_connection_is_refused_three_times() {
            // A port nothing listens on: the connection is refused before anything is sent.
            let port = std::net::TcpListener::bind("127.0.0.1:0")
                .and_then(|listener| listener.local_addr())
                .expect("a free port")
                .port();
            let github =
                Github::new(format!("http://127.0.0.1:{port}/graphql"), SHORT).expect("a client");
            let started = Instant::now();

            let err = github.close(TOKEN, ID).await.unwrap_err();

            assert!(
                matches!(&err, GithubError::Http(err) if err.is_connect()),
                "{err:?}"
            );
            let message = err.to_string();
            assert!(
                message.starts_with(&format!(
                    "couldn't connect to GitHub (http://127.0.0.1:{port}/graphql): tcp connect error: "
                )) && message.contains("refused"),
                "{message}"
            );
            // Each refusal is at once, so the time taken is the waits between the tries.
            assert!(
                started.elapsed() >= RETRY_WAITS.iter().sum::<Duration>(),
                "{:?}",
                started.elapsed()
            );
        }
    }

    mod queries {
        use super::*;

        #[tokio::test]
        async fn should_try_a_query_that_outlasts_the_timeout_three_times() {
            let server = MockServer::start_async().await;
            let viewer = server.mock(|when, then| {
                when.method(POST)
                    .path("/graphql")
                    .body_includes("viewer { login }");
                then.status(200)
                    .delay(SLOW)
                    .json_body(json!({ "data": { "viewer": { "login": "octocat" } } }));
            });

            let result = github(&server).viewer(TOKEN).await;

            assert!(
                matches!(&result, Err(GithubError::Http(err)) if err.is_timeout()),
                "{result:?}"
            );
            viewer.assert_calls(3);
        }
    }
}
