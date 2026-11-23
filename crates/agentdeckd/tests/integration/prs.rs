//! PR sync: `/v1/prs`, the pull requests and Repositories it keeps (spec §4, §8), against
//! httpmock as GitHub and a stub `gh` on `PATH`.

use std::time::{Duration, Instant};

use agentdeck_client::Client;
use agentdeck_client::types::{
    GhLogin, NewLink, NewWorkspace, PrClose, PrCloseResult, PrSyncProgress, PrSyncStage,
    PrSyncStatus, PullRequest, Repository,
};
use agentdeck_core::{ChecksState, PrRef, PrState, RepositoryId, RepositoryName, ReviewState};
use httpmock::{Mock, MockServer};

use agentdeck_core::ws::{Entity, ServerMessage};

use crate::support::github::{
    BATCH, Pr, days_ago, mock_close, mock_close_refused, mock_fetch, mock_hidden, mock_list,
    mock_list_page, mock_node_ids, mock_search, mock_unauthorized, mock_updated_search,
    mock_viewer, repository_id, set_gh_token,
};
use crate::support::ws::{Frame, Ws};
use crate::support::{TestDaemon, set_daemon_env};
use tempfile::TempDir;

const TOKEN: &str = "gho_test";

/// A daemon whose `gh` prints `token` (or is logged out) and whose GitHub is `server`.
async fn daemon(server: &MockServer, token: Option<&str>) -> TestDaemon {
    let dirs = tempfile::tempdir().expect("a temp dir");
    set_gh_token(dirs.path(), token);
    TestDaemon::start_with_github(dirs, &server.url("/graphql")).await
}

/// A daemon like [`daemon`] whose every sync is a full one, as the first sync is.
async fn full_daemon(server: &MockServer, token: Option<&str>) -> TestDaemon {
    let dirs = tempfile::tempdir().expect("a temp dir");
    set_gh_token(dirs.path(), token);
    set_daemon_env(dirs.path(), &[("AGENTDECK_PR_FULL_SYNC_SECS", "0")]);
    TestDaemon::start_with_github(dirs, &server.url("/graphql")).await
}

/// How long the daemon's GitHub requests may take in [`timing_out_daemon`]
/// (`AGENTDECK_GITHUB_TIMEOUT_MS`).
const TIMEOUT_MS: &str = "300";

/// Longer than [`TIMEOUT_MS`]: how long a slow GitHub holds a request.
const SLOW: Duration = Duration::from_secs(2);

/// A daemon like [`full_daemon`] whose GitHub requests give up after [`TIMEOUT_MS`].
async fn timing_out_daemon(server: &MockServer) -> TestDaemon {
    let dirs = tempfile::tempdir().expect("a temp dir");
    set_gh_token(dirs.path(), Some(TOKEN));
    set_daemon_env(
        dirs.path(),
        &[
            ("AGENTDECK_PR_FULL_SYNC_SECS", "0"),
            ("AGENTDECK_GITHUB_TIMEOUT_MS", TIMEOUT_MS),
        ],
    );
    TestDaemon::start_with_github(dirs, &server.url("/graphql")).await
}

/// Whether `message` says GitHub didn't answer `server` in time, and why.
fn says_timed_out(message: Option<&str>, server: &MockServer) -> bool {
    message
        == Some(
            format!(
                "GitHub didn't answer in time ({}): operation timed out",
                server.url("/graphql")
            )
            .as_str(),
        )
}

async fn sync(client: &Client) -> PrSyncStatus {
    let response = client.prs_sync().send().await.expect("sync");
    assert_eq!(response.status().as_u16(), 200);
    response.into_inner()
}

/// A sync asked to be full, as Sync now asks.
async fn full_sync(client: &Client) -> PrSyncStatus {
    let response = client.prs_sync().full(true).send().await.expect("sync");
    assert_eq!(response.status().as_u16(), 200);
    response.into_inner()
}

/// Every pull request the daemon keeps, Finished ones included, as `GET /v1/prs` lists them.
async fn kept_prs(client: &Client) -> Vec<PullRequest> {
    let response = client.pr_list().finished(true).send().await;
    let response = response.expect("list PRs");
    assert_eq!(response.status().as_u16(), 200);
    response.into_inner()
}

/// The kept pull request `repo#number`, Finished or not.
async fn pr_in(client: &Client, repo: &str, number: u32) -> PullRequest {
    kept_prs(client)
        .await
        .into_iter()
        .find(|pr| pr.repository.name.to_string() == repo && pr.number == number)
        .unwrap_or_else(|| panic!("{repo}#{number}, kept"))
}

/// The kept pull request numbered `number` on `acme/widgets`, Finished or not.
async fn pr_numbered(client: &Client, number: u32) -> PullRequest {
    pr_in(client, "acme/widgets", number).await
}

/// The id of `pr`'s `changed` event: its Repository's GitHub id and its number.
fn changed_id(pr: &Pr) -> String {
    format!("{}#{}", pr.repository_id(), pr.number)
}

/// Open PR `number`, last updated on GitHub `days` ago.
fn aged(number: i64, days: i64) -> Pr {
    Pr {
        updated_at: days_ago(days),
        ..Pr::open(number)
    }
}

/// `pr`'s reference, `owner/name#number`.
fn reference(pr: &Pr) -> PrRef {
    format!("{}#{}", pr.repo, pr.number)
        .parse()
        .expect("a PR reference")
}

/// Every pull request the daemon keeps, as `GET /v1/prs` lists them.
async fn list(client: &Client) -> Vec<PullRequest> {
    let response = client.pr_list().send().await.expect("list PRs");
    assert_eq!(response.status().as_u16(), 200);
    response.into_inner()
}

/// The numbers of the stored PRs, sorted.
async fn numbers(client: &Client) -> Vec<u32> {
    let mut numbers: Vec<u32> = kept_prs(client).await.iter().map(|pr| pr.number).collect();
    numbers.sort_unstable();
    numbers
}

mod post_v1_prs_sync {
    use super::*;

    #[tokio::test]
    async fn should_keep_a_new_pr_with_its_fields_200() {
        let github = MockServer::start_async().await;
        let pr = Pr {
            checks: Some("FAILURE"),
            review: Some("CHANGES_REQUESTED"),
            draft: true,
            ..Pr::open(7)
        };
        mock_search(&github, TOKEN, std::slice::from_ref(&pr));
        let daemon = daemon(&github, Some(TOKEN)).await;

        let status = sync(&daemon.client).await;

        assert_eq!(status.login, GhLogin::LoggedIn);
        assert_eq!(status.last_error, None);
        assert!(status.last_synced_at.is_some());
        let prs = kept_prs(&daemon.client).await;
        assert_eq!(prs.len(), 1);
        let stored = &prs[0];
        assert_eq!(stored.title, "Add PR sync");
        assert!(!stored.finished);
        assert_eq!(stored.repository.name.to_string(), "acme/widgets");
        assert_eq!(stored.number, 7);
        assert_eq!(stored.url, pr.url());
        assert_eq!(stored.state, PrState::Open);
        assert!(stored.draft);
        assert_eq!(stored.head_ref, "feat/sync");
        assert_eq!(stored.checks, Some(ChecksState::Failure));
        assert_eq!(stored.review, Some(ReviewState::ChangesRequested));
    }

    #[tokio::test]
    async fn should_tell_websocket_clients_which_prs_changed_200() {
        let github = MockServer::start_async().await;
        let daemon = daemon(&github, Some(TOKEN)).await;
        // Connected before GitHub shows the PR, so whichever sync finds it, this client hears.
        let mut ws = Ws::connect(&daemon).await;
        mock_search(&github, TOKEN, &[Pr::open(7)]);

        sync(&daemon.client).await;

        let changed = ws
            .until(|frame| match frame {
                Frame::Message(ServerMessage::Changed {
                    entity: Entity::PullRequest,
                    id,
                }) => Some(id),
                _ => None,
            })
            .await;
        assert_eq!(changed, changed_id(&Pr::open(7)));
    }

    #[tokio::test]
    async fn should_update_checks_and_review_on_the_same_pr_200() {
        let github = MockServer::start_async().await;
        let mut search = mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;

        search.delete();
        mock_search(
            &github,
            TOKEN,
            &[Pr {
                checks: Some("SUCCESS"),
                review: Some("APPROVED"),
                ..Pr::open(7)
            }],
        );
        sync(&daemon.client).await;

        let prs = kept_prs(&daemon.client).await;
        assert_eq!(prs.len(), 1);
        let pr = &prs[0];
        assert_eq!(pr.reference.to_string(), "acme/widgets#7");
        assert_eq!(pr.checks, Some(ChecksState::Success));
        assert_eq!(pr.review, Some(ReviewState::Approved));
    }

    #[tokio::test]
    async fn should_finish_a_pr_that_merged_out_of_the_search_200() {
        let github = MockServer::start_async().await;
        let mut search = mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = full_daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;

        search.delete();
        mock_search(&github, TOKEN, &[]);
        let fetch = mock_fetch(
            &github,
            &[Pr {
                state: "MERGED",
                ..Pr::open(7)
            }],
        );
        sync(&daemon.client).await;
        sync(&daemon.client).await;

        let pr = pr_numbered(&daemon.client, 7).await;
        assert!(pr.finished);
        assert_eq!(pr.state, PrState::Merged);
        // Fetched by number once; a merged PR isn't fetched again.
        fetch.assert_calls(1);
    }

    #[tokio::test]
    async fn should_fetch_prs_that_left_the_list_25_at_a_time_200() {
        let github = MockServer::start_async().await;
        let open: Vec<Pr> = (1..=120).map(Pr::open).collect();
        let mut search = mock_search(&github, TOKEN, &open);
        let daemon = full_daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;

        search.delete();
        mock_search(&github, TOKEN, &[]);
        let merged: Vec<Pr> = open
            .iter()
            .map(|pr| Pr {
                state: "MERGED",
                ..pr.clone()
            })
            .collect();
        let fetches: Vec<_> = merged
            .chunks(BATCH)
            .map(|batch| mock_fetch(&github, batch))
            .collect();
        sync(&daemon.client).await;

        let prs = kept_prs(&daemon.client).await;
        assert_eq!(prs.len(), 120);
        assert!(
            prs.iter()
                .all(|pr| pr.state == PrState::Merged && pr.finished)
        );
        for fetch in &fetches {
            fetch.assert_calls(1);
        }
    }

