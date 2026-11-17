//! PR sync: `/v1/prs` and the PR tasks it keeps (spec §8), against httpmock as GitHub and a
//! stub `gh` on `PATH`.

use std::path::Path;
use std::time::{Duration, Instant};

use agentdeck_client::Client;
use agentdeck_client::types::{
    GhLogin, NewTask, NewWorkspace, PrClose, PrCloseResult, PrSyncProgress, PrSyncStage,
    PrSyncStatus, Task, Workspace,
};
use agentdeck_core::{ChecksState, PrState, ReviewState, TaskKind, TaskStatus};
use httpmock::{Mock, MockServer};

use agentdeck_core::ws::{Entity, ServerMessage};

use crate::support::git::git;
use crate::support::github::{
    BATCH, Pr, days_ago, mock_close, mock_close_refused, mock_fetch, mock_list, mock_list_page,
    mock_node_ids, mock_search, mock_unauthorized, mock_updated_search, set_gh_token,
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

async fn pr_tasks(client: &Client) -> Vec<Task> {
    client
        .task_list()
        .kind(TaskKind::Pr)
        .send()
        .await
        .expect("list PR tasks")
        .into_inner()
}

async fn attach(client: &Client, dir: &Path) -> Workspace {
    client
        .workspace_create()
        .body(NewWorkspace {
            name: "ws".into(),
            dir: Some(dir.to_str().expect("a UTF-8 path").into()),
        })
        .send()
        .await
        .expect("attach a workspace")
        .into_inner()
}

/// A repo at `dir` with `origin` on GitHub's `repo` and a commit on `branch`. Nothing is
/// fetched: a remote URL is all matching reads.
fn clone_of(dir: &Path, repo: &str, branch: &str) {
    crate::support::git::init_with_commit(dir);
    git(
        dir,
        &[
            "remote",
            "add",
            "origin",
            &format!("git@github.com:{repo}.git"),
        ],
    );
    git(dir, &["branch", branch]);
}

/// Open PR `number`, last updated on GitHub `days` ago.
fn aged(number: i64, days: i64) -> Pr {
    Pr {
        updated_at: days_ago(days),
        ..Pr::open(number)
    }
}

/// The numbers of the stored PRs, in task order.
async fn numbers(client: &Client) -> Vec<i64> {
    let mut numbers: Vec<i64> = pr_tasks(client)
        .await
        .into_iter()
        .map(|task| task.pr.expect("the task's PR").number)
        .collect();
    numbers.sort_unstable();
    numbers
}

mod post_v1_prs_sync {
    use super::*;

    #[tokio::test]
    async fn should_put_a_new_pr_in_the_inbox_in_review_200() {
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
        let tasks = pr_tasks(&daemon.client).await;
        assert_eq!(tasks.len(), 1);
        let task = &tasks[0];
        assert_eq!(task.title, "Add PR sync");
        assert_eq!(task.status, TaskStatus::Review);
        assert_eq!(task.workspace_id, None);
        let stored = task.pr.as_ref().expect("the task's PR");
        assert_eq!(stored.repo, "acme/widgets");
        assert_eq!(stored.number, 7);
        assert_eq!(stored.url, pr.url());
        assert_eq!(stored.state, PrState::Open);
        assert!(stored.draft);
        assert_eq!(stored.head_ref, "feat/sync");
        assert_eq!(stored.checks, Some(ChecksState::Failure));
        assert_eq!(stored.review, Some(ReviewState::ChangesRequested));
    }

    #[tokio::test]
    async fn should_tell_websocket_clients_which_tasks_changed_200() {
        let github = MockServer::start_async().await;
        let daemon = daemon(&github, Some(TOKEN)).await;
        // Connected before GitHub shows the PR, so whichever sync finds it, this client hears.
        let mut ws = Ws::connect(&daemon).await;
        mock_search(&github, TOKEN, &[Pr::open(7)]);

        sync(&daemon.client).await;

        let changed = ws
            .until(|frame| match frame {
                Frame::Message(ServerMessage::Changed {
                    entity: Entity::Task,
                    id,
                }) => Some(id),
                _ => None,
            })
            .await;
        let tasks = pr_tasks(&daemon.client).await;
        assert_eq!(changed, tasks[0].id.to_string());
    }

    #[tokio::test]
    async fn should_put_a_pr_in_the_workspace_whose_clone_has_its_branch_200() {
        let github = MockServer::start_async().await;
        mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        let other = daemon.new_dir("other");
        clone_of(&other.join("widgets"), "acme/widgets", "main-only");
        let home = daemon.new_dir("home");
        clone_of(&home.join("src/widgets"), "Acme/Widgets", "feat/sync");
        let _other = attach(&daemon.client, &other).await;
        let home = attach(&daemon.client, &home).await;

        sync(&daemon.client).await;

        let tasks = pr_tasks(&daemon.client).await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].workspace_id, Some(home.id));
    }

    #[tokio::test]
    async fn should_put_a_pr_whose_branch_no_clone_has_in_the_oldest_workspace_with_its_repo_200() {
        let github = MockServer::start_async().await;
        mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        let gadgets = daemon.new_dir("gadgets");
        clone_of(&gadgets.join("gadgets"), "acme/gadgets", "feat/sync");
        let oldest = daemon.new_dir("oldest");
        clone_of(&oldest.join("widgets"), "acme/widgets", "main-only");
        let newer = daemon.new_dir("newer");
        clone_of(&newer.join("widgets"), "acme/widgets", "other-only");
        attach(&daemon.client, &gadgets).await;
        let oldest = attach(&daemon.client, &oldest).await;
        attach(&daemon.client, &newer).await;

        sync(&daemon.client).await;

        assert_eq!(
            pr_tasks(&daemon.client).await[0].workspace_id,
            Some(oldest.id)
        );
    }

    #[tokio::test]
    async fn should_leave_a_pr_in_the_inbox_when_no_clone_is_of_its_repo_200() {
        let github = MockServer::start_async().await;
        mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        let dir = daemon.new_dir("ws");
        clone_of(&dir.join("gadgets"), "acme/gadgets", "feat/sync");
        attach(&daemon.client, &dir).await;

        sync(&daemon.client).await;

        assert_eq!(pr_tasks(&daemon.client).await[0].workspace_id, None);
    }

    #[tokio::test]
    async fn should_move_an_inbox_pr_into_a_workspace_once_a_clone_appears_200() {
        let github = MockServer::start_async().await;
        mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        let dir = daemon.new_dir("ws");
        let workspace = attach(&daemon.client, &dir).await;
        sync(&daemon.client).await;

        clone_of(&dir.join("widgets"), "acme/widgets", "feat/sync");
        sync(&daemon.client).await;

        assert_eq!(
            pr_tasks(&daemon.client).await[0].workspace_id,
            Some(workspace.id)
        );
    }

    #[tokio::test]
    async fn should_update_checks_and_review_on_the_same_task_200() {
        let github = MockServer::start_async().await;
        let mut search = mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        let before = pr_tasks(&daemon.client).await.remove(0);

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

        let tasks = pr_tasks(&daemon.client).await;
        assert_eq!(tasks.len(), 1);
        let after = &tasks[0];
        assert_eq!(after.id, before.id);
        assert_eq!(after.status, TaskStatus::Review);
        let pr = after.pr.as_ref().expect("the task's PR");
        assert_eq!(pr.checks, Some(ChecksState::Success));
        assert_eq!(pr.review, Some(ReviewState::Approved));
        assert!(after.updated_at > before.updated_at);
    }

    #[tokio::test]
    async fn should_keep_the_status_the_user_picked_while_the_pr_stays_open_200() {
        let github = MockServer::start_async().await;
        mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        let task = pr_tasks(&daemon.client).await.remove(0);
        daemon
            .client
            .task_update()
            .id(task.id)
            .body_map(|patch| patch.status(Some(TaskStatus::Doing)))
            .send()
            .await
            .expect("update");

        sync(&daemon.client).await;

        assert_eq!(pr_tasks(&daemon.client).await[0].status, TaskStatus::Doing);
    }

    #[tokio::test]
    async fn should_move_a_pr_that_merged_out_of_the_search_to_done_200() {
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

        let task = pr_tasks(&daemon.client).await.remove(0);
        assert_eq!(task.status, TaskStatus::Done);
        assert!(task.done_at.is_some());
        assert_eq!(task.pr.expect("the task's PR").state, PrState::Merged);
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

        let tasks = pr_tasks(&daemon.client).await;
        assert_eq!(tasks.len(), 120);
        assert!(tasks.iter().all(|task| task.status == TaskStatus::Done));
        for fetch in &fetches {
            fetch.assert_calls(1);
        }
    }

    #[tokio::test]
    async fn should_close_a_pr_github_no_longer_shows_200() {
        let github = MockServer::start_async().await;
        let mut search = mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = full_daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;

        search.delete();
        mock_search(&github, TOKEN, &[]);
        github.mock(|when, then| {
            when.body_includes("pullRequest(");
            then.status(200).json_body(serde_json::json!({
                "data": { "p0": null },
                "errors": [ { "message": "Could not resolve to a Repository" } ]
            }));
        });
        sync(&daemon.client).await;

        let task = pr_tasks(&daemon.client).await.remove(0);
        assert_eq!(task.status, TaskStatus::Done);
        assert_eq!(task.pr.expect("the task's PR").state, PrState::Closed);
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
        assert_eq!(pr_tasks(&daemon.client).await.len(), 1);
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
        assert_eq!(pr_tasks(&daemon.client).await.len(), 1);
    }

    #[tokio::test]
    async fn should_report_a_github_failure_briefly_after_three_tries_and_keep_the_tasks_200() {
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
        assert_eq!(pr_tasks(&daemon.client).await.len(), 1);
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
        assert_eq!(pr_tasks(&daemon.client).await.len(), 1);
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
        assert_eq!(pr_tasks(&daemon.client).await.len(), 1);
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
        assert_eq!(pr_tasks(&daemon.client).await.len(), 1);
    }

    #[tokio::test]
    async fn should_move_a_pr_merged_since_the_last_sync_to_done_without_a_fetch_200() {
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

        let task = pr_tasks(&daemon.client).await.remove(0);
        assert_eq!(task.status, TaskStatus::Done);
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

        let numbers: Vec<i64> = pr_tasks(&daemon.client)
            .await
            .into_iter()
            .map(|task| task.pr.expect("the task's PR").number)
            .collect();
        assert_eq!(numbers, [7]);
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

        let pr = pr_tasks(&daemon.client)
            .await
            .remove(0)
            .pr
            .expect("the task's PR");
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

        let task = pr_tasks(&daemon.client).await.remove(0);
        assert_eq!(
            task.pr.expect("the task's PR").checks,
            Some(ChecksState::Failure)
        );
        assert!(fetch.calls() >= 1);
        let changed = ws
            .until(|frame| match frame {
                Frame::Message(ServerMessage::Changed {
                    entity: Entity::Task,
                    id,
                }) => Some(id),
                _ => None,
            })
            .await;
        assert_eq!(changed, task.id.to_string());
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

        let pr = pr_tasks(&daemon.client)
            .await
            .remove(0)
            .pr
            .expect("the task's PR");
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

    #[tokio::test]
    async fn should_move_an_inbox_pr_into_a_workspace_once_a_clone_appears_200() {
        let github = MockServer::start_async().await;
        let (daemon, _updated) = synced(&github, &[Pr::open(7)]).await;
        // It has no checks, so it is fetched again in case they started.
        mock_fetch(&github, &[Pr::open(7)]);
        let dir = daemon.new_dir("ws");
        let workspace = attach(&daemon.client, &dir).await;

        clone_of(&dir.join("widgets"), "acme/widgets", "feat/sync");
        sync(&daemon.client).await;

        assert_eq!(
            pr_tasks(&daemon.client).await[0].workspace_id,
            Some(workspace.id)
        );
    }
}

