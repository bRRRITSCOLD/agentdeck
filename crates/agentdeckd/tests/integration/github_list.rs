//! The GitHub repository list for the Link picker: `GET /v1/github/repositories`,
//! `POST /v1/github/repositories/refresh` and `changed github_repositories` (spec §4, §5, §8),
//! with httpmock as GitHub's REST API answering `GET /user/repos` page by page.

use std::time::Duration;

use agentdeck_client::Client;
use agentdeck_client::types::{GithubListStatus, GithubRepositoryList, GithubTokenBody};
use agentdeck_core::ws::{Entity, ServerMessage};
use httpmock::MockServer;

use crate::support::github::{
    RestRepo, mock_search_repositories, mock_user_repos, mock_user_repos_failing,
    mock_user_repos_page, mock_viewer, set_gh_token, user_repos,
};
use crate::support::terminal::eventually;
use crate::support::ws::{Frame, Ws};
use crate::support::{TestDaemon, set_daemon_env};

const TOKEN: &str = "gho_test";

/// A daemon whose `gh` prints [`TOKEN`] and whose GitHub is `github`.
async fn signed_in(github: &MockServer) -> TestDaemon {
    mock_viewer(github, TOKEN, "octocat", None);
    let dirs = tempfile::tempdir().expect("a temp dir");
    set_gh_token(dirs.path(), Some(TOKEN));
    TestDaemon::start_with_github(dirs, &github.url("/graphql")).await
}

async fn list(client: &Client) -> GithubRepositoryList {
    let response = client.github_repository_list().send().await;
    let response = response.expect("read the GitHub repository list");
    assert_eq!(response.status().as_u16(), 200);
    response.into_inner()
}

async fn refresh(client: &Client) {
    let response = client.github_repository_refresh().send().await;
    let response = response.expect("refresh the GitHub repository list");
    assert_eq!(response.status().as_u16(), 204);
}

/// Waits until the list reads `status`, and answers it.
async fn until_status(client: &Client, status: GithubListStatus) -> GithubRepositoryList {
    eventually(&format!("the list {status:?}"), || async {
        let read = list(client).await;
        (read.status == status).then_some(read)
    })
    .await
}

fn names(list: &GithubRepositoryList) -> Vec<String> {
    (list.repositories.iter())
        .map(|r| r.name.to_string())
        .collect()
}

fn named(repos: &[RestRepo]) -> Vec<String> {
    repos.iter().map(|r| r.name.to_owned()).collect()
}

/// Waits for the next `changed github_repositories`.
async fn changed(ws: &mut Ws) {
    ws.until(|frame| match frame {
        Frame::Message(ServerMessage::Changed {
            entity: Entity::GithubRepositories,
            id,
        }) => {
            assert_eq!(id, "list");
            Some(())
        }
        _ => None,
    })
    .await;
}

mod get_v1_github_repositories {
    use super::*;

    #[tokio::test]
    async fn should_read_not_loaded_until_a_refresh_asks_200() {
        let github = MockServer::start_async().await;
        let pages = mock_user_repos(&github, TOKEN, &user_repos(3));
        let daemon = signed_in(&github).await;

        let read = list(&daemon.client).await;

        assert_eq!(read.status, GithubListStatus::NotLoaded);
        assert_eq!((read.loaded, read.total), (0, None));
        assert!(read.repositories.is_empty());
        assert_eq!(pages[0].calls_async().await, 0);
    }

    mod paging {
        use super::*;

        #[tokio::test]
        async fn should_list_every_page_in_githubs_order_once_loaded_200() {
            let github = MockServer::start_async().await;
            let repos = user_repos(250);
            let pages = mock_user_repos(&github, TOKEN, &repos);
            let daemon = signed_in(&github).await;

            refresh(&daemon.client).await;
            let read = until_status(&daemon.client, GithubListStatus::Loaded).await;

            assert_eq!(names(&read), named(&repos));
            assert_eq!((read.loaded, read.total), (250, Some(250)));
            assert_eq!(read.error, None);
            let private: Vec<bool> = read.repositories.iter().map(|r| r.private).collect();
            assert_eq!(private, repos.iter().map(|r| r.private).collect::<Vec<_>>());
            assert_eq!(read.repositories[1].id.get(), repos[1].id);
            assert_eq!(read.repositories[1].url, repos[1].url());
            for page in &pages {
                assert_eq!(page.calls_async().await, 1);
            }
        }

