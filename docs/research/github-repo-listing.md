# Listing a user's GitHub repositories for Link repository

Research for [#345](https://github.com/bRRRITSCOLD/agentdeck/issues/345), feeding the decision in [#350](https://github.com/bRRRITSCOLD/agentdeck/issues/350). This file records facts only, not decisions. The GitHub docs were read on 2026-11-21. The "Observed" lines come from read-only calls made that day with the `gh` CLI's OAuth token (scopes `gist, read:org, repo, workflow`) for an account that can reach 1,104 repositories.

## Short answers

1. **Endpoints:**
   - REST `GET /user/repos` lists every repository the token's user has explicit access to: owned, collaborator and organisation member. It filters with `affiliation` and `visibility`.
   - GraphQL `viewer { repositories(ownerAffiliations: …, affiliations: …) }` lists the same set.
   - Per-owner lists are `GET /orgs/{org}/repos` and `GET /users/{username}/repos`.
   - A single repository is `GET /repos/{owner}/{repo}`.
2. **Tokens:**
   - **Classic PAT with `repo`:** every repository the user can reach across all owners. Without `repo` it sees only public repositories.
   - **`gh` token:** an OAuth-app token with at least `repo, read:org, gist`. It behaves like a classic token, but an organisation with OAuth app access restrictions hides its private repositories until it approves the GitHub CLI app.
   - **Fine-grained PAT:** one resource owner only, a user or one organisation, optionally narrowed to selected repositories. It misses outside-collaborator repositories and every other organisation.
   - **SAML SSO:** an organisation that enforces it drops out of a classic token's results until the token is authorised for it, and the response flags `X-GitHub-SSO: partial-results`.
3. **Pagination and rate limits:**
   - Both APIs page at most 100 per request. 1,104 repositories took 12 requests, about 9–10 s sequentially, at 1 GraphQL point or 1 REST call per page.
   - That is small against the 5,000/hour budgets.
   - A typed search (`GET /search/repositories`) has its own limit of 30 requests per minute and returns at most 1,000 results. It also can't say "repositories I can access" without `user:`/`org:` qualifiers.
   - Listing once, caching the list and filtering locally costs far less than a search per keystroke.
4. **PR sync:**
   - The sync never uses `involves:@me`. A full sync lists `viewer.pullRequests(states: OPEN)`. An incremental sync searches `is:pr author:@me updated:>=T`.
   - Both already fetch `repository { nameWithOwner }` and store it in `pull_requests.repo`, so the repositories of synced PRs cost no new API call: they are `SELECT DISTINCT repo FROM pull_requests`.
   - That set covers only repositories where the user **authored** a PR in the sync's window (open PRs plus those updated within `max_age_days`).
   - Observed: 270 repositories for 271 open PRs, and 304 across all 1,099 authored PRs ever, against 1,104 reachable repositories.

## 1. Endpoints that list a user's repositories

### REST

- **`GET /user/repos`** lists "repositories that the authenticated user has explicit permission (:read, :write, or :admin) to access", which means "repositories they own, repositories where they are a collaborator, and repositories that they can access through an organization membership" ([REST repos](https://docs.github.com/en/rest/repos/repos#list-repositories-for-the-authenticated-user)). Its parameters:
  - `affiliation`: comma-separated `owner`, `collaborator` and `organization_member`, all three by default. `organization_member` "includes every repository on every team that the user is on".
  - `visibility`: `all`, `public` or `private`.
  - `type`: `all`, `owner`, `public`, `private` or `member`. It "will cause a 422 error if used in the same request as visibility or affiliation".
  - `sort`: `created`, `updated`, `pushed` or `full_name` (the default). `direction` sorts `asc` or `desc`.
  - `per_page`: up to 100, default 30. `page` picks the page.
  - `since` and `before`: "Only show repositories updated after/before the given time".
  - Responses: `304 Not Modified` is among them, so conditional requests work.
- **`GET /orgs/{org}/repos`** lists one organisation's repositories. `type` is `all`, `public`, `private`, `forks`, `sources` or `member`. It has no `since` ([same page](https://docs.github.com/en/rest/repos/repos#list-organization-repositories)).
- **`GET /users/{username}/repos`** "Lists public repositories for the specified user" ([same page](https://docs.github.com/en/rest/repos/repos#list-repositories-for-a-user)).
- **`GET /repos/{owner}/{repo}`** fetches one repository, for a typed `owner/name` or URL.
- **Observed:** `GET /user/repos?per_page=100` answered `X-Accepted-Oauth-Scopes:` (empty), so the endpoint names no required scope. The `Link` header ran to `rel="last"` = page 12. The pages held 1,104 repositories: 691 organisation-owned and 413 user-owned.

### GraphQL

Introspection of the live schema (`__type(name: "User")`) shows:

- **`repositories`:** "A list of repositories that the user owns". Its arguments:
  - `ownerAffiliations`: defaults to `[OWNER, COLLABORATOR]`.
  - `affiliations`: "viewer's affiliation options".
  - Filters: `privacy`, `visibility`, `isFork`, `isArchived`, `isLocked` and `hasIssuesEnabled`.
  - `orderBy`: `CREATED_AT`, `UPDATED_AT`, `PUSHED_AT`, `NAME` or `STARGAZERS`.
  - Paging: `first`/`last`/`after`/`before`.
- **`RepositoryAffiliation`** values:
  - `OWNER`;
  - `COLLABORATOR`: "added to as a collaborator";
  - `ORGANIZATION_MEMBER`: "access to through being a member of an organization. This includes every repository on every team that the user is on".
- **`repositoriesContributedTo`:** "repositories that the user recently contributed to". It takes `includeUserRepositories` and `contributionTypes`. Observed: 14, so it is far from complete.
- **`topRepositories`:** "Repositories the user has contributed to, ordered by contribution rank".
- **Observed:** for `viewer`, the affiliation arguments don't split as their descriptions suggest.

  | Query | Count |
  | --- | --- |
  | No arguments | 1,104 |
  | `[OWNER, COLLABORATOR]` | 1,104 |
  | `[OWNER, COLLABORATOR, ORGANIZATION_MEMBER]` | 1,104 |
  | `ownerAffiliations: [OWNER]` | 413 |
  | `affiliations: [COLLABORATOR]` | 691 |
  | `ownerAffiliations: [ORGANIZATION_MEMBER]` | 0 |
  | `affiliations: [ORGANIZATION_MEMBER]` | 0 |

  On this account the organisation repositories came back under `COLLABORATOR`, not `ORGANIZATION_MEMBER`. The REST and GraphQL totals agree (1,104).

## 2. What each kind of token returns

### Personal access token (classic)

- **Scopes:** `repo` "Grants full access to public and private repositories". With no scope a token gets "read-only access to public information (including … repository info …)" ([OAuth scopes](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/scopes-for-oauth-apps)). `read:org` is "Read-only access to organization membership, organization projects, and team membership". No doc says `/user/repos` needs it, and the endpoint's accepted-scopes header is empty (observed).
- **Reach:** a classic token "will grant access to all repositories within the organizations that you have access to, as well as all personal repositories in your personal account". Outside collaborators "can only use personal access tokens (classic)" to reach the organisation repositories they collaborate on ([Managing PATs](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens)).
- **Organisation policy:** "Organization owners can set a policy to restrict the access of personal access tokens (classic) to their organization" ([same page](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens)).
- **SAML SSO:** for a token not yet authorised and used across several organisations, "the API will not return results from the organizations that require SAML SSO and the `X-GitHub-SSO` header will indicate the ID of the organizations … For example: `X-GitHub-SSO: partial-results; organizations=21955855,20582480`" ([Authenticating to the REST API](https://docs.github.com/en/rest/authentication/authenticating-to-the-rest-api)).

### Fine-grained personal access token

- **Permission:** `GET /user/repos`, `GET /orgs/{org}/repos`, `GET /users/{username}/repos` and `GET /repos/{owner}/{repo}` all sit under the Repository permission **Metadata (read)** ([Permissions required for fine-grained PATs](https://docs.github.com/en/rest/authentication/permissions-required-for-fine-grained-personal-access-tokens)).
- **Scope of a token:** "Each token is limited to access resources owned by a single user or organization", and it "can be further limited to only access specific repositories". "Tokens always include read-only access to all public repositories on GitHub" ([Managing PATs](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens)).
- **Documented gaps** ([same page](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens)):
  - repositories "where the user is an outside or repository collaborator";
  - "access multiple organizations at once".

  The [GA changelog (2025-03-18)](https://github.blog/changelog/2025-03-18-fine-grained-pats-are-now-generally-available/) still lists the outside-collaborator case, and the current docs do too.
- **Organisation approval:** a token pending approval from the organisation "will only be able to read public resources until it is approved" ([same page](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens)).
- **What is missing:** every repository outside the chosen resource owner, collaborator repositories and other organisations. Not tested here, because no fine-grained token was at hand: whether `/user/repos` with such a token also lists public repositories of other owners.

### The `gh` CLI token (`gh auth token`)

- **What it is:** an OAuth-app token (`gho_…`). "The minimum required scopes for the token are: `repo`, `read:org`, and `gist`" (`gh auth login --help`). Observed: `gist, read:org, repo, workflow`.
- **Limits:** OAuth tokens share the user's rate limit with PATs and other apps ([REST rate limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api)).
- **OAuth app access restrictions:** when an organisation enables them, "API access to private organization resources is not available for unapproved OAuth apps". They are "enabled by default" for new organisations ([About OAuth app access restrictions](https://docs.github.com/en/organizations/managing-oauth-access-to-your-organizations-data/about-oauth-app-access-restrictions)). So the GitHub CLI app's token misses private repositories of each restricting organisation that hasn't approved the app.
- **agentdeck's own sign-in** has the same limit. The device flow asks for scope `repo` only (`crates/agentdeckd/src/github_auth/device.rs`) under agentdeck's own OAuth app, so each restricting organisation would have to approve that app.
- **Token order in agentdeck:** a token saved in the keyring (a pasted PAT of either kind, or the device-flow token), then `GH_TOKEN`, then `GITHUB_TOKEN`, then `gh auth token` (`crates/agentdeckd/src/github_auth/mod.rs`). The repositories listed therefore depend on which kind of token won.

## 3. Pagination, rate limits, and search versus listing

- **REST pagination:** the `link` header carries `rel="next"`/`"last"`. "For most endpoints, the maximum value of `per_page` is `100`". A larger value is silently cut to 100 ([REST pagination](https://docs.github.com/en/rest/using-the-rest-api/using-pagination-in-the-rest-api)). Because `/user/repos` is page-numbered and `rel="last"` gives the count, the pages after the first can be fetched in parallel.
- **GraphQL pagination:** "Values of `first` and `last` must be within 1-100". Paging is by cursor, so it is sequential. To score a query, add the requests for each connection, assuming each reaches its `first`, then divide by 100 ([GraphQL limits](https://docs.github.com/en/graphql/overview/rate-limits-and-query-limits-for-the-graphql-api)).
- **Primary limits:** REST allows 5,000 requests per hour per user, and GraphQL 5,000 points per hour per user. These are two separate buckets, each shared by every PAT and OAuth app acting for that user ([REST](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api), [GraphQL](https://docs.github.com/en/graphql/overview/rate-limits-and-query-limits-for-the-graphql-api)).
- **Secondary limits:**
  - at most 100 concurrent requests, shared by REST and GraphQL;
  - at most 900 points per minute for REST and 2,000 for GraphQL;
  - at most 90 s of CPU time per 60 s, of which at most 60 s may be GraphQL.

  Source: [REST rate limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api).
- **Conditional requests:** "Making a conditional request does not count against your primary rate limit if a `304` response is returned" ([best practices](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api)). A cached repository list can be revalidated with `etag` at no cost, and `since=` fetches only repositories updated after a time.
- **Observed cost of listing 1,104 repositories:**

  | API | Requests | Cost | Time |
  | --- | --- | --- | --- |
  | REST, 100 per page | 12 | 12 core requests | 9.7 s |
  | GraphQL, `first: 100` (`nameWithOwner` only) | 12 | 1 point each | 9.2 s |

  Both runs were sequential.
- **Search (`GET /search/repositories`):**
  - "Up to 1,000 results for each search", up to 100 per page.
  - "30 requests per minute for all search endpoints except … Search code" ([REST search](https://docs.github.com/en/rest/search/search)). Observed: `X-Ratelimit-Resource: search`, `X-Ratelimit-Limit: 30`.
  - Search is "globally across all of GitHub". Narrowing it takes the qualifiers `user:USERNAME`, `org:ORGNAME`, `in:name`, `is:private` ("private repositories that you can access") and `fork:true` (forks are left out without it). See [Searching for repositories](https://docs.github.com/en/search-github/searching-on-github/searching-for-repositories).
  - `@me` is documented only for issue and PR search, but `q=agent+in:name+user:@me` worked (observed).
  - Search can't express "every repository I can access": collaborator repositories of other users and organisation repositories need one `user:`/`org:` per owner.
  - A search on each keystroke would use up the 30 per minute in a few seconds of typing. A once-fetched list filtered locally has no such limit.
- **For a typed `owner/name` or URL:** `GET /repos/{owner}/{repo}` (or GraphQL `repository(owner:, name:)`) is one request, and it also reaches public repositories the user has no explicit access to.

## 4. What the PR sync already gives

How agentdeck syncs today (`crates/agentdeckd/src/prsync/github.rs`, `mod.rs`; spec §8, `docs/spec.md`):

- **Full sync:** GraphQL `viewer { pullRequests(states: OPEN, first: 100, orderBy: UPDATED_AT DESC) }`, up to 10 pages (`MAX_LIST_PAGES`), stopping at the `max_age_days` cutoff (default 90).
- **Incremental sync:** GraphQL `search(query: "is:pr author:@me updated:>=<T>", type: ISSUE, first: 50)`, up to 10 pages.
- **PRs by number:** batches of 25, four batches in flight.
- **Repository field:** every PR fragment (`PR_FIELDS`, `LIGHT_FIELDS`) includes `repository { nameWithOwner }`, which is stored as `pull_requests.repo` (`UNIQUE (repo, number)`). Aged-out and unreachable rows stay in the table, with `aged_out_at`/`unreachable_at` set.
- **`involves:@me`:** the sync doesn't use it. `involves` is "a logical OR between the `author`, `assignee`, `mentions`, and `commenter` qualifiers" ([searching issues and PRs](https://docs.github.com/en/search-github/searching-on-github/searching-issues-and-pull-requests)). It would add PRs the user reviews or is mentioned on. Observed: 1,101 PRs for `involves:@me` against 1,099 for `author:@me`.

So the "Repositories of synced PRs" source is free: it is a local query on data the sync already stores, with no GitHub call. Its reach is limited:

- only repositories where the user authored a PR;
- only PRs the sync has seen: open ones, plus anything updated within the age cutoff since sync began;
- repositories with no PR of the user's never appear.

Observed for this account:

| Set | Repositories |
| --- | --- |
| Open PRs (271 PRs) | 270 |
| All 1,099 authored PRs ever | 304 |
| `/user/repos` | 1,104 |

## Sources

- [REST: repositories](https://docs.github.com/en/rest/repos/repos)
- [REST: search](https://docs.github.com/en/rest/search/search)
- [Permissions required for fine-grained PATs](https://docs.github.com/en/rest/authentication/permissions-required-for-fine-grained-personal-access-tokens)
- [Managing your personal access tokens](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens)
- [Authenticating to the REST API](https://docs.github.com/en/rest/authentication/authenticating-to-the-rest-api)
- [Scopes for OAuth apps](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/scopes-for-oauth-apps)
- [About OAuth app access restrictions](https://docs.github.com/en/organizations/managing-oauth-access-to-your-organizations-data/about-oauth-app-access-restrictions)
- [Rate limits for the REST API](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api)
- [Rate limits and query limits for the GraphQL API](https://docs.github.com/en/graphql/overview/rate-limits-and-query-limits-for-the-graphql-api)
- [Using pagination in the REST API](https://docs.github.com/en/rest/using-the-rest-api/using-pagination-in-the-rest-api)
- [Best practices for using the REST API](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api)
- [Searching for repositories](https://docs.github.com/en/search-github/searching-on-github/searching-for-repositories)
- [Searching issues and pull requests](https://docs.github.com/en/search-github/searching-on-github/searching-issues-and-pull-requests)
- [Changelog: fine-grained PATs are generally available (2025-03-18)](https://github.blog/changelog/2025-03-18-fine-grained-pats-are-now-generally-available/)
- GitHub GraphQL schema, by live introspection of `User.repositories` and `RepositoryAffiliation`
- `gh auth login --help`
- agentdeck: `crates/agentdeckd/src/prsync/github.rs`, `crates/agentdeckd/src/prsync/mod.rs`, `crates/agentdeckd/src/github_auth/{mod,device}.rs`, `docs/spec.md` §2, §8