mod edited_by_the_user {
    use agentdeck_client::types::TaskPatch;
    use agentdeck_core::TaskPlace;

    use super::*;
    use crate::support::error_response;

    /// Moves `task` to `place`, as the user does in the task sheet.
    async fn move_to(client: &Client, task: &Task, place: TaskPlace) {
        client
            .task_update()
            .id(task.id)
            .body(TaskPatch {
                workspace_id: Some(place),
                ..TaskPatch::default()
            })
            .send()
            .await
            .expect("move the task");
    }

    /// A daemon with PR #7 matched by a clone to the workspace it returns, after a first sync.
    /// With `full`, every sync is a full one that lists #7 again; else later syncs are
    /// incremental and their search shows nothing, so only matching the inbox again sees #7.
    async fn matched(github: &MockServer, full: bool) -> (TestDaemon, Workspace) {
        let daemon = if full {
            mock_search(github, TOKEN, &[Pr::open(7)]);
            full_daemon(github, Some(TOKEN)).await
        } else {
            mock_list(github, TOKEN, &[Pr::open(7)]);
            mock_updated_search(github, TOKEN, &[]);
            daemon(github, Some(TOKEN)).await
        };
        let dir = daemon.new_dir("ws");
        clone_of(&dir.join("widgets"), "acme/widgets", "feat/sync");
        let workspace = attach(&daemon.client, &dir).await;
        sync(&daemon.client).await;
        assert_eq!(
            pr_tasks(&daemon.client).await[0].workspace_id,
            Some(workspace.id)
        );
        (daemon, workspace)
    }