        #[tokio::test]
        async fn should_list_a_single_page_without_a_link_header_200() {
            let github = MockServer::start_async().await;
            let repos = user_repos(40);
            mock_user_repos(&github, TOKEN, &repos);
            let daemon = signed_in(&github).await;

            refresh(&daemon.client).await;
            let read = until_status(&daemon.client, GithubListStatus::Loaded).await;

            assert_eq!(names(&read), named(&repos));
            assert_eq!((read.loaded, read.total), (40, Some(40)));
        }

        #[tokio::test]
        async fn should_serve_page_one_while_the_rest_load_estimating_the_total_200() {
            let github = MockServer::start_async().await;
            let repos = user_repos(250);
            let slow = Duration::from_secs(2);
            mock_user_repos_page(&github, TOKEN, &repos, 1, Duration::ZERO);
            mock_user_repos_page(&github, TOKEN, &repos, 2, slow);
            mock_user_repos_page(&github, TOKEN, &repos, 3, slow);
            let daemon = signed_in(&github).await;

            refresh(&daemon.client).await;
            let read = eventually("page 1", || async {
                let read = list(&daemon.client).await;
                (read.loaded > 0).then_some(read)
            })
            .await;

            assert_eq!(read.status, GithubListStatus::Loading);
            assert_eq!(names(&read), named(&repos[..100]));
            assert_eq!((read.loaded, read.total), (100, Some(300)));
            let read = until_status(&daemon.client, GithubListStatus::Loaded).await;
            assert_eq!(read.loaded, 250);
        }

        #[tokio::test]
        async fn should_load_the_last_page_first_and_count_the_total_exactly_200() {
            let github = MockServer::start_async().await;
            let repos = user_repos(250);
            mock_user_repos_page(&github, TOKEN, &repos, 1, Duration::ZERO);
            mock_user_repos_page(&github, TOKEN, &repos, 2, Duration::from_secs(2));
            mock_user_repos_page(&github, TOKEN, &repos, 3, Duration::ZERO);
            let daemon = signed_in(&github).await;

            refresh(&daemon.client).await;
            let read = eventually("pages 1 and 3", || async {
                let read = list(&daemon.client).await;
                (read.loaded == 150).then_some(read)
            })
            .await;

            assert_eq!(read.status, GithubListStatus::Loading);
            assert_eq!(read.total, Some(250));
            let expected = [named(&repos[..100]), named(&repos[200..])].concat();
            assert_eq!(
                names(&read),
                expected,
                "in GitHub's order, page 2 still to come"
            );
        }

        #[tokio::test]
        async fn should_send_changed_as_the_load_begins_each_page_arrives_and_it_ends() {
            let github = MockServer::start_async().await;
            mock_user_repos(&github, TOKEN, &user_repos(250));
            let daemon = signed_in(&github).await;
            let mut ws = Ws::connect(&daemon).await;

            refresh(&daemon.client).await;
            for _ in 0..5 {
                changed(&mut ws).await;
            }

            assert_eq!(list(&daemon.client).await.status, GithubListStatus::Loaded);
        }
    }

    mod memory {
        use super::*;

        #[tokio::test]
        async fn should_keep_the_list_across_reads_without_asking_github_again_200() {
            let github = MockServer::start_async().await;
            let repos = user_repos(150);
            let pages = mock_user_repos(&github, TOKEN, &repos);
            let daemon = signed_in(&github).await;
            refresh(&daemon.client).await;
            until_status(&daemon.client, GithubListStatus::Loaded).await;

            let first = list(&daemon.client).await;
            let second = list(&daemon.client).await;

            assert_eq!(first, second);
            assert_eq!(names(&second), named(&repos));
            for page in &pages {
                assert_eq!(page.calls_async().await, 1);
            }
        }
    }

