//! `agentdeck_client::repositories`: the Repositories and the Workspaces linking each, as the
//! GUI reads them for the PR alert mute rule, and the Links a Repository group's `…` menu makes
//! and takes away, against a real daemon with httpmock as GitHub. The GUI's fake API answers
//! the same way (spec §12).

use agentdeck_client::Client;
use agentdeck_client::repositories::{link, list, resolve, unlink};
use agentdeck_client::types::{NewLink, NewWorkspace, Repository};
use agentdeck_core::{RepositoryId, WorkspaceId};
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
            .body(NewLink::named(
                "acme/widgets".parse().expect("a Repository"),
            ))
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

/// A Workspace named `name`, linking nothing.
async fn workspace(client: &Client, name: &str) -> WorkspaceId {
    (client.workspace_create())
        .body(NewWorkspace {
            name: name.into(),
            link: Vec::new(),
        })
        .send()
        .await
        .expect("create a workspace")
        .into_inner()
        .id
}

/// `acme/widgets` as a client holds it, private.
fn widgets() -> Repository {
    Repository {
        id: RepositoryId::from(repository_id("acme/widgets")),
        name: "acme/widgets".parse().unwrap(),
        url: "https://github.com/acme/widgets".into(),
        private: true,
        archived: false,
    }
}

mod link {
    use super::*;

    #[tokio::test]
    async fn should_link_and_answer_the_repository_with_the_workspaces_linking_it() {
        let daemon = TestDaemon::start().await;
        let web = workspace(&daemon.client, "web").await;

        let linked = link(&daemon.client, web, &widgets()).await.expect("link");

        assert_eq!((linked.id, linked.private), (widgets().id, true));
        assert_eq!(linked.workspaces, [web]);
    }

    #[tokio::test]
    async fn should_link_again_after_an_unlink_deleted_the_repository_signed_out() {
        let daemon = TestDaemon::start().await;
        let web = workspace(&daemon.client, "web").await;
        link(&daemon.client, web, &widgets()).await.expect("link");
        unlink(&daemon.client, web, widgets().id)
            .await
            .expect("unlink");
        assert_eq!(list(&daemon.client).await.expect("list"), []);

        link(&daemon.client, web, &widgets())
            .await
            .expect("link again");

        let listed = list(&daemon.client).await.expect("list");
        let names: Vec<_> = (listed.iter())
            .map(|r| (r.name.to_string(), r.workspaces.clone()))
            .collect();
        assert_eq!(names, [("acme/widgets".to_owned(), vec![web])]);
    }
}

mod unlink {
    use super::*;

    #[tokio::test]
    async fn should_unlink_and_keep_a_repository_another_workspace_links() {
        let daemon = TestDaemon::start().await;
        let (web, api) = (
            workspace(&daemon.client, "web").await,
            workspace(&daemon.client, "api").await,
        );
        link(&daemon.client, web, &widgets()).await.expect("link");
        link(&daemon.client, api, &widgets()).await.expect("link");

        unlink(&daemon.client, web, widgets().id)
            .await
            .expect("unlink");

        let listed = list(&daemon.client).await.expect("list");
        assert_eq!(listed[0].workspaces, [api]);
    }

    #[tokio::test]
    async fn should_fail_with_the_daemons_words_for_a_repository_it_does_not_link() {
        let daemon = TestDaemon::start().await;
        let web = workspace(&daemon.client, "web").await;

        let err = unlink(&daemon.client, web, widgets().id)
            .await
            .expect_err("a refusal");

        assert_eq!(err.code.as_deref(), Some("not_found"));
    }
}

mod suggestions {
    use agentdeck_client::repositories::suggestions;
    use agentdeck_client::types::{NewPane, NewTab};
    use agentdeck_core::PaneKind;

    use super::*;
    use crate::support::git::git;

    #[tokio::test]
    async fn should_list_what_the_workspaces_panes_work_in_and_it_does_not_link() {
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", None);
        mock_rest_repo(
            &github,
            TOKEN,
            "acme/widgets",
            RestRepo::new("acme/widgets"),
        );
        let dirs = tempfile::tempdir().expect("a temp dir");
        set_gh_token(dirs.path(), Some(TOKEN));
        let daemon = TestDaemon::start_with_github(dirs, &github.url("/graphql")).await;
        let dir = daemon.new_dir("widgets");
        git(&dir, &["init", "--quiet"]);
        git(
            &dir,
            &["remote", "add", "origin", "git@github.com:acme/widgets.git"],
        );
        let web = (daemon.client.workspace_create())
            .body(NewWorkspace {
                name: "web".into(),
                link: Vec::new(),
            })
            .send()
            .await
            .expect("create a workspace")
            .into_inner();
        let tab = (daemon.client.tab_create())
            .id(web.id)
            .body(NewTab {
                title: "tab".into(),
                layout: None,
            })
            .send()
            .await
            .expect("create a tab")
            .into_inner();
        (daemon.client.pane_create())
            .body(NewPane {
                tab_id: tab.id,
                kind: PaneKind::Shell,
                agent: None,
                argv: Some(vec!["sleep".into(), "600".into()]),
                cwd: dir.to_str().expect("a UTF-8 path").to_owned(),
            })
            .send()
            .await
            .expect("spawn a pane");

        let suggested = suggestions(&daemon.client, web.id)
            .await
            .expect("list Suggestions");

        assert_eq!(
            suggested,
            [Repository {
                id: RepositoryId::from(repository_id("acme/widgets")),
                name: "acme/widgets".parse().unwrap(),
                url: "https://github.com/acme/widgets".into(),
                private: false,
                archived: false,
            }]
        );
    }

    #[tokio::test]
    async fn should_list_none_on_a_daemon_older_than_the_route() {
        let daemon = TestDaemon::start().await;
        let older = agentdeck_client::authenticated(
            &format!("{}/later", daemon.base_url()),
            &daemon.info.token,
        )
        .expect("a client");
        let workspace: WorkspaceId = "019a0000-0000-7000-8000-000000000001".parse().unwrap();

        let suggested = suggestions(&older, workspace).await.expect("no failure");

        assert_eq!(suggested, []);
    }

    #[tokio::test]
    async fn should_fail_with_the_daemons_words_for_a_workspace_that_does_not_exist() {
        let daemon = TestDaemon::start().await;
        let workspace: WorkspaceId = "019a0000-0000-7000-8000-000000000001".parse().unwrap();

        let err = suggestions(&daemon.client, workspace)
            .await
            .expect_err("a refusal");

        assert_eq!(err.code.as_deref(), Some("not_found"));
    }
}