    #[tokio::test]
    async fn should_keep_a_pr_task_the_user_moved_to_the_inbox_there_in_a_full_sync_200() {
        let github = MockServer::start_async().await;
        let (daemon, _) = matched(&github, true).await;
        let task = pr_tasks(&daemon.client).await.remove(0);

        move_to(&daemon.client, &task, TaskPlace::Inbox).await;
        sync(&daemon.client).await;

        assert_eq!(pr_tasks(&daemon.client).await[0].workspace_id, None);
    }

    #[tokio::test]
    async fn should_keep_a_pr_task_the_user_moved_to_the_inbox_there_in_an_incremental_sync_200() {
        let github = MockServer::start_async().await;
        let (daemon, _) = matched(&github, false).await;
        let task = pr_tasks(&daemon.client).await.remove(0);

        move_to(&daemon.client, &task, TaskPlace::Inbox).await;
        sync(&daemon.client).await;

        assert_eq!(pr_tasks(&daemon.client).await[0].workspace_id, None);
    }

    #[tokio::test]
    async fn should_keep_the_workspace_the_user_picked_200() {
        let github = MockServer::start_async().await;
        let (daemon, _) = matched(&github, true).await;
        let picked = attach(&daemon.client, &daemon.new_dir("picked")).await;
        let task = pr_tasks(&daemon.client).await.remove(0);

        move_to(&daemon.client, &task, TaskPlace::Workspace(picked.id)).await;
        sync(&daemon.client).await;

        assert_eq!(
            pr_tasks(&daemon.client).await[0].workspace_id,
            Some(picked.id)
        );
    }