    mod search {
        use super::*;

        #[tokio::test]
        async fn should_never_call_githubs_repository_search_200() {
            let github = MockServer::start_async().await;
            let search = mock_search_repositories(&github);
            mock_user_repos(&github, TOKEN, &user_repos(250));
            let daemon = signed_in(&github).await;

            refresh(&daemon.client).await;
            until_status(&daemon.client, GithubListStatus::Loaded).await;
            refresh(&daemon.client).await;
            list(&daemon.client).await;

            assert_eq!(search.calls_async().await, 0);
        }
    }
}

mod post_v1_github_repositories_refresh {
    use super::*;

    #[tokio::test]
    async fn should_load_again_quietly_showing_the_list_before_until_it_ends_204() {
        let github = MockServer::start_async().await;
        let before = user_repos(150);
        let mut pages = mock_user_repos(&github, TOKEN, &before);
        let daemon = signed_in(&github).await;
        refresh(&daemon.client).await;
        until_status(&daemon.client, GithubListStatus::Loaded).await;
        for page in &mut pages {
            page.delete_async().await;
        }
        let after = user_repos(250);
        mock_user_repos_page(&github, TOKEN, &after, 1, Duration::ZERO);
        mock_user_repos_page(&github, TOKEN, &after, 2, Duration::from_secs(1));
        mock_user_repos_page(&github, TOKEN, &after, 3, Duration::ZERO);

        refresh(&daemon.client).await;
        let meanwhile = list(&daemon.client).await;

        assert_eq!(meanwhile.status, GithubListStatus::Loaded);
        assert_eq!(names(&meanwhile), named(&before));
        let read = eventually("the new list", || async {
            let read = list(&daemon.client).await;
            (read.loaded == 250).then_some(read)
        })
        .await;
        assert_eq!(read.status, GithubListStatus::Loaded);
        assert_eq!(names(&read), named(&after));
    }

    #[tokio::test]
    async fn should_leave_a_load_already_running_to_finish_204() {
        let github = MockServer::start_async().await;
        let repos = user_repos(150);
        let first = mock_user_repos_page(&github, TOKEN, &repos, 1, Duration::ZERO);
        mock_user_repos_page(&github, TOKEN, &repos, 2, Duration::from_secs(1));
        let daemon = signed_in(&github).await;

        refresh(&daemon.client).await;
        refresh(&daemon.client).await;
        until_status(&daemon.client, GithubListStatus::Loaded).await;

        assert_eq!(first.calls_async().await, 1);
    }

    /// A quiet refresh sends each held page's `ETag` as `If-None-Match` (page 1 excepted, whose
    /// `Link` header a `304` wouldn't carry), so a page GitHub answers `304` costs nothing
    /// against the rate limit and keeps the page held.
    mod conditional {
        use super::*;

        use crate::support::github::UserReposPage;

        /// A daemon holding `repos`, loaded once, and their page mocks.
        async fn holding<'a>(
            github: &'a MockServer,
            repos: &[RestRepo],
        ) -> (TestDaemon, Vec<UserReposPage<'a>>) {
            let pages = mock_user_repos(github, TOKEN, repos);
            let daemon = signed_in(github).await;
            refresh(&daemon.client).await;
            until_status(&daemon.client, GithubListStatus::Loaded).await;
            (daemon, pages)
        }