    #[tokio::test]
    async fn should_mark_a_pr_github_no_longer_shows_unreachable_and_keep_it_200() {
        let github = MockServer::start_async().await;
        let mut search = mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = full_daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;

        search.delete();
        mock_search(&github, TOKEN, &[]);
        mock_hidden(&github, TOKEN, &Pr::open(7));
        sync(&daemon.client).await;

        let pr = pr_numbered(&daemon.client, 7).await;
        assert_eq!(pr.state, PrState::Open);
        assert!(pr.unreachable);
        assert!(!pr.finished);
    }

    #[tokio::test]
    async fn should_retry_with_a_fresh_gh_token_after_a_401_200() {
        let github = MockServer::start_async().await;
        let mut old = mock_search(&github, "gho_old", &[]);
        let daemon = daemon(&github, Some("gho_old")).await;
        sync(&daemon.client).await;

        old.delete();
        mock_unauthorized(&github, "gho_old");
        mock_search(&github, "gho_new", &[Pr::open(7)]);
        set_gh_token(daemon.root(), Some("gho_new"));
        let status = sync(&daemon.client).await;

        assert_eq!(status.login, GhLogin::LoggedIn);
        assert_eq!(status.last_error, None);
        assert_eq!(kept_prs(&daemon.client).await.len(), 1);
    }

    #[tokio::test]
    async fn should_log_out_when_github_rejects_the_gh_token_200() {
        let github = MockServer::start_async().await;
        mock_unauthorized(&github, TOKEN);
        let daemon = daemon(&github, Some(TOKEN)).await;

        let status = sync(&daemon.client).await;

        assert_eq!(status.login, GhLogin::LoggedOut);
        assert_eq!(
            status.last_error.as_deref(),
            Some("GitHub rejected the token")
        );
    }

    #[tokio::test]
    async fn should_not_call_github_without_a_gh_login_200() {
        let github = MockServer::start_async().await;
        let any = github.mock(|_, then| {
            then.status(500);
        });
        let daemon = daemon(&github, None).await;

        let status = sync(&daemon.client).await;

        assert_eq!(status.login, GhLogin::LoggedOut);
        assert!(
            status
                .last_error
                .as_deref()
                .is_some_and(|err| err.starts_with("no GitHub token")),
            "{status:?}"
        );
        assert_eq!(status.last_synced_at, None);
        any.assert_calls(0);
    }

    #[tokio::test]
    async fn should_pick_up_a_gh_login_made_after_start_200() {
        let github = MockServer::start_async().await;
        mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = daemon(&github, None).await;

        set_gh_token(daemon.root(), Some(TOKEN));
        let status = sync(&daemon.client).await;

        assert_eq!(status.login, GhLogin::LoggedIn);
        assert_eq!(kept_prs(&daemon.client).await.len(), 1);
    }

