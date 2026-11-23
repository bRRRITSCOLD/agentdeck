//! `agentdeck_client::workspaces`: making a Workspace and editing it as the GUI's New
//! workspace and Edit workspace do, a name and the Repositories to link and unlink in one
//! request, against a real daemon with httpmock as GitHub. The GUI's fake API answers the same
//! way (spec §12).

use agentdeck_client::repositories::list;
use agentdeck_client::workspaces::{Change, create, update};
use agentdeck_core::{RepositoryId, RepositoryName};
use httpmock::MockServer;

use crate::support::TestDaemon;
use crate::support::github::{
    RestRepo, mock_rest_repo, mock_rest_repo_missing, mock_viewer, repository_id, set_gh_token,
};

const TOKEN: &str = "gho_test";
const WIDGETS: &str = "acme/widgets";
const GADGETS: &str = "acme/gadgets";

/// A daemon signed in to `github`, which shows widgets and gadgets but not `acme/nope`.
async fn daemon(github: &MockServer) -> TestDaemon {
    mock_viewer(github, TOKEN, "octocat", None);
    mock_rest_repo(github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
    mock_rest_repo(github, TOKEN, GADGETS, RestRepo::new(GADGETS));
    mock_rest_repo_missing(github, "acme/nope");
    let dirs = tempfile::tempdir().expect("a temp dir");
    set_gh_token(dirs.path(), Some(TOKEN));
    TestDaemon::start_with_github(dirs, &github.url("/graphql")).await
}

fn named(repository: &str) -> RepositoryName {
    repository.parse().expect("a Repository")
}

/// Each Repository's name with the Workspaces linking it.
async fn links(daemon: &TestDaemon) -> Vec<(String, usize)> {
    let repositories = list(&daemon.client).await.expect("list Repositories");
    (repositories.iter())
        .map(|r| (r.name.to_string(), r.workspaces.len()))
        .collect()
}

mod create {
    use super::*;

    #[tokio::test]
    async fn should_make_the_workspace_linking_what_was_picked() {
        let github = MockServer::start_async().await;
        let daemon = daemon(&github).await;

        let made = create(&daemon.client, "web".into(), vec![named(WIDGETS)])
            .await
            .expect("create");

        assert_eq!(made.name, "web");
        let linked = list(&daemon.client).await.expect("list Repositories");
        assert_eq!(linked[0].workspaces, [made.id]);
        assert_eq!(links(&daemon).await, [(WIDGETS.to_owned(), 1)]);
    }

    #[tokio::test]
    async fn should_fail_with_the_daemons_words_making_nothing() {
        let github = MockServer::start_async().await;
        let daemon = daemon(&github).await;

        let err = create(&daemon.client, "web".into(), vec![named("acme/nope")])
            .await
            .expect_err("a refusal");

        assert_eq!(err.code.as_deref(), Some("unknown_repository"));
        assert_eq!(
            err.message,
            "GitHub can't see acme/nope. Check the name or your token's access."
        );
        let workspaces = daemon.client.workspace_list().send().await;
        assert_eq!(workspaces.expect("list").into_inner(), []);
    }
}

mod update {
    use super::*;

    #[tokio::test]
    async fn should_rename_link_and_unlink_together() {
        let github = MockServer::start_async().await;
        let daemon = daemon(&github).await;
        let web = create(&daemon.client, "web".into(), vec![named(WIDGETS)])
            .await
            .expect("create");

        let change = Change {
            name: Some("storefront".into()),
            link: vec![named(GADGETS)],
            unlink: vec![RepositoryId::from(repository_id(WIDGETS))],
        };
        let updated = update(&daemon.client, web.id, change)
            .await
            .expect("update");

        assert_eq!(updated.name, "storefront");
        assert_eq!(links(&daemon).await, [(GADGETS.to_owned(), 1)]);
    }

    #[tokio::test]
    async fn should_fail_with_the_daemons_words_changing_nothing() {
        let github = MockServer::start_async().await;
        let daemon = daemon(&github).await;
        let web = create(&daemon.client, "web".into(), vec![named(WIDGETS)])
            .await
            .expect("create");

        let change = Change {
            name: Some("storefront".into()),
            link: vec![named("acme/nope")],
            unlink: vec![RepositoryId::from(repository_id(WIDGETS))],
        };
        let err = update(&daemon.client, web.id, change)
            .await
            .expect_err("a refusal");

        assert_eq!(err.code.as_deref(), Some("unknown_repository"));
        let kept = daemon.client.workspace_get().id(web.id).send().await;
        assert_eq!(kept.expect("get").into_inner().name, "web");
        assert_eq!(links(&daemon).await, [(WIDGETS.to_owned(), 1)]);
    }
}