        /// GitHub's list changed to `after`: every page answers for it now.
        async fn changed_to<'a>(
            github: &'a MockServer,
            pages: &mut [UserReposPage<'a>],
            after: &[RestRepo],
        ) -> Vec<UserReposPage<'a>> {
            for page in pages {
                page.delete_async().await;
            }
            mock_user_repos(github, TOKEN, after)
        }

        /// Waits until `page` has answered `304` `count` times.
        async fn not_modified(page: &UserReposPage<'_>, count: usize) {
            eventually(&format!("{count} 304s"), || async {
                (page.not_modified_async().await == count).then_some(())
            })
            .await;
        }

        #[tokio::test]
        async fn should_send_each_held_pages_etag_and_keep_the_list_on_304s_204() {
            let github = MockServer::start_async().await;
            let repos = user_repos(250);
            let (daemon, pages) = holding(&github, &repos).await;
            let before = list(&daemon.client).await;
            let mut ws = Ws::connect(&daemon).await;

            refresh(&daemon.client).await;
            not_modified(&pages[1], 1).await;
            not_modified(&pages[2], 1).await;

            assert!(
                ws.quiet_for(Duration::from_millis(500)).await.is_none(),
                "no `changed` for a list that stayed the same"
            );
            assert_eq!(list(&daemon.client).await, before);
            assert_eq!(pages[0].calls_async().await, 2, "page 1 read again");
            assert_eq!(
                pages[0].not_modified_async().await,
                0,
                "page 1 unconditional"
            );
            for page in &pages[1..] {
                assert_eq!(page.calls_async().await, 2);
            }
        }

        #[tokio::test]
        async fn should_read_a_changed_page_whole_keeping_the_pages_that_answer_304_204() {
            let github = MockServer::start_async().await;
            let before = user_repos(250);
            let (daemon, mut pages) = holding(&github, &before).await;
            let mut after = before.clone();
            after[150].private = !after[150].private;
            let pages = changed_to(&github, &mut pages, &after).await;
            let mut ws = Ws::connect(&daemon).await;

            refresh(&daemon.client).await;
            changed(&mut ws).await;

            let read = list(&daemon.client).await;
            assert_eq!(read.status, GithubListStatus::Loaded);
            assert_eq!(names(&read), named(&after));
            assert_eq!(read.repositories[150].private, after[150].private);
            assert_eq!(pages[1].not_modified_async().await, 0, "page 2 changed");
            assert_eq!(pages[1].calls_async().await, 1);
            assert_eq!(pages[2].not_modified_async().await, 1, "page 3 kept");
        }

        mod page_shift {
            use super::*;

            #[tokio::test]
            async fn should_read_every_shifted_page_and_a_new_last_page_when_a_repo_is_added_204() {
                let github = MockServer::start_async().await;
                let before = user_repos(300);
                let (daemon, mut pages) = holding(&github, &before).await;
                let after = [vec![RestRepo::new("acme/aaa")], before.clone()].concat();
                let pages = changed_to(&github, &mut pages, &after).await;

                refresh(&daemon.client).await;
                let read = eventually("301 repositories", || async {
                    let read = list(&daemon.client).await;
                    (read.loaded == 301).then_some(read)
                })
                .await;

                assert_eq!(names(&read), named(&after));
                assert_eq!(read.total, Some(301));
                for page in &pages {
                    assert_eq!(page.calls_async().await, 1);
                    assert_eq!(page.not_modified_async().await, 0);
                }
            }

            #[tokio::test]
            async fn should_find_a_page_added_after_a_full_last_page_that_answers_304_204() {
                let github = MockServer::start_async().await;
                let before = user_repos(200);
                let (daemon, mut pages) = holding(&github, &before).await;
                let after = [before.clone(), vec![RestRepo::new("acme/zzz")]].concat();
                let pages = changed_to(&github, &mut pages, &after).await;

                refresh(&daemon.client).await;
                let read = eventually("201 repositories", || async {
                    let read = list(&daemon.client).await;
                    (read.loaded == 201).then_some(read)
                })
                .await;

                assert_eq!(names(&read), named(&after));
                assert_eq!(pages[1].not_modified_async().await, 1, "page 2 kept");
                assert_eq!(pages[2].calls_async().await, 1, "page 3 read");
            }

            #[tokio::test]
            async fn should_read_every_shifted_page_and_drop_the_last_when_a_repo_is_removed_204() {
                let github = MockServer::start_async().await;
                let before = user_repos(201);
                let (daemon, mut pages) = holding(&github, &before).await;
                let after = before[1..].to_vec();
                let pages = changed_to(&github, &mut pages, &after).await;

                refresh(&daemon.client).await;
                let read = eventually("200 repositories", || async {
                    let read = list(&daemon.client).await;
                    (read.loaded == 200).then_some(read)
                })
                .await;

                assert_eq!(names(&read), named(&after));
                assert_eq!(read.total, Some(200));
                assert_eq!(pages.len(), 2);
                for page in &pages {
                    assert_eq!(page.not_modified_async().await, 0);
                }
            }
        }
    }

    mod failure {
        use super::*;

        #[tokio::test]
        async fn should_fail_on_a_5xx_after_trying_three_times_then_load_on_retry_204() {
            let github = MockServer::start_async().await;
            let broken =
                mock_user_repos_failing(&github, 1, 502, "Server Error", &[("x-ignored", "1")]);
            let daemon = signed_in(&github).await;

            refresh(&daemon.client).await;
            let read = until_status(&daemon.client, GithubListStatus::Failed).await;

            assert_eq!(
                read.error.as_deref(),
                Some("GitHub answered 502: Server Error")
            );
            assert!(read.repositories.is_empty());
            assert_eq!(broken.calls_async().await, 3);
            broken.delete_async().await;
            let repos = user_repos(120);
            mock_user_repos(&github, TOKEN, &repos);
            refresh(&daemon.client).await;
            let read = until_status(&daemon.client, GithubListStatus::Loaded).await;
            assert_eq!(names(&read), named(&repos));
            assert_eq!(read.error, None);
        }

        #[tokio::test]
        async fn should_fail_at_once_when_github_limits_the_rate_403_204() {
            let github = MockServer::start_async().await;
            let limited = mock_user_repos_failing(
                &github,
                1,
                403,
                "API rate limit exceeded for user ID 1.",
                &[("x-ratelimit-remaining", "0")],
            );
            let daemon = signed_in(&github).await;

            refresh(&daemon.client).await;
            let read = until_status(&daemon.client, GithubListStatus::Failed).await;

            assert_eq!(
                read.error.as_deref(),
                Some("GitHub answered 403: API rate limit exceeded for user ID 1.")
            );
            assert_eq!(limited.calls_async().await, 1);
        }

        #[tokio::test]
        async fn should_fail_when_github_answers_too_many_requests_429_204() {
            let github = MockServer::start_async().await;
            mock_user_repos_failing(
                &github,
                1,
                429,
                "Too Many Requests",
                &[("retry-after", "60")],
            );
            let daemon = signed_in(&github).await;

            refresh(&daemon.client).await;
            let read = until_status(&daemon.client, GithubListStatus::Failed).await;

            assert_eq!(
                read.error.as_deref(),
                Some("GitHub answered 429: Too Many Requests")
            );
        }

        #[tokio::test]
        async fn should_fail_whole_when_a_later_page_fails_keeping_none_204() {
            let github = MockServer::start_async().await;
            let repos = user_repos(250);
            mock_user_repos_page(&github, TOKEN, &repos, 1, Duration::ZERO);
            mock_user_repos_page(&github, TOKEN, &repos, 3, Duration::ZERO);
            mock_user_repos_failing(&github, 2, 403, "Forbidden", &[]);
            let daemon = signed_in(&github).await;

            refresh(&daemon.client).await;
            let read = until_status(&daemon.client, GithubListStatus::Failed).await;

            assert_eq!(
                read.error.as_deref(),
                Some("GitHub answered 403: Forbidden")
            );
            assert_eq!((read.loaded, read.total), (0, None));
            assert!(read.repositories.is_empty());
        }

        #[tokio::test]
        async fn should_fail_when_github_rejects_the_token_401_204() {
            let github = MockServer::start_async().await;
            mock_user_repos_failing(&github, 1, 401, "Bad credentials", &[]);
            let daemon = signed_in(&github).await;

            refresh(&daemon.client).await;
            let read = until_status(&daemon.client, GithubListStatus::Failed).await;

            assert_eq!(read.error.as_deref(), Some("GitHub rejected the token"));
        }

        #[tokio::test]
        async fn should_keep_the_list_it_had_when_a_refresh_fails_saying_why_204() {
            let github = MockServer::start_async().await;
            let repos = user_repos(150);
            let mut pages = mock_user_repos(&github, TOKEN, &repos);
            let daemon = signed_in(&github).await;
            refresh(&daemon.client).await;
            until_status(&daemon.client, GithubListStatus::Loaded).await;
            for page in &mut pages {
                page.delete_async().await;
            }
            mock_user_repos_failing(&github, 1, 403, "API rate limit exceeded", &[]);
            let mut ws = Ws::connect(&daemon).await;

            refresh(&daemon.client).await;
            changed(&mut ws).await;

            let read = list(&daemon.client).await;
            assert_eq!(read.status, GithubListStatus::Loaded);
            assert_eq!(names(&read), named(&repos));
            assert_eq!(
                read.error.as_deref(),
                Some("GitHub answered 403: API rate limit exceeded")
            );
        }
    }

    mod signed_out {
        use super::*;

        #[tokio::test]
        async fn should_read_signed_out_without_asking_github_204() {
            let github = MockServer::start_async().await;
            let pages = mock_user_repos(&github, TOKEN, &user_repos(3));
            let daemon = TestDaemon::start_with_github(
                tempfile::tempdir().expect("a temp dir"),
                &github.url("/graphql"),
            )
            .await;

            refresh(&daemon.client).await;
            let read = until_status(&daemon.client, GithubListStatus::SignedOut).await;

            assert!(read.repositories.is_empty());
            assert_eq!(pages[0].calls_async().await, 0);
        }

        #[tokio::test]
        async fn should_load_the_new_sign_ins_list_when_the_token_changes_204() {
            const OTHER: &str = "gho_other";
            let github = MockServer::start_async().await;
            mock_viewer(&github, TOKEN, "octocat", None);
            mock_viewer(&github, OTHER, "hubot", None);
            let theirs = user_repos(30);
            mock_user_repos(&github, TOKEN, &user_repos(120));
            mock_user_repos(&github, OTHER, &theirs);
            let dirs = tempfile::tempdir().expect("a temp dir");
            set_gh_token(dirs.path(), Some(TOKEN));
            set_daemon_env(dirs.path(), &[("AGENTDECK_KEYRING", "memory")]);
            let daemon = TestDaemon::start_with_github(dirs, &github.url("/graphql")).await;
            refresh(&daemon.client).await;
            until_status(&daemon.client, GithubListStatus::Loaded).await;

            let saved = (daemon.client.github_token_put())
                .body(GithubTokenBody {
                    token: OTHER.into(),
                })
                .send()
                .await;
            saved.expect("save the other token");

            let read = eventually("hubot's list", || async {
                let read = list(&daemon.client).await;
                (read.status == GithubListStatus::Loaded && read.loaded == 30).then_some(read)
            })
            .await;
            assert_eq!(names(&read), named(&theirs));
        }

        #[tokio::test]
        async fn should_drop_the_held_pages_etags_when_the_token_changes_204() {
            const OTHER: &str = "gho_other";
            let github = MockServer::start_async().await;
            mock_viewer(&github, TOKEN, "octocat", None);
            mock_viewer(&github, OTHER, "hubot", None);
            let repos = user_repos(150);
            mock_user_repos(&github, TOKEN, &repos);
            let theirs = mock_user_repos(&github, OTHER, &repos);
            let dirs = tempfile::tempdir().expect("a temp dir");
            set_gh_token(dirs.path(), Some(TOKEN));
            set_daemon_env(dirs.path(), &[("AGENTDECK_KEYRING", "memory")]);
            let daemon = TestDaemon::start_with_github(dirs, &github.url("/graphql")).await;
            refresh(&daemon.client).await;
            until_status(&daemon.client, GithubListStatus::Loaded).await;

            let saved = (daemon.client.github_token_put())
                .body(GithubTokenBody {
                    token: OTHER.into(),
                })
                .send()
                .await;
            saved.expect("save the other token");

            eventually("hubot's page 2", || async {
                (theirs[1].calls_async().await == 1).then_some(())
            })
            .await;
            assert_eq!(
                theirs[1].not_modified_async().await,
                0,
                "no If-None-Match from the sign-in before"
            );
            let read = until_status(&daemon.client, GithubListStatus::Loaded).await;
            assert_eq!(names(&read), named(&repos));
        }
    }
}