    #[tokio::test]
    async fn should_log_out_once_gh_logs_out_though_github_still_takes_its_token_200() {
        let github = MockServer::start_async().await;
        mock_search(&github, TOKEN, &[]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;

        set_gh_token(daemon.root(), None);
        let status = sync(&daemon.client).await;

        assert_eq!(status.login, GhLogin::LoggedOut);
        assert!(
            status
                .last_error
                .as_deref()
                .is_some_and(|err| err.starts_with("no GitHub token")),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn should_sync_with_the_token_gh_gives_after_another_login_200() {
        let github = MockServer::start_async().await;
        mock_search(&github, "gho_old", &[]);
        let daemon = daemon(&github, Some("gho_old")).await;
        sync(&daemon.client).await;

        mock_search(&github, "gho_new", &[Pr::open(7)]);
        set_gh_token(daemon.root(), Some("gho_new"));
        let status = sync(&daemon.client).await;

        assert_eq!(status.login, GhLogin::LoggedIn);
        assert_eq!(kept_prs(&daemon.client).await.len(), 1);
    }

    #[tokio::test]
    async fn should_report_a_github_failure_briefly_after_three_tries_and_keep_the_prs_200() {
        let github = MockServer::start_async().await;
        let mut search = mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        let synced = sync(&daemon.client).await;

        search.delete();
        let bad_gateway = github.mock(|_, then| {
            then.status(502)
                .body("<html><head><title>502 Bad Gateway</title></head></html>");
        });
        let status = sync(&daemon.client).await;

        assert_eq!(status.login, GhLogin::LoggedIn);
        assert_eq!(status.last_error.as_deref(), Some("GitHub answered 502"));
        assert_eq!(status.last_synced_at, synced.last_synced_at);
        assert_eq!(kept_prs(&daemon.client).await.len(), 1);
        bad_gateway.assert_calls(3);
    }

    #[tokio::test]
    async fn should_keep_the_message_github_gives_with_a_failure_200() {
        let github = MockServer::start_async().await;
        github.mock(|_, then| {
            then.status(403)
                .json_body(serde_json::json!({ "message": "API rate limit exceeded" }));
        });
        let daemon = daemon(&github, Some(TOKEN)).await;

        let status = sync(&daemon.client).await;

        assert_eq!(
            status.last_error.as_deref(),
            Some("GitHub answered 403: API rate limit exceeded")
        );
    }

    #[tokio::test]
    async fn should_say_github_did_not_answer_in_time_when_a_sync_times_out_200() {
        let github = MockServer::start_async().await;
        let mut search = mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = timing_out_daemon(&github).await;
        sync(&daemon.client).await;

        search.delete();
        let slow = github.mock(|_, then| {
            then.status(200)
                .delay(SLOW)
                .json_body(serde_json::json!({ "data": {} }));
        });
        let status = sync(&daemon.client).await;

        assert!(
            says_timed_out(status.last_error.as_deref(), &github),
            "{:?}",
            status.last_error
        );
        assert_eq!(kept_prs(&daemon.client).await.len(), 1);
        slow.assert_calls(3);
    }

    #[tokio::test]
    async fn should_retry_a_github_hiccup_and_finish_the_sync() {
        let github = MockServer::start_async().await;
        let bad_gateway = github.mock(|_, then| {
            then.status(502).body("bad gateway");
        });
        // The timer's first sync, at start, meets the 502.
        let daemon = daemon(&github, Some(TOKEN)).await;
        let deadline = Instant::now() + Duration::from_secs(10);
        while bad_gateway.calls_async().await == 0 {
            assert!(
                Instant::now() < deadline,
                "no call to GitHub 10 s after start"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        bad_gateway.delete_async().await;
        mock_search(&github, TOKEN, &[Pr::open(7)]);

        // The same sync, retried: no other runs for 120 s.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = daemon
                .client
                .prs_status()
                .send()
                .await
                .expect("status")
                .into_inner();
            if status.last_synced_at.is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "not synced: {status:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(kept_prs(&daemon.client).await.len(), 1);
    }
}

mod incremental {
    use super::*;

    /// A daemon past its first, full sync of `open`, whose incremental search shows nothing
    /// until a test says otherwise. The first sync's mocks are deleted, so a test's own fetch
    /// by number is the only one.
    async fn synced<'a>(github: &'a MockServer, open: &[Pr]) -> (TestDaemon, Mock<'a>) {
        let mut first = mock_list(github, TOKEN, open);
        let updated = mock_updated_search(github, TOKEN, &[]);
        let daemon = daemon(github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        first.delete();
        (daemon, updated)
    }

    #[tokio::test]
    async fn should_ask_only_for_prs_updated_since_the_last_sync_200() {
        let github = MockServer::start_async().await;
        let full = mock_list(&github, TOKEN, &[Pr::open(7)]);
        let updated = mock_updated_search(&github, TOKEN, &[]);
        let daemon = daemon(&github, Some(TOKEN)).await;

        sync(&daemon.client).await;
        sync(&daemon.client).await;

        full.list.assert_calls(1);
        assert!(updated.calls() >= 1);
        assert_eq!(kept_prs(&daemon.client).await.len(), 1);
    }

    #[tokio::test]
    async fn should_finish_a_pr_merged_since_the_last_sync_without_a_fetch_200() {
        let github = MockServer::start_async().await;
        let (daemon, mut updated) = synced(&github, &[Pr::open(7)]).await;
        let fetch = github.mock(|when, then| {
            when.body_includes("pullRequest(");
            then.status(500);
        });

        updated.delete();
        mock_updated_search(
            &github,
            TOKEN,
            &[Pr {
                state: "MERGED",
                ..Pr::open(7)
            }],
        );
        sync(&daemon.client).await;

        let pr = pr_numbered(&daemon.client, 7).await;
        assert_eq!(pr.state, PrState::Merged);
        assert!(pr.finished);
        fetch.assert_calls(0);
    }

    #[tokio::test]
    async fn should_not_store_a_closed_pr_it_never_saw_open_200() {
        let github = MockServer::start_async().await;
        let (daemon, mut updated) = synced(&github, &[Pr::open(7)]).await;

        updated.delete();
        mock_updated_search(
            &github,
            TOKEN,
            &[Pr {
                state: "MERGED",
                ..Pr::open(9)
            }],
        );
        sync(&daemon.client).await;

        assert_eq!(numbers(&daemon.client).await, [7]);
    }

    #[tokio::test]
    async fn should_refresh_pending_checks_the_search_leaves_out_200() {
        let github = MockServer::start_async().await;
        let pending = Pr {
            checks: Some("PENDING"),
            ..Pr::open(7)
        };
        let (daemon, _updated) = synced(&github, &[pending]).await;
        let fetch = mock_fetch(
            &github,
            &[Pr {
                checks: Some("SUCCESS"),
                ..Pr::open(7)
            }],
        );

        sync(&daemon.client).await;

        let pr = pr_numbered(&daemon.client, 7).await;
        assert_eq!(pr.checks, Some(ChecksState::Success));
        assert!(fetch.calls() >= 1);
    }

    #[tokio::test]
    async fn should_refetch_a_pr_stored_before_its_checks_started_and_say_it_changed_200() {
        let github = MockServer::start_async().await;
        let (daemon, _updated) = synced(&github, &[Pr::open(7)]).await;
        let mut ws = Ws::connect(&daemon).await;
        let fetch = mock_fetch(
            &github,
            &[Pr {
                checks: Some("FAILURE"),
                ..Pr::open(7)
            }],
        );

        sync(&daemon.client).await;

        let pr = pr_numbered(&daemon.client, 7).await;
        assert_eq!(pr.checks, Some(ChecksState::Failure));
        assert!(fetch.calls() >= 1);
        let changed = ws
            .until(|frame| match frame {
                Frame::Message(ServerMessage::Changed {
                    entity: Entity::PullRequest,
                    id,
                }) => Some(id),
                _ => None,
            })
            .await;
        assert_eq!(changed, changed_id(&Pr::open(7)));
    }

    #[tokio::test]
    async fn should_refetch_a_pr_awaiting_a_review_200() {
        let github = MockServer::start_async().await;
        let awaiting = Pr {
            checks: Some("SUCCESS"),
            review: Some("REVIEW_REQUIRED"),
            ..Pr::open(7)
        };
        let (daemon, _updated) = synced(&github, std::slice::from_ref(&awaiting)).await;
        mock_fetch(
            &github,
            &[Pr {
                review: Some("APPROVED"),
                ..awaiting
            }],
        );

        sync(&daemon.client).await;

        let pr = pr_numbered(&daemon.client, 7).await;
        assert_eq!(pr.review, Some(ReviewState::Approved));
    }

    #[tokio::test]
    async fn should_not_refetch_a_pr_whose_checks_finished_and_review_was_approved_200() {
        let github = MockServer::start_async().await;
        let settled = Pr {
            checks: Some("SUCCESS"),
            review: Some("APPROVED"),
            ..Pr::open(7)
        };
        let (daemon, _updated) = synced(&github, &[settled]).await;
        let fetch = github.mock(|when, then| {
            when.body_includes("pullRequest(");
            then.status(500);
        });

        sync(&daemon.client).await;

        fetch.assert_calls(0);
    }

    #[tokio::test]
    async fn should_not_add_a_pr_older_than_the_cutoff_200() {
        let github = MockServer::start_async().await;
        let mut first = mock_list(&github, TOKEN, &[]);
        let mut updated = mock_updated_search(&github, TOKEN, &[]);
        let dirs = tempfile::tempdir().expect("a temp dir");
        set_gh_token(dirs.path(), Some(TOKEN));
        set_daemon_env(dirs.path(), &[("AGENTDECK_PR_MAX_AGE_DAYS", "90")]);
        let daemon = TestDaemon::start_with_github(dirs, &github.url("/graphql")).await;
        sync(&daemon.client).await;
        first.delete();

        updated.delete();
        mock_updated_search(&github, TOKEN, &[aged(8, 400), aged(9, 1)]);
        sync(&daemon.client).await;

        assert_eq!(numbers(&daemon.client).await, [9]);
    }
}

mod full {
    use super::*;

    #[tokio::test]
    async fn should_fetch_details_only_for_new_changed_or_unsettled_prs_200() {
        let github = MockServer::start_async().await;
        let settled = Pr {
            checks: Some("SUCCESS"),
            ..Pr::open(7)
        };
        let mut first = mock_list(&github, TOKEN, &[settled.clone(), Pr::open(8)]);
        let daemon = full_daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        first.delete();

        let changed = Pr {
            updated_at: "2026-11-02T10:00:00Z",
            checks: Some("SUCCESS"),
            ..Pr::open(8)
        };
        let list = mock_list_page(
            &github,
            TOKEN,
            &[settled.clone(), changed.clone()],
            None,
            None,
            2,
            Duration::ZERO,
        );
        let unchanged = mock_fetch(&github, &[settled]);
        let details = mock_fetch(&github, &[changed]);
        sync(&daemon.client).await;

        list.assert_calls(1);
        unchanged.assert_calls(0);
        details.assert_calls(1);
        let checks: Vec<_> = kept_prs(&daemon.client)
            .await
            .into_iter()
            .map(|pr| (pr.number, pr.checks))
            .collect();
        assert!(
            checks.contains(&(8, Some(ChecksState::Success))),
            "{checks:?}"
        );
    }

    #[tokio::test]
    async fn should_say_it_is_fetching_details_once_the_list_is_read() {
        let github = MockServer::start_async().await;
        mock_list_page(
            &github,
            TOKEN,
            &[Pr::open(7)],
            None,
            None,
            1,
            Duration::ZERO,
        );
        github.mock(|when, then| {
            when.body_includes("pullRequest(");
            then.status(200)
                .delay(Duration::from_secs(5))
                .json_body(serde_json::json!({ "data": {} }));
        });
        let daemon = daemon(&github, Some(TOKEN)).await;

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let status = daemon
                .client
                .prs_status()
                .send()
                .await
                .expect("status")
                .into_inner();
            if status.progress.as_ref().map(|p| p.stage) == Some(PrSyncStage::Details) {
                break;
            }
            assert!(Instant::now() < deadline, "never said details: {status:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn should_list_a_new_pr_before_its_details_arrive() {
        let github = MockServer::start_async().await;
        mock_list_page(
            &github,
            TOKEN,
            &[Pr::open(7)],
            None,
            None,
            1,
            Duration::ZERO,
        );
        github.mock(|when, then| {
            when.body_includes("pullRequest(");
            then.status(200)
                .delay(Duration::from_secs(5))
                .json_body(serde_json::json!({ "data": {} }));
        });
        let daemon = daemon(&github, Some(TOKEN)).await;

        let deadline = Instant::now() + Duration::from_secs(3);
        while kept_prs(&daemon.client).await.is_empty() {
            assert!(Instant::now() < deadline, "no row while its details load");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

mod asked_full {
    use super::*;

    /// Whether PR 7 is Finished, and its stored state.
    async fn seven(client: &Client) -> (bool, PrState) {
        let pr = pr_numbered(client, 7).await;
        (pr.finished, pr.state)
    }

    #[tokio::test]
    async fn should_reopen_a_pr_stored_closed_that_no_incremental_search_shows_200() {
        let github = MockServer::start_async().await;
        mock_list(&github, TOKEN, &[Pr::open(7)]);
        let mut updated = mock_updated_search(&github, TOKEN, &[]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        updated.delete();
        let mut closed = mock_updated_search(
            &github,
            TOKEN,
            &[Pr {
                state: "CLOSED",
                ..Pr::open(7)
            }],
        );
        sync(&daemon.client).await;
        closed.delete();
        mock_updated_search(&github, TOKEN, &[]);

        sync(&daemon.client).await;
        let incremental = seven(&daemon.client).await;
        let status = full_sync(&daemon.client).await;

        assert_eq!(incremental, (true, PrState::Closed));
        assert_eq!(status.last_error, None);
        assert_eq!(seven(&daemon.client).await, (false, PrState::Open));
    }

    #[tokio::test]
    async fn should_run_a_full_sync_after_the_one_running_and_answer_once_it_ends_200() {
        let github = MockServer::start_async().await;
        let list = mock_list_page(
            &github,
            TOKEN,
            &[Pr::open(7)],
            None,
            None,
            1,
            Duration::from_millis(800),
        );
        mock_fetch(&github, &[Pr::open(7)]);
        let updated = mock_updated_search(&github, TOKEN, &[]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        let deadline = Instant::now() + Duration::from_secs(5);
        while (daemon.client.prs_status().send().await)
            .expect("status")
            .into_inner()
            .progress
            .is_none()
        {
            assert!(Instant::now() < deadline, "the first sync never started");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let status = full_sync(&daemon.client).await;

        list.assert_calls(2);
        updated.assert_calls(0);
        assert_eq!(status.progress, None);
        assert_eq!(status.last_error, None);
    }
}

mod sign_in_changes {
    use super::*;

    /// A token that sees every repo.
    const FULL: &str = "gho_full";

    /// A fine-grained token that sees only `acme/widgets`.
    const NARROW: &str = "github_pat_narrow";

    /// PR 8, on a repo [`NARROW`] can't see.
    fn secret() -> Pr {
        Pr {
            repo: "acme/secret",
            ..Pr::open(8)
        }
    }

    /// A daemon that synced PR 7 and [`secret`] with [`FULL`], and that GitHub answers for
    /// [`NARROW`] with PR 7 alone.
    async fn narrowed(github: &MockServer) -> TestDaemon {
        mock_search(github, FULL, &[Pr::open(7), secret()]);
        mock_search(github, NARROW, &[Pr::open(7)]);
        mock_hidden(github, NARROW, &secret());
        let daemon = daemon(github, Some(FULL)).await;
        sync(&daemon.client).await;
        set_gh_token(daemon.root(), Some(NARROW));
        daemon
    }

    #[tokio::test]
    async fn should_mark_the_prs_of_a_repo_the_new_token_cannot_see_unreachable_200() {
        let github = MockServer::start_async().await;
        let daemon = narrowed(&github).await;

        let status = sync(&daemon.client).await;

        assert_eq!(status.last_error, None);
        let pr = pr_in(&daemon.client, "acme/secret", 8).await;
        assert_eq!(pr.state, PrState::Open);
        assert!(pr.unreachable);
        assert!(!pr.finished);
        let seen = pr_in(&daemon.client, "acme/widgets", 7).await;
        assert!(!seen.unreachable);
    }

    #[tokio::test]
    async fn should_bring_back_a_pr_the_token_sees_again_200() {
        let github = MockServer::start_async().await;
        let daemon = narrowed(&github).await;
        sync(&daemon.client).await;

        set_gh_token(daemon.root(), Some(FULL));
        sync(&daemon.client).await;

        let pr = pr_in(&daemon.client, "acme/secret", 8).await;
        assert_eq!(pr.state, PrState::Open);
        assert!(!pr.unreachable);
    }

    #[tokio::test]
    async fn should_not_close_an_unreachable_pr_200() {
        let github = MockServer::start_async().await;
        let daemon = narrowed(&github).await;
        sync(&daemon.client).await;
        let lookup = github.mock(|when, then| {
            when.body_includes("fragment nodeId on PullRequest");
            then.status(500);
        });

        let results = daemon
            .client
            .prs_close()
            .body(PrClose {
                prs: vec![reference(&secret())],
            })
            .send()
            .await
            .expect("close")
            .into_inner()
            .results;

        assert_eq!(results.len(), 1);
        assert!(!results[0].ok);
        assert_eq!(results[0].error.as_deref(), Some("Not an open PR"));
        lookup.assert_calls(0);
    }

    #[tokio::test]
    async fn should_run_a_full_sync_with_another_token_200() {
        let github = MockServer::start_async().await;
        mock_search(&github, "gho_old", &[]);
        let new = mock_search(&github, "gho_new", &[Pr::open(7)]);
        let updated = mock_updated_search(&github, "gho_new", &[Pr::open(7)]);
        let daemon = daemon(&github, Some("gho_old")).await;
        sync(&daemon.client).await;

        set_gh_token(daemon.root(), Some("gho_new"));
        sync(&daemon.client).await;

        new.list.assert_calls(1);
        updated.assert_calls(0);
    }

    #[tokio::test]
    async fn should_run_a_full_sync_after_signing_out_and_in_again_200() {
        let github = MockServer::start_async().await;
        let listing = mock_list(&github, TOKEN, &[Pr::open(7)]);
        let updated = mock_updated_search(&github, TOKEN, &[]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        set_gh_token(daemon.root(), None);
        sync(&daemon.client).await;
        let (lists, searches) = (listing.list.calls(), updated.calls());

        set_gh_token(daemon.root(), Some(TOKEN));
        let status = sync(&daemon.client).await;

        assert_eq!(status.last_error, None);
        assert_eq!(listing.list.calls(), lists + 1);
        assert_eq!(updated.calls(), searches);
    }

    #[tokio::test]
    async fn should_keep_every_pr_and_the_login_they_synced_as_once_signed_out_200() {
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", Some("repo"));
        mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        let signed_in = sync(&daemon.client).await;

        set_gh_token(daemon.root(), None);
        let status = sync(&daemon.client).await;

        assert_eq!(signed_in.synced_as.as_deref(), Some("octocat"));
        assert_eq!(status.login, GhLogin::LoggedOut);
        assert_eq!(status.synced_as.as_deref(), Some("octocat"));
        let pr = pr_numbered(&daemon.client, 7).await;
        assert!(!pr.finished);
        assert_eq!(pr.state, PrState::Open);
    }

    #[tokio::test]
    async fn should_report_the_login_prs_synced_as_after_a_restart_200() {
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", Some("repo"));
        mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        let (_, dirs) = daemon.terminate();
        set_gh_token(dirs.path(), None);

        let daemon = TestDaemon::start_with_github(dirs, &github.url("/graphql")).await;
        let status = sync(&daemon.client).await;

        assert_eq!(status.login, GhLogin::LoggedOut);
        assert_eq!(status.synced_as.as_deref(), Some("octocat"));
    }

    #[tokio::test]
    async fn should_report_no_login_before_any_sync_200() {
        let github = MockServer::start_async().await;
        let daemon = daemon(&github, None).await;

        let status = sync(&daemon.client).await;

        assert_eq!(status.synced_as, None);
    }
}

mod max_age {
    use super::*;

    /// Temp dirs for a daemon whose `gh` prints [`TOKEN`] and whose every sync is full, with
    /// `AGENTDECK_PR_MAX_AGE_DAYS` set to `days`.
    fn dirs_with_cutoff(days: &str) -> TempDir {
        let dirs = tempfile::tempdir().expect("a temp dir");
        set_gh_token(dirs.path(), Some(TOKEN));
        set_daemon_env(
            dirs.path(),
            &[
                ("AGENTDECK_PR_FULL_SYNC_SECS", "0"),
                ("AGENTDECK_PR_MAX_AGE_DAYS", days),
            ],
        );
        dirs
    }

    /// A daemon that stored `open` with no cutoff, restarted with a 90-day one. Its
    /// `gh` is logged out until a test logs it in, so nothing syncs before the test's own
    /// mocks are up; the first daemon's mocks are deleted.
    pub(super) async fn restarted_with_cutoff(github: &MockServer, open: &[Pr]) -> TestDaemon {
        let mut first = mock_list(github, TOKEN, open);
        let daemon =
            TestDaemon::start_with_github(dirs_with_cutoff("0"), &github.url("/graphql")).await;
        sync(&daemon.client).await;
        assert_eq!(kept_prs(&daemon.client).await.len(), open.len());
        let (_, dirs) = daemon.terminate();
        first.delete();
        set_gh_token(dirs.path(), None);
        set_daemon_env(
            dirs.path(),
            &[
                ("AGENTDECK_PR_FULL_SYNC_SECS", "0"),
                ("AGENTDECK_PR_MAX_AGE_DAYS", "90"),
            ],
        );
        TestDaemon::start_with_github(dirs, &github.url("/graphql")).await
    }

    /// GitHub's list of open PRs, most recently updated first, as one page.
    pub(super) fn mock_open<'a>(github: &'a MockServer, prs: &[Pr]) -> Mock<'a> {
        mock_list_page(github, TOKEN, prs, None, None, prs.len(), Duration::ZERO)
    }

    #[tokio::test]
    async fn should_stop_paging_at_the_first_pr_older_than_the_cutoff_200() {
        let github = MockServer::start_async().await;
        let first = mock_list_page(
            &github,
            TOKEN,
            &[aged(7, 1), aged(8, 400)],
            None,
            Some("c1"),
            3,
            Duration::ZERO,
        );
        let second = mock_list_page(
            &github,
            TOKEN,
            &[aged(9, 500)],
            Some("c1"),
            None,
            3,
            Duration::ZERO,
        );
        mock_fetch(&github, &[aged(7, 1)]);
        let old = mock_fetch(&github, &[aged(8, 400)]);
        let daemon =
            TestDaemon::start_with_github(dirs_with_cutoff("90"), &github.url("/graphql")).await;

        sync(&daemon.client).await;

        assert!(first.calls() >= 1);
        second.assert_calls(0);
        old.assert_calls(0);
        assert_eq!(numbers(&daemon.client).await, [7]);
    }

    #[tokio::test]
    async fn should_read_the_cutoff_from_config_toml_200() {
        let github = MockServer::start_async().await;
        mock_list(&github, TOKEN, &[aged(7, 1), aged(8, 40)]);
        let dirs = tempfile::tempdir().expect("a temp dir");
        set_gh_token(dirs.path(), Some(TOKEN));
        // Empty, so the harness's no-cutoff default doesn't override the file.
        set_daemon_env(dirs.path(), &[("AGENTDECK_PR_MAX_AGE_DAYS", "")]);
        let config = dirs.path().join("config/agentdeck");
        std::fs::create_dir_all(&config).expect("creating the config dir");
        std::fs::write(
            config.join("config.toml"),
            "[pr_sync]\nmax_age_days = 30\n\n[some_later_section]\nkey = 1\n",
        )
        .expect("writing config.toml");
        let daemon = TestDaemon::start_with_github(dirs, &github.url("/graphql")).await;

        sync(&daemon.client).await;

        assert_eq!(numbers(&daemon.client).await, [7]);
    }

    #[tokio::test]
    async fn should_finish_a_stored_pr_older_than_the_cutoff_and_say_so_200() {
        let github = MockServer::start_async().await;
        let daemon = restarted_with_cutoff(&github, &[aged(7, 1), aged(8, 400)]).await;
        mock_open(&github, &[aged(7, 1), aged(8, 400)]);
        // 7 has no checks, so it is fetched again in case they started.
        mock_fetch(&github, &[aged(7, 1)]);
        let mut ws = Ws::connect(&daemon).await;

        set_gh_token(daemon.root(), Some(TOKEN));
        sync(&daemon.client).await;

        let id = changed_id(&aged(8, 400));
        ws.until(|frame| match frame {
            Frame::Message(ServerMessage::Changed {
                entity: Entity::PullRequest,
                id: changed,
            }) if changed == id => Some(()),
            _ => None,
        })
        .await;
        let old = pr_numbered(&daemon.client, 8).await;
        assert!(old.finished);
        // Nothing changed on GitHub: the PR is still open.
        assert_eq!(old.state, PrState::Open);
        assert!(!pr_numbered(&daemon.client, 7).await.finished);
    }

    #[tokio::test]
    async fn should_not_fetch_a_pr_older_than_the_cutoff_as_gone_200() {
        let github = MockServer::start_async().await;
        let daemon = restarted_with_cutoff(&github, &[aged(7, 1), aged(8, 400)]).await;
        // However the list leaves it out, a PR stored older than the cutoff isn't gone.
        mock_open(&github, &[aged(7, 1)]);
        let gone = mock_fetch(&github, &[aged(8, 400)]);

        set_gh_token(daemon.root(), Some(TOKEN));
        sync(&daemon.client).await;
        sync(&daemon.client).await;

        gone.assert_calls(0);
    }

    #[tokio::test]
    async fn should_bring_back_an_aged_out_pr_github_updates_again_200() {
        let github = MockServer::start_async().await;
        let daemon = restarted_with_cutoff(&github, &[aged(8, 400)]).await;
        let mut list = mock_open(&github, &[aged(8, 400)]);
        set_gh_token(daemon.root(), Some(TOKEN));
        sync(&daemon.client).await;
        assert!(pr_numbered(&daemon.client, 8).await.finished);

        list.delete();
        let fresh = aged(8, 0);
        mock_open(&github, std::slice::from_ref(&fresh));
        mock_fetch(&github, &[fresh]);
        sync(&daemon.client).await;

        assert!(!pr_numbered(&daemon.client, 8).await.finished);
    }
}

mod progress {
    use super::*;

    /// A daemon whose first sync reads two pages: PR 1 at once, then PR 2 after `delay`.
    async fn two_pages(github: &MockServer, delay: Duration) -> TestDaemon {
        mock_list_page(
            github,
            TOKEN,
            &[Pr::open(1)],
            None,
            Some("c1"),
            2,
            Duration::ZERO,
        );
        mock_list_page(github, TOKEN, &[Pr::open(2)], Some("c1"), None, 2, delay);
        mock_fetch(github, &[Pr::open(1), Pr::open(2)]);
        daemon(github, Some(TOKEN)).await
    }

    async fn status(client: &Client) -> PrSyncStatus {
        client
            .prs_status()
            .send()
            .await
            .expect("status")
            .into_inner()
    }

    #[tokio::test]
    async fn should_store_each_page_as_it_arrives_and_say_how_far_it_is() {
        let github = MockServer::start_async().await;
        let daemon = two_pages(&github, Duration::from_secs(3)).await;

        let deadline = Instant::now() + Duration::from_secs(2);
        while kept_prs(&daemon.client).await.is_empty() {
            assert!(
                Instant::now() < deadline,
                "page 1 not stored while page 2 waits"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(
            status(&daemon.client).await.progress,
            Some(PrSyncProgress {
                prs_done: 1,
                prs: 2,
                stage: PrSyncStage::Listing,
            })
        );
        let finished = sync(&daemon.client).await;
        assert_eq!(finished.progress, None);
        assert_eq!(kept_prs(&daemon.client).await.len(), 2);
    }

    #[tokio::test]
    async fn should_tell_websocket_clients_the_sync_status_changed() {
        let github = MockServer::start_async().await;
        let daemon = two_pages(&github, Duration::from_millis(500)).await;
        let mut ws = Ws::connect(&daemon).await;

        ws.until(|frame| match frame {
            Frame::Message(ServerMessage::Changed {
                entity: Entity::PrSync,
                ..
            }) => Some(()),
            _ => None,
        })
        .await;
    }
}

mod get_v1_prs_status {
    use super::*;

    #[tokio::test]
    async fn should_report_logged_out_when_gh_has_no_login_200() {
        let daemon = TestDaemon::start().await;

        let response = daemon.client.prs_status().send().await.expect("status");

        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(response.into_inner().login, GhLogin::LoggedOut);
    }

    #[tokio::test]
    async fn should_report_the_age_cutoff_the_sync_uses_200() {
        let dirs = tempfile::tempdir().expect("a temp dir");
        set_daemon_env(dirs.path(), &[("AGENTDECK_PR_MAX_AGE_DAYS", "30")]);
        let daemon = TestDaemon::start_in(dirs).await;

        let response = daemon.client.prs_status().send().await.expect("status");

        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(response.into_inner().max_age_days, 30);
    }

    #[tokio::test]
    async fn should_report_no_age_cutoff_as_zero_200() {
        let dirs = tempfile::tempdir().expect("a temp dir");
        set_daemon_env(dirs.path(), &[("AGENTDECK_PR_MAX_AGE_DAYS", "0")]);
        let daemon = TestDaemon::start_in(dirs).await;

        let response = daemon.client.prs_status().send().await.expect("status");

        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(response.into_inner().max_age_days, 0);
    }
}

mod timer {
    use super::*;

    #[tokio::test]
    async fn should_sync_once_at_start_without_being_asked() {
        let github = MockServer::start_async().await;
        mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = daemon(&github, Some(TOKEN)).await;

        let deadline = Instant::now() + Duration::from_secs(10);
        while kept_prs(&daemon.client).await.is_empty() {
            assert!(Instant::now() < deadline, "no PR 10 s after start");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let status = daemon
            .client
            .prs_status()
            .send()
            .await
            .expect("status")
            .into_inner();
        assert_eq!(status.login, GhLogin::LoggedIn);
    }
}

mod get_v1_prs {
    use super::*;

    /// A daemon that synced `prs`, whose next sync is full.
    async fn synced(github: &MockServer, prs: &[Pr]) -> TestDaemon {
        mock_search(github, TOKEN, prs);
        let daemon = full_daemon(github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        daemon
    }

    /// The pull requests of Repository `name`, as `GET /v1/prs?repository=` lists them.
    async fn list_of(client: &Client, name: &str) -> Vec<PullRequest> {
        let name: RepositoryName = name.parse().expect("a Repository");
        let response = client.pr_list().repository(name).send().await;
        response.expect("list PRs").into_inner()
    }

    /// `pr` once GitHub renamed `acme/widgets`, its Repository, to `acme/gizmos`.
    fn renamed(pr: Pr) -> Pr {
        Pr {
            repo: "acme/gizmos",
            repo_id: Some(pr.repository_id()),
            ..pr
        }
    }

    #[tokio::test]
    async fn should_list_each_pr_with_its_reference_repository_and_fields_200() {
        let github = MockServer::start_async().await;
        let pr = Pr {
            private: true,
            draft: true,
            head_ref: "fix/sync",
            base_ref: "release/1.2",
            author: Some("hubot"),
            checks: Some("FAILURE"),
            review: Some("CHANGES_REQUESTED"),
            ..Pr::open(7)
        };
        let daemon = synced(&github, std::slice::from_ref(&pr)).await;

        let prs = list(&daemon.client).await;

        assert_eq!(
            prs,
            [PullRequest {
                reference: "acme/widgets#7".parse().unwrap(),
                number: 7,
                repository: Repository {
                    id: RepositoryId::from(repository_id("acme/widgets")),
                    name: "acme/widgets".parse().unwrap(),
                    url: "https://github.com/acme/widgets".into(),
                    private: true,
                    archived: false,
                },
                title: "Add PR sync".into(),
                url: pr.url(),
                state: PrState::Open,
                draft: true,
                head_ref: "fix/sync".into(),
                base_ref: "release/1.2".into(),
                author: Some("hubot".into()),
                checks: Some(ChecksState::Failure),
                review: Some(ReviewState::ChangesRequested),
                updated_at: pr.updated_at.into(),
                unreachable: false,
                finished: false,
            }]
        );
    }

    #[tokio::test]
    async fn should_list_no_author_for_a_pr_whose_account_is_gone_200() {
        let github = MockServer::start_async().await;
        let ghost = Pr {
            author: None,
            ..Pr::open(7)
        };
        let daemon = synced(&github, &[ghost]).await;

        let prs = list(&daemon.client).await;

        assert_eq!(prs[0].author, None);
    }

    #[tokio::test]
    async fn should_list_prs_by_repository_name_then_number_200() {
        let github = MockServer::start_async().await;
        let gadgets = |number| Pr {
            repo: "Acme/gadgets",
            ..Pr::open(number)
        };
        let daemon = synced(
            &github,
            &[Pr::open(9), gadgets(12), Pr::open(7), gadgets(3)],
        )
        .await;

        let prs = list(&daemon.client).await;

        let references: Vec<String> = prs.iter().map(|pr| pr.reference.to_string()).collect();
        assert_eq!(
            references,
            [
                "Acme/gadgets#3",
                "Acme/gadgets#12",
                "acme/widgets#7",
                "acme/widgets#9"
            ]
        );
    }

    #[tokio::test]
    async fn should_list_only_the_repository_asked_for_ignoring_case_200() {
        let github = MockServer::start_async().await;
        let gadget = Pr {
            repo: "acme/gadgets",
            ..Pr::open(9)
        };
        let daemon = synced(&github, &[Pr::open(7), gadget]).await;

        let gadgets = list_of(&daemon.client, "ACME/Gadgets").await;
        let none = list_of(&daemon.client, "acme/nothing").await;

        let references: Vec<String> = gadgets.iter().map(|pr| pr.reference.to_string()).collect();
        assert_eq!(references, ["acme/gadgets#9"]);
        assert!(none.is_empty(), "{none:?}");
    }

    #[tokio::test]
    async fn should_list_only_the_prs_of_the_repositories_a_workspace_links_200() {
        let github = MockServer::start_async().await;
        let gadget = Pr {
            repo: "acme/gadgets",
            ..Pr::open(9)
        };
        let daemon = synced(&github, &[Pr::open(7), gadget]).await;
        let web = daemon
            .client
            .workspace_create()
            .body(NewWorkspace {
                name: "web".into(),
                dir: None,
            })
            .send()
            .await
            .expect("create a workspace")
            .into_inner();
        daemon
            .client
            .link_create()
            .id(web.id)
            .body(NewLink {
                repository: "acme/gadgets".parse().unwrap(),
            })
            .send()
            .await
            .expect("link");

        let webs = daemon.client.pr_list().workspace(web.id).send().await;
        let widgets_of_web = daemon
            .client
            .pr_list()
            .workspace(web.id)
            .repository("acme/widgets".parse::<RepositoryName>().unwrap())
            .send()
            .await;

        let webs = webs.expect("list PRs").into_inner();
        let references: Vec<String> = webs.iter().map(|pr| pr.reference.to_string()).collect();
        assert_eq!(references, ["acme/gadgets#9"]);
        assert_eq!(widgets_of_web.expect("list PRs").into_inner(), []);
    }

    #[tokio::test]
    async fn should_keep_the_repository_and_its_pr_when_github_renames_it_200() {
        let github = MockServer::start_async().await;
        let mut before = mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = full_daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;

        before.delete();
        mock_search(&github, TOKEN, &[renamed(Pr::open(7))]);
        let status = sync(&daemon.client).await;

        assert_eq!(status.last_error, None);
        let prs = list(&daemon.client).await;
        assert_eq!(prs.len(), 1, "{prs:?}");
        assert_eq!(prs[0].reference.to_string(), "acme/gizmos#7");
        assert_eq!(
            prs[0].repository.id,
            RepositoryId::from(repository_id("acme/widgets"))
        );
        assert_eq!(prs[0].repository.name.to_string(), "acme/gizmos");
        assert_eq!(prs[0].repository.url, "https://github.com/acme/gizmos");
    }

    /// `acme/widgets` renamed to `acme/gizmos`, and another Repository then created as
    /// `acme/widgets` with a PR of the same number: two PRs, each with its own Repository.
    /// A PR keyed by its name would collide with the renamed one's stored name.
    #[tokio::test]
    async fn should_keep_two_prs_when_another_repository_takes_a_renamed_ones_name_200() {
        let github = MockServer::start_async().await;
        let mut before = mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = full_daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        let newcomer = Pr {
            repo_id: Some(repository_id("acme/widgets, the second")),
            title: "Start over",
            ..Pr::open(7)
        };

        before.delete();
        // The newcomer first, so it is stored while the renamed one still has the name.
        mock_search(&github, TOKEN, &[newcomer.clone(), renamed(Pr::open(7))]);
        let status = sync(&daemon.client).await;

        assert_eq!(status.last_error, None);
        let prs = list(&daemon.client).await;
        let kept: Vec<(String, u64, String)> = prs
            .iter()
            .map(|pr| {
                (
                    pr.reference.to_string(),
                    pr.repository.id.get(),
                    pr.title.clone(),
                )
            })
            .collect();
        assert_eq!(
            kept,
            [
                (
                    "acme/gizmos#7".to_owned(),
                    repository_id("acme/widgets"),
                    "Add PR sync".to_owned()
                ),
                (
                    "acme/widgets#7".to_owned(),
                    newcomer.repository_id(),
                    "Start over".to_owned()
                ),
            ]
        );
    }

    #[tokio::test]
    async fn should_store_a_pr_merged_after_its_repository_was_renamed_200() {
        let github = MockServer::start_async().await;
        let mut first = mock_list(&github, TOKEN, &[Pr::open(7)]);
        let mut updated = mock_updated_search(&github, TOKEN, &[]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        first.delete();

        updated.delete();
        let merged = Pr {
            state: "MERGED",
            ..renamed(Pr::open(7))
        };
        mock_updated_search(&github, TOKEN, &[merged]);
        sync(&daemon.client).await;

        let prs = (daemon.client.pr_list().finished(true).send().await)
            .expect("list PRs")
            .into_inner();
        assert_eq!(prs.len(), 1, "{prs:?}");
        assert_eq!(prs[0].reference.to_string(), "acme/gizmos#7");
        assert_eq!(prs[0].state, PrState::Merged);
        assert!(prs[0].finished);
    }

    #[tokio::test]
    async fn should_tell_websocket_clients_a_repository_was_created_then_renamed_200() {
        let github = MockServer::start_async().await;
        let daemon = full_daemon(&github, Some(TOKEN)).await;
        let mut ws = Ws::connect(&daemon).await;
        let id = repository_id("acme/widgets").to_string();
        let repository_changed = |frame| match frame {
            Frame::Message(ServerMessage::Changed {
                entity: Entity::Repository,
                id,
            }) => Some(id),
            _ => None,
        };

        let mut before = mock_search(&github, TOKEN, &[Pr::open(7)]);
        sync(&daemon.client).await;
        assert_eq!(ws.until(repository_changed).await, id);

        before.delete();
        mock_search(&github, TOKEN, &[renamed(Pr::open(7))]);
        sync(&daemon.client).await;
        assert_eq!(ws.until(repository_changed).await, id);
    }
}

mod post_v1_prs_close {
    use super::max_age::{mock_open, restarted_with_cutoff};
    use super::*;

    /// Closes `prs`, which must answer 200, and returns the results.
    async fn close(client: &Client, prs: Vec<PrRef>) -> Vec<PrCloseResult> {
        let response = client
            .prs_close()
            .body(PrClose { prs })
            .send()
            .await
            .expect("close");
        assert_eq!(response.status().as_u16(), 200);
        response.into_inner().results
    }

    /// A daemon that has synced `open`; every sync after is full.
    async fn synced(github: &MockServer, open: &[Pr]) -> TestDaemon {
        mock_search(github, TOKEN, open);
        let daemon = full_daemon(github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        assert_eq!(kept_prs(&daemon.client).await.len(), open.len());
        daemon
    }

    fn ok(pr: &PrRef) -> PrCloseResult {
        PrCloseResult {
            pr: pr.clone(),
            ok: true,
            error: None,
        }
    }

    fn failed(pr: &PrRef, error: &str) -> PrCloseResult {
        PrCloseResult {
            pr: pr.clone(),
            ok: false,
            error: Some(error.into()),
        }
    }

    #[tokio::test]
    async fn should_close_each_pr_and_finish_it_200() {
        let github = MockServer::start_async().await;
        let prs = [Pr::open(7), Pr::open(8)];
        let daemon = synced(&github, &prs).await;
        // Looked up in the order asked.
        let lookup = mock_node_ids(&github, &[prs[1].clone(), prs[0].clone()]);
        let closes: Vec<_> = prs.iter().map(|pr| mock_close(&github, pr)).collect();
        let [seven, eight] = [reference(&prs[0]), reference(&prs[1])];

        let results = close(&daemon.client, vec![eight.clone(), seven.clone()]).await;

        assert_eq!(results, [ok(&eight), ok(&seven)]);
        lookup.assert_calls(1);
        for mock in &closes {
            mock.assert_calls(1);
        }
        for number in [7, 8] {
            let pr = pr_numbered(&daemon.client, number).await;
            assert!(pr.finished);
            assert_eq!(pr.state, PrState::Closed);
        }
    }

    #[tokio::test]
    async fn should_close_a_pr_named_by_its_url_or_another_case_and_answer_as_asked_200() {
        let github = MockServer::start_async().await;
        let prs = [Pr::open(7), Pr::open(8)];
        let daemon = synced(&github, &prs).await;
        mock_node_ids(&github, &prs);
        for pr in &prs {
            mock_close(&github, pr);
        }
        let by_url: PrRef = "https://github.com/acme/widgets/pull/7/files"
            .parse()
            .unwrap();
        let by_case: PrRef = "ACME/Widgets#8".parse().unwrap();

        let results = close(&daemon.client, vec![by_url, by_case.clone()]).await;

        assert_eq!(results, [ok(&reference(&prs[0])), ok(&by_case)]);
        let listed = list(&daemon.client).await;
        assert!(
            listed.iter().all(|pr| pr.state == PrState::Closed),
            "{listed:?}"
        );
    }

    #[tokio::test]
    async fn should_close_a_pr_by_its_repositorys_new_name_after_a_rename_200() {
        let github = MockServer::start_async().await;
        let mut before = mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = full_daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        before.delete();
        let renamed = Pr {
            repo: "acme/gizmos",
            repo_id: Some(Pr::open(7).repository_id()),
            ..Pr::open(7)
        };
        mock_search(&github, TOKEN, std::slice::from_ref(&renamed));
        sync(&daemon.client).await;
        let lookup = mock_node_ids(&github, std::slice::from_ref(&renamed));
        mock_close(&github, &renamed);

        let results = close(&daemon.client, vec![reference(&renamed)]).await;

        assert_eq!(results, [ok(&reference(&renamed))]);
        lookup.assert_calls(1);
    }

    #[tokio::test]
    async fn should_tell_websocket_clients_the_closed_pr_changed_200() {
        let github = MockServer::start_async().await;
        let pr = Pr::open(7);
        let daemon = synced(&github, std::slice::from_ref(&pr)).await;
        mock_node_ids(&github, std::slice::from_ref(&pr));
        mock_close(&github, &pr);
        let mut ws = Ws::connect(&daemon).await;

        close(&daemon.client, vec![reference(&pr)]).await;

        ws.until(|frame| match frame {
            Frame::Message(ServerMessage::Changed {
                entity: Entity::PullRequest,
                id,
            }) if id == changed_id(&pr) => Some(()),
            _ => None,
        })
        .await;
    }

    #[tokio::test]
    async fn should_look_up_node_ids_25_to_a_request_200() {
        let github = MockServer::start_async().await;
        let prs: Vec<Pr> = (1..=30).map(Pr::open).collect();
        let daemon = synced(&github, &prs).await;
        let lookups: Vec<_> = prs
            .chunks(BATCH)
            .map(|batch| mock_node_ids(&github, batch))
            .collect();
        for pr in &prs {
            mock_close(&github, pr);
        }

        let results = close(&daemon.client, prs.iter().map(reference).collect()).await;

        assert!(results.iter().all(|result| result.ok), "{results:?}");
        assert_eq!(lookups.len(), 2);
        for lookup in &lookups {
            lookup.assert_calls(1);
        }
    }

    #[tokio::test]
    async fn should_keep_closing_after_github_refuses_one_and_give_its_message_200() {
        let github = MockServer::start_async().await;
        let prs = [Pr::open(7), Pr::open(8), Pr::open(9)];
        let daemon = synced(&github, &prs).await;
        mock_node_ids(&github, &prs);
        mock_close(&github, &prs[0]);
        let refusal = "octocat does not have the correct permissions to execute `ClosePullRequest`";
        mock_close_refused(&github, &prs[1], refusal);
        let last = mock_close(&github, &prs[2]);
        let refs: Vec<PrRef> = prs.iter().map(reference).collect();

        let results = close(&daemon.client, refs.clone()).await;

        assert_eq!(
            results,
            [ok(&refs[0]), failed(&refs[1], refusal), ok(&refs[2])]
        );
        last.assert_calls(1);
        let refused = pr_numbered(&daemon.client, 8).await;
        assert!(!refused.finished);
        assert_eq!(refused.state, PrState::Open);
    }

    #[tokio::test]
    async fn should_fail_a_close_that_timed_out_and_still_reads_open_with_why_200() {
        let github = MockServer::start_async().await;
        let pr = Pr::open(7);
        mock_search(&github, TOKEN, std::slice::from_ref(&pr));
        let daemon = timing_out_daemon(&github).await;
        sync(&daemon.client).await;
        mock_node_ids(&github, std::slice::from_ref(&pr));
        let id = pr.node_id();
        let close_mutation = github.mock(|when, then| {
            when.body_includes("closePullRequest(");
            then.status(200).delay(SLOW).json_body(serde_json::json!({
                "data": { "closePullRequest": { "pullRequest": { "id": id, "state": "CLOSED" } } }
            }));
        });
        github.mock(|when, then| {
            when.body_includes("... on PullRequest { state }");
            then.status(200)
                .json_body(serde_json::json!({ "data": { "node": { "state": "OPEN" } } }));
        });

        let results = close(&daemon.client, vec![reference(&pr)]).await;

        assert_eq!(results.len(), 1);
        assert!(!results[0].ok);
        assert!(
            says_timed_out(results[0].error.as_deref(), &github),
            "{:?}",
            results[0].error
        );
        close_mutation.assert_calls(1);
    }

    #[tokio::test]
    async fn should_fail_a_pr_github_does_not_show_with_githubs_message_200() {
        let github = MockServer::start_async().await;
        let pr = Pr::open(7);
        let daemon = synced(&github, std::slice::from_ref(&pr)).await;
        let message = "Could not resolve to a PullRequest with the number of 7.";
        github.mock(|when, then| {
            when.body_includes("fragment nodeId on PullRequest");
            then.status(200).json_body(serde_json::json!({
                "data": { "p0": { "pullRequest": null } },
                "errors": [ { "type": "NOT_FOUND", "path": [ "p0", "pullRequest" ], "message": message } ]
            }));
        });
        let any_close = github.mock(|when, then| {
            when.body_includes("closePullRequest(");
            then.status(500);
        });

        let results = close(&daemon.client, vec![reference(&pr)]).await;

        assert_eq!(results, [failed(&reference(&pr), message)]);
        any_close.assert_calls(0);
    }

    #[tokio::test]
    async fn should_fail_the_prs_of_a_lookup_that_fails_with_why_200() {
        let github = MockServer::start_async().await;
        let pr = Pr::open(7);
        let daemon = synced(&github, std::slice::from_ref(&pr)).await;
        github.mock(|when, then| {
            when.body_includes("fragment nodeId on PullRequest");
            then.status(502)
                .json_body(serde_json::json!({ "message": "Server Error" }));
        });

        let results = close(&daemon.client, vec![reference(&pr)]).await;

        assert_eq!(
            results,
            [failed(&reference(&pr), "GitHub answered 502: Server Error")]
        );
    }

    #[tokio::test]
    async fn should_fail_a_pr_that_is_not_stored_open_without_asking_github_200() {
        let github = MockServer::start_async().await;
        let merged = Pr {
            state: "MERGED",
            ..Pr::open(8)
        };
        let daemon = synced(&github, &[Pr::open(7), merged.clone()]).await;
        let lookup = github.mock(|when, then| {
            when.body_includes("fragment nodeId on PullRequest");
            then.status(500);
        });
        let unknown: PrRef = "acme/widgets#9".parse().unwrap();
        let elsewhere: PrRef = "acme/gadgets#7".parse().unwrap();

        let results = close(
            &daemon.client,
            vec![reference(&merged), unknown.clone(), elsewhere.clone()],
        )
        .await;

        assert_eq!(
            results,
            [
                failed(&reference(&merged), "Not an open PR"),
                failed(&unknown, "Not an open PR"),
                failed(&elsewhere, "Not an open PR"),
            ]
        );
        lookup.assert_calls(0);
    }

    #[tokio::test]
    async fn should_refuse_a_reference_that_names_no_pull_request_400() {
        let daemon = TestDaemon::start().await;
        let http = reqwest::Client::new();

        let response = http
            .post(format!("{}/v1/prs/close", daemon.base_url()))
            .bearer_auth(&daemon.info.token)
            .json(&serde_json::json!({ "prs": ["acme/widgets"] }))
            .send()
            .await
            .expect("the request");

        assert_eq!(response.status().as_u16(), 400);
        let body = response.text().await.expect("the body");
        assert!(body.contains("isn't a pull request"), "{body}");
    }

    #[tokio::test]
    async fn should_fail_every_pr_without_a_token_200() {
        let github = MockServer::start_async().await;
        let pr = Pr::open(7);
        let daemon = synced(&github, std::slice::from_ref(&pr)).await;
        let (_, dirs) = daemon.terminate();
        set_gh_token(dirs.path(), None);
        let daemon = TestDaemon::start_with_github(dirs, &github.url("/graphql")).await;
        let lookup = github.mock(|when, then| {
            when.body_includes("fragment nodeId on PullRequest");
            then.status(500);
        });

        let results = close(&daemon.client, vec![reference(&pr)]).await;

        assert_eq!(
            results,
            [failed(&reference(&pr), "Not signed in to GitHub")]
        );
        lookup.assert_calls(0);
    }

    #[tokio::test]
    async fn should_read_the_token_again_after_a_401_and_close_with_the_new_one_200() {
        let github = MockServer::start_async().await;
        let pr = Pr::open(7);
        let mut old = mock_search(&github, "gho_old", std::slice::from_ref(&pr));
        let daemon = daemon(&github, Some("gho_old")).await;
        sync(&daemon.client).await;

        old.delete();
        mock_unauthorized(&github, "gho_old");
        set_gh_token(daemon.root(), Some("gho_new"));
        let id = pr.node_id();
        let lookup = github.mock(|when, then| {
            when.header("authorization", "Bearer gho_new")
                .body_includes("fragment nodeId on PullRequest");
            then.status(200).json_body(serde_json::json!({
                "data": { "p0": { "pullRequest": { "id": id } } }
            }));
        });
        let id = pr.node_id();
        let close_mock = github.mock(|when, then| {
            when.header("authorization", "Bearer gho_new")
                .body_includes("closePullRequest(");
            then.status(200).json_body(serde_json::json!({
                "data": { "closePullRequest": { "pullRequest": { "id": id, "state": "CLOSED" } } }
            }));
        });

        let results = close(&daemon.client, vec![reference(&pr)]).await;

        assert_eq!(results, [ok(&reference(&pr))]);
        lookup.assert_calls(1);
        close_mock.assert_calls(1);
    }

    #[tokio::test]
    async fn should_not_close_with_the_gh_token_once_gh_logs_out_200() {
        let github = MockServer::start_async().await;
        let pr = Pr::open(7);
        let daemon = synced(&github, std::slice::from_ref(&pr)).await;
        let lookup = mock_node_ids(&github, std::slice::from_ref(&pr));

        set_gh_token(daemon.root(), None);
        let results = close(&daemon.client, vec![reference(&pr)]).await;

        assert_eq!(
            results,
            [failed(&reference(&pr), "Not signed in to GitHub")]
        );
        lookup.assert_calls(0);
    }

    #[tokio::test]
    async fn should_close_an_aged_out_pr_200() {
        let github = MockServer::start_async().await;
        let old = aged(8, 400);
        let daemon = restarted_with_cutoff(&github, std::slice::from_ref(&old)).await;
        mock_open(&github, std::slice::from_ref(&old));
        set_gh_token(daemon.root(), Some(TOKEN));
        sync(&daemon.client).await;
        assert!(pr_numbered(&daemon.client, 8).await.finished);
        mock_node_ids(&github, std::slice::from_ref(&old));
        mock_close(&github, &old);

        let results = close(&daemon.client, vec![reference(&old)]).await;

        assert_eq!(results, [ok(&reference(&old))]);
        assert_eq!(pr_numbered(&daemon.client, 8).await.state, PrState::Closed);
    }

    #[tokio::test]
    async fn should_not_fetch_a_closed_pr_by_number_on_the_next_sync_200() {
        let github = MockServer::start_async().await;
        let pr = Pr::open(7);
        mock_search(&github, TOKEN, std::slice::from_ref(&pr));
        let daemon = full_daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        mock_node_ids(&github, std::slice::from_ref(&pr));
        mock_close(&github, &pr);
        close(&daemon.client, vec![reference(&pr)]).await;

        github.reset_async().await;
        mock_list(&github, TOKEN, &[]);
        let fetch = mock_fetch(
            &github,
            &[Pr {
                state: "CLOSED",
                ..pr
            }],
        );
        sync(&daemon.client).await;

        fetch.assert_calls(0);
        let pr = pr_numbered(&daemon.client, 7).await;
        assert_eq!((pr.state, pr.finished), (PrState::Closed, true));
    }
}

mod finished {
    use super::max_age::{mock_open, restarted_with_cutoff};
    use super::*;

    /// Every pull request the daemon keeps, Finished ones included.
    async fn list_all(client: &Client) -> Vec<PullRequest> {
        let response = client.pr_list().finished(true).send().await;
        let response = response.expect("list PRs");
        assert_eq!(response.status().as_u16(), 200);
        response.into_inner()
    }

    /// The kept pull request numbered `number`, Finished or not.
    async fn kept(client: &Client, number: u32) -> Option<PullRequest> {
        (list_all(client).await)
            .into_iter()
            .find(|pr| pr.number == number)
    }

    /// The names of the Repositories agentdeck keeps.
    async fn repositories(client: &Client) -> Vec<String> {
        let response = client.repository_list().send().await;
        (response.expect("list Repositories").into_inner().iter())
            .map(|repository| repository.name.to_string())
            .collect()
    }

    /// The numbers `GET /v1/prs` lists without Finished ones.
    async fn unfinished(client: &Client) -> Vec<u32> {
        list(client).await.iter().map(|pr| pr.number).collect()
    }

    /// Temp dirs for a daemon whose `gh` prints [`TOKEN`], whose every sync is full, and whose
    /// Finished PRs are deleted once `keep_secs` have passed.
    fn dirs_keeping_for(keep_secs: &str) -> TempDir {
        let dirs = tempfile::tempdir().expect("a temp dir");
        set_gh_token(dirs.path(), Some(TOKEN));
        set_daemon_env(
            dirs.path(),
            &[
                ("AGENTDECK_PR_FULL_SYNC_SECS", "0"),
                ("AGENTDECK_PR_FINISHED_SECS", keep_secs),
            ],
        );
        dirs
    }

    /// `pr`, merged.
    fn merged(pr: Pr) -> Pr {
        Pr {
            state: "MERGED",
            ..pr
        }
    }

    /// `pr`, closed.
    fn closed(pr: Pr) -> Pr {
        Pr {
            state: "CLOSED",
            ..pr
        }
    }

    #[tokio::test]
    async fn should_finish_a_pr_github_shows_merged_and_leave_it_out_unless_asked_200() {
        let github = MockServer::start_async().await;
        let mut search = mock_search(&github, TOKEN, &[Pr::open(7), Pr::open(8)]);
        let daemon = full_daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        assert!(!kept(&daemon.client, 7).await.expect("PR 7").finished);

        search.delete();
        mock_search(&github, TOKEN, &[Pr::open(8)]);
        mock_fetch(&github, &[merged(Pr::open(7))]);
        sync(&daemon.client).await;

        let pr = kept(&daemon.client, 7).await.expect("PR 7, kept");
        assert!(pr.finished);
        assert_eq!(pr.state, PrState::Merged);
        assert_eq!(unfinished(&daemon.client).await, [8]);
        let not_asked = daemon.client.pr_list().finished(false).send().await;
        let numbers: Vec<u32> = (not_asked.expect("list PRs").into_inner().iter())
            .map(|pr| pr.number)
            .collect();
        assert_eq!(numbers, [8]);
    }

    #[tokio::test]
    async fn should_finish_a_pr_an_incremental_sync_finds_closed_200() {
        let github = MockServer::start_async().await;
        let mut first = mock_list(&github, TOKEN, &[Pr::open(7)]);
        let mut updated = mock_updated_search(&github, TOKEN, &[]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        first.delete();

        updated.delete();
        mock_updated_search(&github, TOKEN, &[closed(Pr::open(7))]);
        sync(&daemon.client).await;

        let pr = kept(&daemon.client, 7).await.expect("PR 7, kept");
        assert!(pr.finished);
        assert_eq!(pr.state, PrState::Closed);
        assert_eq!(unfinished(&daemon.client).await, Vec::<u32>::new());
    }

    #[tokio::test]
    async fn should_finish_an_open_pr_not_updated_within_the_cutoff_200() {
        let github = MockServer::start_async().await;
        let daemon = restarted_with_cutoff(&github, &[aged(7, 1), aged(8, 400)]).await;
        mock_open(&github, &[aged(7, 1), aged(8, 400)]);
        mock_fetch(&github, &[aged(7, 1)]);

        set_gh_token(daemon.root(), Some(TOKEN));
        sync(&daemon.client).await;

        let old = kept(&daemon.client, 8).await.expect("PR 8, kept");
        assert!(old.finished);
        // Nothing changed on GitHub: the PR is still open.
        assert_eq!(old.state, PrState::Open);
        assert!(!kept(&daemon.client, 7).await.expect("PR 7").finished);
        assert_eq!(unfinished(&daemon.client).await, [7]);
    }

    #[tokio::test]
    async fn should_finish_a_pr_closed_from_agentdeck_at_once_200() {
        let github = MockServer::start_async().await;
        let pr = Pr::open(7);
        mock_search(&github, TOKEN, std::slice::from_ref(&pr));
        let daemon = full_daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        mock_node_ids(&github, std::slice::from_ref(&pr));
        mock_close(&github, &pr);

        let results = daemon
            .client
            .prs_close()
            .body(PrClose {
                prs: vec![reference(&pr)],
            })
            .send()
            .await
            .expect("close")
            .into_inner()
            .results;

        assert!(results[0].ok);
        let pr = kept(&daemon.client, 7).await.expect("PR 7, kept");
        assert!(pr.finished);
        assert_eq!(pr.state, PrState::Closed);
    }

    #[tokio::test]
    async fn should_stop_finishing_a_pr_github_shows_open_again_200() {
        let github = MockServer::start_async().await;
        let mut first = mock_list(&github, TOKEN, &[Pr::open(7)]);
        let mut updated = mock_updated_search(&github, TOKEN, &[]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        first.delete();
        updated.delete();
        let mut closing = mock_updated_search(&github, TOKEN, &[closed(Pr::open(7))]);
        sync(&daemon.client).await;
        assert!(kept(&daemon.client, 7).await.expect("PR 7").finished);

        closing.delete();
        mock_updated_search(&github, TOKEN, &[Pr::open(7)]);
        sync(&daemon.client).await;

        let pr = kept(&daemon.client, 7).await.expect("PR 7, kept");
        assert!(!pr.finished);
        assert_eq!(pr.state, PrState::Open);
        assert_eq!(unfinished(&daemon.client).await, [7]);
    }

    #[tokio::test]
    async fn should_stop_finishing_an_aged_pr_github_updates_again_200() {
        let github = MockServer::start_async().await;
        let daemon = restarted_with_cutoff(&github, &[aged(8, 400)]).await;
        let mut stale = mock_open(&github, &[aged(8, 400)]);
        set_gh_token(daemon.root(), Some(TOKEN));
        sync(&daemon.client).await;
        assert!(kept(&daemon.client, 8).await.expect("PR 8").finished);

        stale.delete();
        mock_open(&github, &[aged(8, 0)]);
        mock_fetch(&github, &[aged(8, 0)]);
        sync(&daemon.client).await;

        assert!(!kept(&daemon.client, 8).await.expect("PR 8").finished);
    }

    #[tokio::test]
    async fn should_keep_a_finished_pr_within_its_window_200() {
        let github = MockServer::start_async().await;
        let mut search = mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = full_daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        search.delete();
        mock_search(&github, TOKEN, &[]);
        mock_fetch(&github, &[merged(Pr::open(7))]);

        sync(&daemon.client).await;
        sync(&daemon.client).await;

        assert!(kept(&daemon.client, 7).await.is_some());
    }

    #[tokio::test]
    async fn should_delete_a_pr_and_the_repository_nothing_else_references_after_its_window_200() {
        let github = MockServer::start_async().await;
        let gadget = Pr {
            repo: "acme/gadgets",
            ..Pr::open(3)
        };
        let mut search = mock_search(&github, TOKEN, &[Pr::open(7), Pr::open(8), gadget.clone()]);
        let daemon =
            TestDaemon::start_with_github(dirs_keeping_for("0"), &github.url("/graphql")).await;
        sync(&daemon.client).await;
        let mut ws = Ws::connect(&daemon).await;

        search.delete();
        mock_search(&github, TOKEN, &[Pr::open(8)]);
        mock_fetch(&github, &[closed(gadget), merged(Pr::open(7))]);
        sync(&daemon.client).await;

        ws.until(|frame| match frame {
            Frame::Message(ServerMessage::Changed {
                entity: Entity::Repository,
                id,
            }) if id == repository_id("acme/gadgets").to_string() => Some(()),
            _ => None,
        })
        .await;
        let kept: Vec<String> = (list_all(&daemon.client).await.iter())
            .map(|pr| pr.reference.to_string())
            .collect();
        assert_eq!(kept, ["acme/widgets#8"]);
        // acme/widgets keeps PR 8, so it stays; acme/gadgets has nothing left, so it goes.
        assert_eq!(repositories(&daemon.client).await, ["acme/widgets"]);
    }

    #[tokio::test]
    async fn should_keep_a_linked_repository_whose_prs_left_200() {
        let github = MockServer::start_async().await;
        let mut search = mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon =
            TestDaemon::start_with_github(dirs_keeping_for("0"), &github.url("/graphql")).await;
        sync(&daemon.client).await;
        let web = (daemon.client.workspace_create())
            .body(NewWorkspace {
                name: "web".into(),
                dir: None,
            })
            .send()
            .await
            .expect("create a workspace")
            .into_inner();
        (daemon.client.link_create())
            .id(web.id)
            .body(NewLink {
                repository: "acme/widgets".parse().expect("a Repository"),
            })
            .send()
            .await
            .expect("link");

        search.delete();
        mock_search(&github, TOKEN, &[]);
        mock_fetch(&github, &[merged(Pr::open(7))]);
        sync(&daemon.client).await;

        assert_eq!(kept(&daemon.client, 7).await, None);
        assert_eq!(repositories(&daemon.client).await, ["acme/widgets"]);
    }

    #[tokio::test]
    async fn should_keep_an_unreachable_pr_with_no_window_200() {
        let github = MockServer::start_async().await;
        let secret = Pr {
            repo: "acme/secret",
            ..aged(8, 400)
        };
        mock_search(&github, "gho_full", &[Pr::open(7), secret.clone()]);
        mock_search(&github, "github_pat_narrow", &[Pr::open(7)]);
        mock_hidden(&github, "github_pat_narrow", &secret);
        let dirs = dirs_keeping_for("0");
        set_gh_token(dirs.path(), Some("gho_full"));
        let daemon = TestDaemon::start_with_github(dirs, &github.url("/graphql")).await;
        sync(&daemon.client).await;
        set_gh_token(daemon.root(), Some("github_pat_narrow"));
        sync(&daemon.client).await;
        assert!(kept(&daemon.client, 8).await.expect("PR 8").unreachable);

        // Past the cutoff, with a window of none: an unreachable PR still stays.
        set_daemon_env(
            daemon.root(),
            &[
                ("AGENTDECK_PR_FULL_SYNC_SECS", "0"),
                ("AGENTDECK_PR_FINISHED_SECS", "0"),
                ("AGENTDECK_PR_MAX_AGE_DAYS", "90"),
            ],
        );
        let (_, dirs) = daemon.terminate();
        let daemon = TestDaemon::start_with_github(dirs, &github.url("/graphql")).await;
        sync(&daemon.client).await;
        sync(&daemon.client).await;

        let pr = kept(&daemon.client, 8).await.expect("PR 8, kept");
        assert!(pr.unreachable);
        assert!(!pr.finished);
        assert_eq!(pr.repository.name.to_string(), "acme/secret");
    }
}
