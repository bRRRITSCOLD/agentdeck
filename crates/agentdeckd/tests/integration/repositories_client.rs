//! `agentdeck_client::repositories`: the Repositories and the Workspaces linking each, as the
//! GUI reads them for the PR alert mute rule, against a real daemon with httpmock as GitHub.
//! The GUI's fake API answers the same way (spec §12).

use agentdeck_client::repositories::{list, resolve};
use agentdeck_client::types::{NewLink, NewWorkspace, Repository};
use agentdeck_core::RepositoryId;
use httpmock::MockServer;

use crate::support::TestDaemon;
use crate::support::github::{
    Pr, RestRepo, mock_rest_repo, mock_rest_repo_missing, mock_search, mock_viewer, repository_id,
    set_gh_token,
};

const TOKEN: &str = "gho_test";

mod list {
    use super::*;

    #[tokio::test]
    async fn should_list_each_repository_with_the_workspaces_linking_it_archived_ones_included() {
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", None);
        let gadget = Pr {
            repo: "acme/gadgets",
            ..Pr::open(3)
        };
        mock_search(&github, TOKEN, &[Pr::open(7), gadget]);
        let dirs = tempfile::tempdir().expect("a temp dir");
        set_gh_token(dirs.path(), Some(TOKEN));
        let daemon = TestDaemon::start_with_github(dirs, &github.url("/graphql")).await;
        daemon.client.prs_sync().send().await.expect("sync");
        let web = (daemon.client.workspace_create())
            .body(NewWorkspace {
                name: "web".into(),
                link: Vec::new(),
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
        (daemon.client.workspace_archive())
            .id(web.id)
            .send()
            .await
            .expect("archive");

        let repositories = list(&daemon.client).await.expect("list Repositories");

        let listed: Vec<(String, Vec<_>)> = (repositories.iter())
            .map(|r| (r.name.to_string(), r.workspaces.clone()))
            .collect();
        assert_eq!(
            listed,
            [
                ("acme/gadgets".to_owned(), vec![]),
                ("acme/widgets".to_owned(), vec![web.id]),
            ]
        );
    }

    #[tokio::test]
    async fn should_list_none_on_a_daemon_older_than_the_route() {
        let daemon = TestDaemon::start().await;
        // Under a prefix, every route the client knows is one the daemon lacks.
        let older = agentdeck_client::authenticated(
            &format!("{}/later", daemon.base_url()),
            &daemon.info.token,
        )
        .expect("a client");

        let repositories = list(&older).await.expect("no failure");

        assert_eq!(repositories, []);
    }

    #[tokio::test]
    async fn should_fail_with_the_daemons_words_when_it_refuses_the_token() {
        let daemon = TestDaemon::start().await;
        let stranger =
            agentdeck_client::authenticated(&daemon.base_url(), "not-the-token").expect("a client");

        let err = list(&stranger).await.expect_err("a refusal");

        assert_eq!(err.code.as_deref(), Some("unauthorized"));
    }
}

mod resolve {
    use super::*;

    /// A daemon signed in to `github`.
    async fn daemon(github: &MockServer) -> TestDaemon {
        mock_viewer(github, TOKEN, "octocat", None);
        let dirs = tempfile::tempdir().expect("a temp dir");
        set_gh_token(dirs.path(), Some(TOKEN));
        TestDaemon::start_with_github(dirs, &github.url("/graphql")).await
    }

    #[tokio::test]
    async fn should_answer_the_repository_github_shows_for_a_typed_url() {
        let github = MockServer::start_async().await;
        let private = RestRepo {
            private: true,
            ..RestRepo::new("acme/widgets")
        };
        mock_rest_repo(&github, TOKEN, "acme/widgets", private);
        let daemon = daemon(&github).await;
        let typed = "https://github.com/acme/widgets"
            .parse()
            .expect("a Repository");

        let found = resolve(&daemon.client, &typed).await.expect("resolve");

        assert_eq!(
            found,
            Repository {
                id: RepositoryId::from(repository_id("acme/widgets")),
                name: "acme/widgets".parse().unwrap(),
                url: "https://github.com/acme/widgets".into(),
                private: true,
                archived: false,
            }
        );
    }

    #[tokio::test]
    async fn should_fail_with_the_daemons_words_when_github_cannot_see_it() {
        let github = MockServer::start_async().await;
        mock_rest_repo_missing(&github, "acme/nope");
        let daemon = daemon(&github).await;

        let err = resolve(&daemon.client, &"acme/nope".parse().unwrap())
            .await
            .expect_err("a refusal");

        assert_eq!(err.code.as_deref(), Some("unknown_repository"));
        assert_eq!(
            err.message,
            "GitHub can't see acme/nope. Check the name or your token's access."
        );
    }
}