    #[tokio::test]
    async fn should_refuse_another_title_for_a_pr_task_400() {
        let github = MockServer::start_async().await;
        mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        let task = pr_tasks(&daemon.client).await.remove(0);

        let (status, error) = error_response(
            daemon
                .client
                .task_update()
                .id(task.id)
                .body(TaskPatch {
                    title: Some("my name for it".into()),
                    ..TaskPatch::default()
                })
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("invalid_title"));
    }

    #[tokio::test]
    async fn should_take_a_pr_tasks_own_title_and_a_description_200() {
        let github = MockServer::start_async().await;
        mock_search(&github, TOKEN, &[Pr::open(7)]);
        let daemon = daemon(&github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        let task = pr_tasks(&daemon.client).await.remove(0);

        let edited = daemon
            .client
            .task_update()
            .id(task.id)
            .body(TaskPatch {
                title: Some(task.title.clone()),
                body: Some("Ship after the release.".into()),
                ..TaskPatch::default()
            })
            .send()
            .await
            .expect("edit")
            .into_inner();

        assert_eq!(edited.title, task.title);
        assert_eq!(edited.body, "Ship after the release.");
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
        let checks: Vec<_> = pr_tasks(&daemon.client)
            .await
            .into_iter()
            .map(|task| {
                let pr = task.pr.expect("the task's PR");
                (pr.number, pr.checks)
            })
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
        while pr_tasks(&daemon.client).await.is_empty() {
            assert!(Instant::now() < deadline, "no row while its details load");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
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
        assert_eq!(pr_tasks(&daemon.client).await.len(), open.len());
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

    pub(super) async fn task_of(client: &Client, number: i64) -> Task {
        pr_tasks(client)
            .await
            .into_iter()
            .find(|task| task.pr.as_ref().is_some_and(|pr| pr.number == number))
            .expect("the PR's task")
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
    async fn should_move_a_stored_pr_older_than_the_cutoff_to_done_200() {
        let github = MockServer::start_async().await;
        let daemon = restarted_with_cutoff(&github, &[aged(7, 1), aged(8, 400)]).await;
        mock_open(&github, &[aged(7, 1), aged(8, 400)]);
        // 7 has no checks, so it is fetched again in case they started.
        mock_fetch(&github, &[aged(7, 1)]);
        let old = task_of(&daemon.client, 8).await;
        let mut ws = Ws::connect(&daemon).await;

        set_gh_token(daemon.root(), Some(TOKEN));
        sync(&daemon.client).await;

        ws.until(|frame| match frame {
            Frame::Message(ServerMessage::Changed {
                entity: Entity::Task,
                id,
            }) if id == old.id.to_string() => Some(()),
            _ => None,
        })
        .await;
        let old = task_of(&daemon.client, 8).await;
        assert_eq!(old.status, TaskStatus::Done);
        assert!(old.done_at.is_some());
        // Nothing changed on GitHub: the PR is still open.
        assert_eq!(old.pr.expect("the task's PR").state, PrState::Open);
        assert_eq!(task_of(&daemon.client, 7).await.status, TaskStatus::Review);
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
    async fn should_move_an_aged_out_task_to_done_only_once_200() {
        let github = MockServer::start_async().await;
        let daemon = restarted_with_cutoff(&github, &[aged(8, 400)]).await;
        mock_open(&github, &[aged(8, 400)]);
        set_gh_token(daemon.root(), Some(TOKEN));
        sync(&daemon.client).await;
        let task = task_of(&daemon.client, 8).await;
        assert_eq!(task.status, TaskStatus::Done);

        daemon
            .client
            .task_update()
            .id(task.id)
            .body_map(|patch| patch.status(Some(TaskStatus::Review)))
            .send()
            .await
            .expect("update");
        sync(&daemon.client).await;

        assert_eq!(task_of(&daemon.client, 8).await.status, TaskStatus::Review);
    }

    #[tokio::test]
    async fn should_bring_back_an_aged_out_pr_github_updates_again_200() {
        let github = MockServer::start_async().await;
        let daemon = restarted_with_cutoff(&github, &[aged(8, 400)]).await;
        let mut list = mock_open(&github, &[aged(8, 400)]);
        set_gh_token(daemon.root(), Some(TOKEN));
        sync(&daemon.client).await;
        assert_eq!(task_of(&daemon.client, 8).await.status, TaskStatus::Done);

        list.delete();
        let fresh = aged(8, 0);
        mock_open(&github, std::slice::from_ref(&fresh));
        mock_fetch(&github, &[fresh]);
        sync(&daemon.client).await;

        assert_eq!(task_of(&daemon.client, 8).await.status, TaskStatus::Review);
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
        while pr_tasks(&daemon.client).await.is_empty() {
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
        assert_eq!(pr_tasks(&daemon.client).await.len(), 2);
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
        while pr_tasks(&daemon.client).await.is_empty() {
            assert!(Instant::now() < deadline, "no PR task 10 s after start");
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

mod post_v1_prs_close {
    use super::max_age::{mock_open, restarted_with_cutoff, task_of};
    use super::*;
    use agentdeck_core::TaskId;

    /// Closes `task_ids`, which must answer 200, and returns the results.
    async fn close(client: &Client, task_ids: Vec<TaskId>) -> Vec<PrCloseResult> {
        let response = client
            .prs_close()
            .body(PrClose { task_ids })
            .send()
            .await
            .expect("close");
        assert_eq!(response.status().as_u16(), 200);
        response.into_inner().results
    }

    /// A daemon that has synced `open` into PR tasks; every sync after is full.
    async fn synced(github: &MockServer, open: &[Pr]) -> TestDaemon {
        mock_search(github, TOKEN, open);
        let daemon = full_daemon(github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        assert_eq!(pr_tasks(&daemon.client).await.len(), open.len());
        daemon
    }

    fn ok(task: &Task) -> PrCloseResult {
        PrCloseResult {
            task_id: task.id,
            ok: true,
            error: None,
        }
    }

    fn failed(task_id: TaskId, error: &str) -> PrCloseResult {
        PrCloseResult {
            task_id,
            ok: false,
            error: Some(error.into()),
        }
    }

    #[tokio::test]
    async fn should_close_each_pr_and_move_its_task_to_done_200() {
        let github = MockServer::start_async().await;
        let prs = [Pr::open(7), Pr::open(8)];
        let daemon = synced(&github, &prs).await;
        // Looked up in the order asked.
        let lookup = mock_node_ids(&github, &[prs[1].clone(), prs[0].clone()]);
        let closes: Vec<_> = prs.iter().map(|pr| mock_close(&github, pr)).collect();
        let seven = task_of(&daemon.client, 7).await;
        let eight = task_of(&daemon.client, 8).await;

        let results = close(&daemon.client, vec![eight.id, seven.id]).await;

        assert_eq!(results, [ok(&eight), ok(&seven)]);
        lookup.assert_calls(1);
        for mock in &closes {
            mock.assert_calls(1);
        }
        for number in [7, 8] {
            let task = task_of(&daemon.client, number).await;
            assert_eq!(task.status, TaskStatus::Done);
            assert!(task.done_at.is_some());
            assert_eq!(task.pr.expect("the task's PR").state, PrState::Closed);
        }
    }

    #[tokio::test]
    async fn should_tell_websocket_clients_the_closed_task_changed_200() {
        let github = MockServer::start_async().await;
        let pr = Pr::open(7);
        let daemon = synced(&github, std::slice::from_ref(&pr)).await;
        mock_node_ids(&github, std::slice::from_ref(&pr));
        mock_close(&github, &pr);
        let task = task_of(&daemon.client, 7).await;
        let mut ws = Ws::connect(&daemon).await;

        close(&daemon.client, vec![task.id]).await;

        ws.until(|frame| match frame {
            Frame::Message(ServerMessage::Changed {
                entity: Entity::Task,
                id,
            }) if id == task.id.to_string() => Some(()),
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
        let mut tasks = pr_tasks(&daemon.client).await;
        tasks.sort_by_key(|task| task.pr.as_ref().expect("the task's PR").number);
        let ids: Vec<TaskId> = tasks.iter().map(|task| task.id).collect();

        let results = close(&daemon.client, ids).await;

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
        let tasks = [
            task_of(&daemon.client, 7).await,
            task_of(&daemon.client, 8).await,
            task_of(&daemon.client, 9).await,
        ];

        let results = close(&daemon.client, tasks.iter().map(|task| task.id).collect()).await;

        assert_eq!(
            results,
            [ok(&tasks[0]), failed(tasks[1].id, refusal), ok(&tasks[2])]
        );
        last.assert_calls(1);
        let refused = task_of(&daemon.client, 8).await;
        assert_eq!(refused.status, TaskStatus::Review);
        assert_eq!(refused.pr.expect("the task's PR").state, PrState::Open);
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
        let task = task_of(&daemon.client, 7).await;

        let results = close(&daemon.client, vec![task.id]).await;

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
        let task = task_of(&daemon.client, 7).await;

        let results = close(&daemon.client, vec![task.id]).await;

        assert_eq!(results, [failed(task.id, message)]);
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
        let task = task_of(&daemon.client, 7).await;

        let results = close(&daemon.client, vec![task.id]).await;

        assert_eq!(
            results,
            [failed(task.id, "GitHub answered 502: Server Error")]
        );
    }

    #[tokio::test]
    async fn should_fail_a_task_that_is_not_an_open_pr_without_asking_github_200() {
        let github = MockServer::start_async().await;
        let merged = Pr {
            state: "MERGED",
            ..Pr::open(8)
        };
        let daemon = synced(&github, &[Pr::open(7), merged]).await;
        let plain = daemon
            .client
            .task_create()
            .body(NewTask {
                body: None,
                status: None,
                title: "Write docs".into(),
                workspace_id: None,
            })
            .send()
            .await
            .expect("create a task")
            .into_inner();
        let lookup = github.mock(|when, then| {
            when.body_includes("fragment nodeId on PullRequest");
            then.status(500);
        });
        let merged = task_of(&daemon.client, 8).await;
        let unknown: TaskId = uuid::Uuid::now_v7().into();

        let results = close(&daemon.client, vec![plain.id, merged.id, unknown]).await;

        assert_eq!(
            results,
            [
                failed(plain.id, "Not an open PR"),
                failed(merged.id, "Not an open PR"),
                failed(unknown, "Not an open PR"),
            ]
        );
        lookup.assert_calls(0);
    }

    #[tokio::test]
    async fn should_fail_every_pr_without_a_token_200() {
        let github = MockServer::start_async().await;
        let daemon = synced(&github, &[Pr::open(7)]).await;
        let task = task_of(&daemon.client, 7).await;
        let (_, dirs) = daemon.terminate();
        set_gh_token(dirs.path(), None);
        let daemon = TestDaemon::start_with_github(dirs, &github.url("/graphql")).await;
        let lookup = github.mock(|when, then| {
            when.body_includes("fragment nodeId on PullRequest");
            then.status(500);
        });

        let results = close(&daemon.client, vec![task.id]).await;

        assert_eq!(results, [failed(task.id, "Not signed in to GitHub")]);
        lookup.assert_calls(0);
    }

    #[tokio::test]
    async fn should_read_the_token_again_after_a_401_and_close_with_the_new_one_200() {
        let github = MockServer::start_async().await;
        let pr = Pr::open(7);
        let mut old = mock_search(&github, "gho_old", std::slice::from_ref(&pr));
        let daemon = daemon(&github, Some("gho_old")).await;
        sync(&daemon.client).await;
        let task = task_of(&daemon.client, 7).await;

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

        let results = close(&daemon.client, vec![task.id]).await;

        assert_eq!(results, [ok(&task)]);
        lookup.assert_calls(1);
        close_mock.assert_calls(1);
    }

    #[tokio::test]
    async fn should_close_an_aged_out_pr_200() {
        let github = MockServer::start_async().await;
        let old = aged(8, 400);
        let daemon = restarted_with_cutoff(&github, std::slice::from_ref(&old)).await;
        mock_open(&github, std::slice::from_ref(&old));
        set_gh_token(daemon.root(), Some(TOKEN));
        sync(&daemon.client).await;
        let task = task_of(&daemon.client, 8).await;
        assert_eq!(task.status, TaskStatus::Done);
        mock_node_ids(&github, std::slice::from_ref(&old));
        mock_close(&github, &old);

        let results = close(&daemon.client, vec![task.id]).await;

        assert_eq!(results, [ok(&task)]);
        let task = task_of(&daemon.client, 8).await;
        assert_eq!(task.pr.expect("the task's PR").state, PrState::Closed);
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
        let task = task_of(&daemon.client, 7).await;
        close(&daemon.client, vec![task.id]).await;

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
        assert_eq!(task_of(&daemon.client, 7).await.status, TaskStatus::Done);
    }
}
