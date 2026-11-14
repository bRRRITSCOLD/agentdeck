//! PR sync: `/v1/prs` and the PR tasks it keeps (spec §8), against httpmock as GitHub and a
//! stub `gh` on `PATH`.

use std::path::Path;
use std::time::{Duration, Instant};

use agentdeck_client::Client;
use agentdeck_client::types::{
    GhLogin, NewWorkspace, PrSyncProgress, PrSyncStatus, Task, Workspace,
};
use agentdeck_core::{ChecksState, PrState, ReviewState, TaskKind, TaskStatus};
use httpmock::MockServer;

use agentdeck_core::ws::{Entity, ServerMessage};

use crate::support::git::git;
use crate::support::github::{
    Pr, mock_fetch, mock_full_search, mock_search, mock_search_page, mock_unauthorized,
    mock_updated_search, set_gh_token,
};
use crate::support::ws::{Frame, Ws};
use crate::support::{TestDaemon, set_daemon_env};

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
    async fn should_fetch_prs_that_left_the_search_50_at_a_time_200() {
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
            .chunks(50)
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
                .is_some_and(|err| err.contains("no oauth token found")),
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

    /// A daemon past its first, full sync, whose GitHub shows `open` in the full search and
    /// nothing in the incremental one until a test says otherwise.
    async fn synced(github: &MockServer, open: &[Pr]) -> TestDaemon {
        mock_full_search(github, TOKEN, open);
        let daemon = daemon(github, Some(TOKEN)).await;
        sync(&daemon.client).await;
        daemon
    }

    #[tokio::test]
    async fn should_ask_only_for_prs_updated_since_the_last_sync_200() {
        let github = MockServer::start_async().await;
        let full = mock_full_search(&github, TOKEN, &[Pr::open(7)]);
        let updated = mock_updated_search(&github, TOKEN, &[]);
        let daemon = daemon(&github, Some(TOKEN)).await;

        sync(&daemon.client).await;
        sync(&daemon.client).await;

        full.assert_calls(1);
        assert!(updated.calls() >= 1);
        assert_eq!(pr_tasks(&daemon.client).await.len(), 1);
    }

    #[tokio::test]
    async fn should_move_a_pr_merged_since_the_last_sync_to_done_without_a_fetch_200() {
        let github = MockServer::start_async().await;
        let mut updated = mock_updated_search(&github, TOKEN, &[]);
        let fetch = github.mock(|when, then| {
            when.body_includes("pullRequest(");
            then.status(500);
        });
        let daemon = synced(&github, &[Pr::open(7)]).await;

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
        let mut updated = mock_updated_search(&github, TOKEN, &[]);
        let daemon = synced(&github, &[Pr::open(7)]).await;

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
        mock_updated_search(&github, TOKEN, &[]);
        let fetch = mock_fetch(
            &github,
            &[Pr {
                checks: Some("SUCCESS"),
                ..Pr::open(7)
            }],
        );
        let daemon = synced(
            &github,
            &[Pr {
                checks: Some("PENDING"),
                ..Pr::open(7)
            }],
        )
        .await;

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
    async fn should_move_an_inbox_pr_into_a_workspace_once_a_clone_appears_200() {
        let github = MockServer::start_async().await;
        mock_updated_search(&github, TOKEN, &[]);
        let daemon = synced(&github, &[Pr::open(7)]).await;
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

mod progress {
    use super::*;

    /// A daemon whose first sync reads two pages: PR 1 at once, then PR 2 after `delay`.
    async fn two_pages(github: &MockServer, delay: Duration) -> TestDaemon {
        mock_search_page(
            github,
            TOKEN,
            &[Pr::open(1)],
            None,
            Some("c1"),
            2,
            Duration::ZERO,
        );
        mock_search_page(github, TOKEN, &[Pr::open(2)], Some("c1"), None, 2, delay);
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
                prs: 2
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
