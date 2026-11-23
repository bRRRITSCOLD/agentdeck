//! Repositories and their Links: `/v1/repositories` and `/v1/workspaces/{id}/links` (spec §3,
//! §4), with httpmock as GitHub's REST and GraphQL APIs and a stub `gh` on `PATH`.

use agentdeck_client::Client;
use agentdeck_client::types::{NewLink, NewWorkspace, Repository, RepositoryLinks, Workspace};
use agentdeck_core::ws::{Entity, ServerMessage};
use agentdeck_core::{RepositoryId, RepositoryName, WorkspaceId};
use httpmock::MockServer;

use crate::support::github::{
    Pr, RestRepo, mock_rest_repo, mock_rest_repo_missing, mock_search, mock_viewer, repository_id,
    set_gh_token,
};
use crate::support::ws::{Frame, Ws};
use crate::support::{TestDaemon, error_response};

const TOKEN: &str = "gho_test";

/// A daemon whose `gh` prints [`TOKEN`] and whose GitHub, REST and GraphQL, is `github`.
async fn daemon(github: &MockServer) -> TestDaemon {
    mock_viewer(github, TOKEN, "octocat", None);
    let dirs = tempfile::tempdir().expect("a temp dir");
    set_gh_token(dirs.path(), Some(TOKEN));
    TestDaemon::start_with_github(dirs, &github.url("/graphql")).await
}

/// A daemon like [`daemon`] that has synced `prs`, so their Repositories are kept.
async fn synced(github: &MockServer, prs: &[Pr]) -> TestDaemon {
    mock_search(github, TOKEN, prs);
    let daemon = daemon(github).await;
    let status = daemon.client.prs_sync().send().await.expect("sync");
    assert_eq!(status.into_inner().last_error, None);
    daemon
}

async fn workspace(client: &Client, name: &str) -> Workspace {
    client
        .workspace_create()
        .body(NewWorkspace {
            name: name.into(),
            link: Vec::new(),
        })
        .send()
        .await
        .expect("create a workspace")
        .into_inner()
}

async fn link(client: &Client, workspace: WorkspaceId, repository: &str) -> RepositoryLinks {
    let response = client
        .link_create()
        .id(workspace)
        .body(new_link(repository))
        .send()
        .await
        .expect("link");
    assert_eq!(response.status().as_u16(), 200);
    response.into_inner()
}

fn new_link(repository: &str) -> NewLink {
    NewLink::named(repository.parse().expect("a Repository"))
}

/// A link by GitHub's id, with what the client holds of `repo` under `name`: as Undo after an
/// unlink sends it.
fn held_link(repo: RestRepo, name: &str) -> NewLink {
    NewLink {
        repository: name.parse().expect("a Repository"),
        id: Some(RepositoryId::from(repo.id)),
        url: Some(format!("https://github.com/{name}")),
        private: Some(repo.private),
        archived: Some(false),
    }
}

async fn unlink(client: &Client, workspace: WorkspaceId, repository: RepositoryId) {
    let response = client
        .link_delete()
        .id(workspace)
        .repository(repository)
        .send()
        .await
        .expect("unlink");
    assert_eq!(response.status().as_u16(), 204);
}

async fn list(client: &Client) -> Vec<RepositoryLinks> {
    let response = client.repository_list().send().await;
    response.expect("list Repositories").into_inner()
}

async fn list_for(client: &Client, workspace: WorkspaceId) -> Vec<RepositoryLinks> {
    let response = client.repository_list().workspace(workspace).send().await;
    response
        .expect("list a workspace's Repositories")
        .into_inner()
}

fn names(repositories: &[RepositoryLinks]) -> Vec<String> {
    repositories.iter().map(|r| r.name.to_string()).collect()
}

/// The Repository `repo` as GitHub shows it, linked by `workspaces`.
fn listed(repo: RestRepo, workspaces: &[WorkspaceId]) -> RepositoryLinks {
    RepositoryLinks {
        id: RepositoryId::from(repo.id),
        name: repo.name.parse().unwrap(),
        url: repo.url(),
        private: repo.private,
        archived: false,
        unreachable: false,
        workspaces: workspaces.to_vec(),
    }
}

/// The next `changed` for a Repository or a Workspace, as `(entity, id)`.
async fn next_link_change(ws: &mut Ws) -> (Entity, String) {
    ws.until(|frame| match frame {
        Frame::Message(ServerMessage::Changed { entity, id })
            if matches!(entity, Entity::Repository | Entity::Workspace) =>
        {
            Some((entity, id))
        }
        _ => None,
    })
    .await
}

const WIDGETS: &str = "acme/widgets";
const GADGETS: &str = "acme/gadgets";

mod get_v1_repositories {
    use super::*;

    #[tokio::test]
    async fn should_list_none_before_anything_is_linked_or_synced_200() {
        let daemon = TestDaemon::start().await;

        assert_eq!(list(&daemon.client).await, []);
    }

    #[tokio::test]
    async fn should_list_each_repository_by_name_with_the_workspaces_linking_it_oldest_first_200() {
        let github = MockServer::start_async().await;
        mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
        let private_gadgets = RestRepo {
            private: true,
            ..RestRepo::new(GADGETS)
        };
        mock_rest_repo(&github, TOKEN, GADGETS, private_gadgets);
        let daemon = daemon(&github).await;
        let web = workspace(&daemon.client, "web").await;
        let api = workspace(&daemon.client, "api").await;
        link(&daemon.client, api.id, WIDGETS).await;
        link(&daemon.client, web.id, WIDGETS).await;
        link(&daemon.client, api.id, GADGETS).await;

        let repositories = list(&daemon.client).await;

        assert_eq!(
            repositories,
            [
                listed(private_gadgets, &[api.id]),
                listed(RestRepo::new(WIDGETS), &[web.id, api.id]),
            ]
        );
    }

    #[tokio::test]
    async fn should_list_a_repository_kept_for_a_pull_request_with_no_workspaces_200() {
        let github = MockServer::start_async().await;
        let daemon = synced(&github, &[Pr::open(7)]).await;

        let repositories = list(&daemon.client).await;

        assert_eq!(repositories, [listed(RestRepo::new(WIDGETS), &[])]);
    }

    #[tokio::test]
    async fn should_list_only_the_repositories_a_workspace_links_200() {
        let github = MockServer::start_async().await;
        let gadget = Pr {
            repo: GADGETS,
            ..Pr::open(9)
        };
        let daemon = synced(&github, &[Pr::open(7), gadget]).await;
        let web = workspace(&daemon.client, "web").await;
        let empty = workspace(&daemon.client, "empty").await;
        link(&daemon.client, web.id, GADGETS).await;

        let webs = list_for(&daemon.client, web.id).await;
        let none = list_for(&daemon.client, empty.id).await;

        assert_eq!(names(&webs), [GADGETS]);
        assert_eq!(webs[0].workspaces, [web.id]);
        assert_eq!(none, []);
    }

    #[tokio::test]
    async fn should_refuse_a_workspace_that_does_not_exist_400() {
        let daemon = TestDaemon::start().await;
        let gone = workspace(&daemon.client, "gone").await;
        daemon
            .client
            .workspace_delete()
            .id(gone.id)
            .send()
            .await
            .expect("delete");

        let (status, error) = error_response(
            daemon
                .client
                .repository_list()
                .workspace(gone.id)
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("unknown_workspace"));
    }
}

mod get_v1_repositories_resolve {
    use super::*;

    #[tokio::test]
    async fn should_answer_the_repository_github_finds_by_its_url_and_store_nothing_200() {
        let github = MockServer::start_async().await;
        let repo = RestRepo {
            private: true,
            ..RestRepo::new(WIDGETS)
        };
        let lookup = mock_rest_repo(&github, TOKEN, WIDGETS, repo);
        let daemon = daemon(&github).await;

        let response = daemon
            .client
            .repository_resolve()
            .repository(
                "https://github.com/acme/widgets"
                    .parse::<RepositoryName>()
                    .unwrap(),
            )
            .send()
            .await
            .expect("resolve");

        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response.into_inner(),
            Repository {
                id: RepositoryId::from(repo.id),
                name: WIDGETS.parse().unwrap(),
                url: repo.url(),
                private: true,
                archived: false,
            }
        );
        lookup.assert_calls(1);
        assert_eq!(list(&daemon.client).await, []);
    }

    #[tokio::test]
    async fn should_answer_a_renamed_repository_by_its_new_name_200() {
        let github = MockServer::start_async().await;
        let renamed = RestRepo {
            id: repository_id(WIDGETS),
            ..RestRepo::new("acme/gizmos")
        };
        mock_rest_repo(&github, TOKEN, WIDGETS, renamed);
        let daemon = synced(&github, &[Pr::open(7)]).await;

        let resolved = daemon
            .client
            .repository_resolve()
            .repository(WIDGETS.parse::<RepositoryName>().unwrap())
            .send()
            .await
            .expect("resolve")
            .into_inner();

        assert_eq!(resolved.name.to_string(), "acme/gizmos");
        assert_eq!(names(&list(&daemon.client).await), ["acme/gizmos"]);
    }

    #[tokio::test]
    async fn should_say_github_cannot_see_a_repository_400() {
        let github = MockServer::start_async().await;
        mock_rest_repo_missing(&github, "acme/nope");
        let daemon = daemon(&github).await;

        let (status, error) = error_response(
            daemon
                .client
                .repository_resolve()
                .repository("acme/nope".parse::<RepositoryName>().unwrap())
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("unknown_repository"));
        assert_eq!(
            error.message,
            "GitHub can't see acme/nope. Check the name or your token's access."
        );
    }

    #[tokio::test]
    async fn should_need_a_github_sign_in_409() {
        let daemon = TestDaemon::start().await;

        let (status, error) = error_response(
            daemon
                .client
                .repository_resolve()
                .repository(WIDGETS.parse::<RepositoryName>().unwrap())
                .send()
                .await,
        );

        assert_eq!(status, 409);
        assert_eq!(error.error_code.as_deref(), Some("not_signed_in"));
        assert_eq!(error.message, "Sign in to GitHub to look up acme/widgets.");
    }
}

mod post_v1_workspaces_links {
    use super::*;

    #[tokio::test]
    async fn should_link_a_repository_github_finds_and_answer_it_with_its_workspaces_200() {
        let github = MockServer::start_async().await;
        mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
        let daemon = daemon(&github).await;
        let web = workspace(&daemon.client, "web").await;

        let linked = link(&daemon.client, web.id, "https://github.com/acme/widgets").await;

        let expected = listed(RestRepo::new(WIDGETS), &[web.id]);
        assert_eq!(linked, expected);
        assert_eq!(list(&daemon.client).await, [expected]);
    }

    #[tokio::test]
    async fn should_link_a_repository_agentdeck_keeps_without_asking_github_200() {
        let github = MockServer::start_async().await;
        let daemon = synced(&github, &[Pr::open(7)]).await;
        let lookup = mock_rest_repo(&github, TOKEN, "ACME/Widgets", RestRepo::new(WIDGETS));
        let web = workspace(&daemon.client, "web").await;

        let linked = link(&daemon.client, web.id, "ACME/Widgets").await;

        assert_eq!(linked, listed(RestRepo::new(WIDGETS), &[web.id]));
        lookup.assert_calls(0);
    }

    mod by_id {
        use super::*;

        #[tokio::test]
        async fn should_store_a_deleted_repository_again_from_what_the_client_holds_signed_out_200()
         {
            let daemon = TestDaemon::start().await;
            let web = workspace(&daemon.client, "web").await;
            let private = RestRepo {
                private: true,
                ..RestRepo::new(WIDGETS)
            };
            (daemon.client.link_create())
                .id(web.id)
                .body(held_link(private, WIDGETS))
                .send()
                .await
                .expect("the first link by id");
            unlink(&daemon.client, web.id, RepositoryId::from(private.id)).await;
            assert_eq!(list(&daemon.client).await, [], "the unlink deleted it");
            let (_, by_name) = error_response(
                (daemon.client.link_create())
                    .id(web.id)
                    .body(new_link(WIDGETS))
                    .send()
                    .await,
            );
            assert_eq!(by_name.error_code.as_deref(), Some("not_signed_in"));

            let response = (daemon.client.link_create())
                .id(web.id)
                .body(held_link(private, WIDGETS))
                .send()
                .await
                .expect("link by id again");

            assert_eq!(response.status().as_u16(), 200);
            let expected = listed(private, &[web.id]);
            assert_eq!(response.into_inner(), expected);
            assert_eq!(list(&daemon.client).await, [expected]);
        }

        #[tokio::test]
        async fn should_link_the_repository_it_keeps_by_that_id_under_its_name_now_200() {
            let github = MockServer::start_async().await;
            let daemon = synced(&github, &[Pr::open(7)]).await;
            let web = workspace(&daemon.client, "web").await;

            let linked = (daemon.client.link_create())
                .id(web.id)
                .body(held_link(RestRepo::new(WIDGETS), "acme/old-widgets"))
                .send()
                .await
                .expect("link by id")
                .into_inner();

            assert_eq!(linked, listed(RestRepo::new(WIDGETS), &[web.id]));
        }

        #[tokio::test]
        async fn should_tell_websocket_clients_the_repository_then_the_workspace_changed_200() {
            let daemon = TestDaemon::start().await;
            let web = workspace(&daemon.client, "web").await;
            let mut ws = Ws::connect(&daemon).await;

            (daemon.client.link_create())
                .id(web.id)
                .body(held_link(RestRepo::new(WIDGETS), WIDGETS))
                .send()
                .await
                .expect("link by id");

            assert_eq!(
                next_link_change(&mut ws).await,
                (Entity::Repository, repository_id(WIDGETS).to_string())
            );
            assert_eq!(
                next_link_change(&mut ws).await,
                (Entity::Workspace, web.id.to_string())
            );
        }

        #[tokio::test]
        async fn should_answer_404_for_a_workspace_that_does_not_exist_and_store_nothing() {
            let daemon = TestDaemon::start().await;
            let gone = workspace(&daemon.client, "gone").await;
            (daemon.client.workspace_delete())
                .id(gone.id)
                .send()
                .await
                .expect("delete");

            let (status, error) = error_response(
                (daemon.client.link_create())
                    .id(gone.id)
                    .body(held_link(RestRepo::new(WIDGETS), WIDGETS))
                    .send()
                    .await,
            );

            assert_eq!(status, 404);
            assert_eq!(error.error_code.as_deref(), Some("not_found"));
            assert_eq!(list(&daemon.client).await, []);
        }
    }

    #[tokio::test]
    async fn should_refuse_a_repository_github_cannot_see_and_link_nothing_400() {
        let github = MockServer::start_async().await;
        mock_rest_repo_missing(&github, "acme/nope");
        let daemon = daemon(&github).await;
        let web = workspace(&daemon.client, "web").await;

        let (status, error) = error_response(
            daemon
                .client
                .link_create()
                .id(web.id)
                .body(new_link("acme/nope"))
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("unknown_repository"));
        assert_eq!(
            error.message,
            "GitHub can't see acme/nope. Check the name or your token's access."
        );
        assert_eq!(list(&daemon.client).await, []);
    }

    #[tokio::test]
    async fn should_link_a_pair_once_however_often_it_is_asked_200() {
        let github = MockServer::start_async().await;
        mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
        let daemon = daemon(&github).await;
        let web = workspace(&daemon.client, "web").await;

        link(&daemon.client, web.id, WIDGETS).await;
        let again = link(&daemon.client, web.id, WIDGETS).await;

        assert_eq!(again.workspaces, [web.id]);
        assert_eq!(list(&daemon.client).await.len(), 1);
    }

    #[tokio::test]
    async fn should_answer_404_for_a_workspace_that_does_not_exist() {
        let github = MockServer::start_async().await;
        let lookup = mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
        let daemon = daemon(&github).await;
        let gone = workspace(&daemon.client, "gone").await;
        daemon
            .client
            .workspace_delete()
            .id(gone.id)
            .send()
            .await
            .expect("delete");

        let (status, error) = error_response(
            daemon
                .client
                .link_create()
                .id(gone.id)
                .body(new_link(WIDGETS))
                .send()
                .await,
        );

        assert_eq!(status, 404);
        assert_eq!(error.error_code.as_deref(), Some("not_found"));
        lookup.assert_calls(0);
        assert_eq!(list(&daemon.client).await, []);
    }

    #[tokio::test]
    async fn should_tell_websocket_clients_the_repository_then_the_workspace_changed_200() {
        let github = MockServer::start_async().await;
        mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
        let daemon = daemon(&github).await;
        let web = workspace(&daemon.client, "web").await;
        let mut ws = Ws::connect(&daemon).await;

        link(&daemon.client, web.id, WIDGETS).await;

        assert_eq!(
            next_link_change(&mut ws).await,
            (Entity::Repository, repository_id(WIDGETS).to_string())
        );
        assert_eq!(
            next_link_change(&mut ws).await,
            (Entity::Workspace, web.id.to_string())
        );
    }
}

mod delete_v1_workspaces_links {
    use super::*;

    /// A daemon that knows `acme/widgets` from GitHub, and workspace `web` linking it.
    async fn web_linking_widgets(github: &MockServer) -> (TestDaemon, Workspace) {
        mock_rest_repo(github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
        let daemon = daemon(github).await;
        let web = workspace(&daemon.client, "web").await;
        link(&daemon.client, web.id, WIDGETS).await;
        (daemon, web)
    }

    fn widgets() -> RepositoryId {
        RepositoryId::from(repository_id(WIDGETS))
    }

    #[tokio::test]
    async fn should_delete_the_repository_with_its_last_link_204() {
        let github = MockServer::start_async().await;
        let (daemon, web) = web_linking_widgets(&github).await;

        unlink(&daemon.client, web.id, widgets()).await;

        assert_eq!(list(&daemon.client).await, []);
    }

    #[tokio::test]
    async fn should_keep_a_repository_another_workspace_links_204() {
        let github = MockServer::start_async().await;
        let (daemon, web) = web_linking_widgets(&github).await;
        let api = workspace(&daemon.client, "api").await;
        link(&daemon.client, api.id, WIDGETS).await;

        unlink(&daemon.client, web.id, widgets()).await;

        assert_eq!(
            list(&daemon.client).await,
            [listed(RestRepo::new(WIDGETS), &[api.id])]
        );
    }

    #[tokio::test]
    async fn should_keep_a_repository_one_of_your_pull_requests_belongs_to_204() {
        let github = MockServer::start_async().await;
        let daemon = synced(&github, &[Pr::open(7)]).await;
        let web = workspace(&daemon.client, "web").await;
        link(&daemon.client, web.id, WIDGETS).await;

        unlink(&daemon.client, web.id, widgets()).await;

        assert_eq!(
            list(&daemon.client).await,
            [listed(RestRepo::new(WIDGETS), &[])]
        );
    }

    #[tokio::test]
    async fn should_answer_404_for_a_repository_the_workspace_does_not_link() {
        let github = MockServer::start_async().await;
        let (daemon, _web) = web_linking_widgets(&github).await;
        let api = workspace(&daemon.client, "api").await;

        let (status, error) = error_response(
            daemon
                .client
                .link_delete()
                .id(api.id)
                .repository(widgets())
                .send()
                .await,
        );

        assert_eq!(status, 404);
        assert_eq!(error.error_code.as_deref(), Some("not_found"));
        assert_eq!(list(&daemon.client).await.len(), 1);
    }

    #[tokio::test]
    async fn should_tell_websocket_clients_the_repository_then_the_workspace_changed_204() {
        let github = MockServer::start_async().await;
        let (daemon, web) = web_linking_widgets(&github).await;
        let mut ws = Ws::connect(&daemon).await;

        unlink(&daemon.client, web.id, widgets()).await;

        assert_eq!(
            next_link_change(&mut ws).await,
            (Entity::Repository, widgets().to_string())
        );
        assert_eq!(
            next_link_change(&mut ws).await,
            (Entity::Workspace, web.id.to_string())
        );
    }
}

mod delete_v1_workspaces {
    use super::*;

    #[tokio::test]
    async fn should_delete_its_links_and_each_repository_nothing_else_names_204() {
        let github = MockServer::start_async().await;
        mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
        mock_rest_repo(&github, TOKEN, GADGETS, RestRepo::new(GADGETS));
        let daemon = daemon(&github).await;
        let web = workspace(&daemon.client, "web").await;
        let api = workspace(&daemon.client, "api").await;
        link(&daemon.client, web.id, WIDGETS).await;
        link(&daemon.client, web.id, GADGETS).await;
        link(&daemon.client, api.id, GADGETS).await;
        let mut ws = Ws::connect(&daemon).await;

        daemon
            .client
            .workspace_delete()
            .id(web.id)
            .send()
            .await
            .expect("delete");

        assert_eq!(
            list(&daemon.client).await,
            [listed(RestRepo::new(GADGETS), &[api.id])]
        );
        let mut changed = vec![
            next_link_change(&mut ws).await,
            next_link_change(&mut ws).await,
        ];
        changed.sort_by(|a, b| a.1.cmp(&b.1));
        let mut expected = vec![
            (Entity::Repository, repository_id(WIDGETS).to_string()),
            (Entity::Repository, repository_id(GADGETS).to_string()),
        ];
        expected.sort_by(|a, b| a.1.cmp(&b.1));
        assert_eq!(changed, expected);
        assert_eq!(
            next_link_change(&mut ws).await,
            (Entity::Workspace, web.id.to_string())
        );
    }
}

/// `name` and its Repositories as the daemon holds them now: its name, then the names of the
/// Repositories it links.
async fn workspace_now(client: &Client, workspace: WorkspaceId) -> (String, Vec<String>) {
    let read = client.workspace_get().id(workspace).send().await;
    let name = read.expect("read the workspace").into_inner().name;
    (name, names(&list_for(client, workspace).await))
}

fn repository_names(repositories: &[&str]) -> Vec<RepositoryName> {
    let parsed = repositories
        .iter()
        .map(|r| r.parse().expect("a Repository"));
    parsed.collect()
}

mod post_v1_workspaces {
    use super::*;

    fn linking(name: &str, repositories: &[&str]) -> NewWorkspace {
        NewWorkspace {
            name: name.into(),
            link: repository_names(repositories),
        }
    }

    #[tokio::test]
    async fn should_link_the_repositories_asked_for_known_or_looked_up_on_github_201() {
        let github = MockServer::start_async().await;
        let daemon = synced(&github, &[Pr::open(7)]).await;
        let known = mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
        mock_rest_repo(&github, TOKEN, GADGETS, RestRepo::new(GADGETS));

        let created = (daemon.client.workspace_create())
            .body(linking("web", &[WIDGETS, GADGETS]))
            .send()
            .await;

        let created = created.expect("create");
        assert_eq!(created.status().as_u16(), 201);
        let web = created.into_inner();
        assert_eq!(
            list_for(&daemon.client, web.id).await,
            [
                listed(RestRepo::new(GADGETS), &[web.id]),
                listed(RestRepo::new(WIDGETS), &[web.id]),
            ]
        );
        known.assert_calls(0);
    }

    #[tokio::test]
    async fn should_make_nothing_when_github_cannot_see_a_repository_400() {
        let github = MockServer::start_async().await;
        mock_rest_repo(&github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
        mock_rest_repo_missing(&github, "acme/nope");
        let daemon = daemon(&github).await;

        let (status, error) = error_response(
            (daemon.client.workspace_create())
                .body(linking("web", &[WIDGETS, "acme/nope"]))
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("unknown_repository"));
        assert_eq!(
            error.message,
            "GitHub can't see acme/nope. Check the name or your token's access."
        );
        let workspaces = daemon.client.workspace_list().send().await;
        assert_eq!(workspaces.expect("list").into_inner(), []);
        assert_eq!(list(&daemon.client).await, []);
    }
}

mod patch_v1_workspaces_id {
    use super::*;

    /// A daemon whose GitHub shows `acme/widgets` and `acme/gadgets`, and workspace `web`
    /// linking widgets.
    async fn web_linking_widgets(github: &MockServer) -> (TestDaemon, Workspace) {
        mock_rest_repo(github, TOKEN, WIDGETS, RestRepo::new(WIDGETS));
        mock_rest_repo(github, TOKEN, GADGETS, RestRepo::new(GADGETS));
        let daemon = daemon(github).await;
        let web = workspace(&daemon.client, "web").await;
        link(&daemon.client, web.id, WIDGETS).await;
        (daemon, web)
    }

    fn widgets() -> RepositoryId {
        RepositoryId::from(repository_id(WIDGETS))
    }

    #[tokio::test]
    async fn should_rename_link_and_unlink_in_one_request_200() {
        let github = MockServer::start_async().await;
        let (daemon, web) = web_linking_widgets(&github).await;

        let updated = daemon
            .client
            .workspace_update()
            .id(web.id)
            .body_map(|patch| {
                patch
                    .name(Some("storefront".to_owned()))
                    .link(repository_names(&[GADGETS]))
                    .unlink(vec![widgets()])
            })
            .send()
            .await
            .expect("update")
            .into_inner();

        assert_eq!(updated.name, "storefront");
        assert_eq!(
            workspace_now(&daemon.client, web.id).await,
            ("storefront".to_owned(), vec![GADGETS.to_owned()])
        );
        // Nothing else names widgets, so it went with its Link.
        assert_eq!(names(&list(&daemon.client).await), [GADGETS]);
    }

    #[tokio::test]
    async fn should_change_nothing_when_github_cannot_see_a_repository_to_link_400() {
        let github = MockServer::start_async().await;
        let (daemon, web) = web_linking_widgets(&github).await;
        mock_rest_repo_missing(&github, "acme/nope");

        let (status, error) = error_response(
            daemon
                .client
                .workspace_update()
                .id(web.id)
                .body_map(|patch| {
                    patch
                        .name(Some("storefront".to_owned()))
                        .link(repository_names(&[GADGETS, "acme/nope"]))
                        .unlink(vec![widgets()])
                })
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("unknown_repository"));
        assert_eq!(
            workspace_now(&daemon.client, web.id).await,
            ("web".to_owned(), vec![WIDGETS.to_owned()])
        );
        assert_eq!(names(&list(&daemon.client).await), [WIDGETS]);
    }

    #[tokio::test]
    async fn should_change_nothing_when_the_name_is_blank_400() {
        let github = MockServer::start_async().await;
        let (daemon, web) = web_linking_widgets(&github).await;

        let (status, error) = error_response(
            daemon
                .client
                .workspace_update()
                .id(web.id)
                .body_map(|patch| {
                    patch
                        .name(Some(" ".to_owned()))
                        .link(repository_names(&[GADGETS]))
                        .unlink(vec![widgets()])
                })
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("invalid_name"));
        assert_eq!(
            workspace_now(&daemon.client, web.id).await,
            ("web".to_owned(), vec![WIDGETS.to_owned()])
        );
    }

    #[tokio::test]
    async fn should_refuse_to_link_and_unlink_one_repository_at_once_400() {
        let github = MockServer::start_async().await;
        let (daemon, web) = web_linking_widgets(&github).await;

        let (status, error) = error_response(
            daemon
                .client
                .workspace_update()
                .id(web.id)
                .body_map(|patch| {
                    patch
                        .link(repository_names(&["ACME/Widgets"]))
                        .unlink(vec![widgets()])
                })
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("invalid_links"));
        assert_eq!(
            error.message,
            "acme/widgets can't be linked and unlinked at once"
        );
        assert_eq!(names(&list_for(&daemon.client, web.id).await), [WIDGETS]);
    }

    #[tokio::test]
    async fn should_leave_a_repository_it_does_not_link_as_it_is_200() {
        let github = MockServer::start_async().await;
        let (daemon, web) = web_linking_widgets(&github).await;
        let api = workspace(&daemon.client, "api").await;

        let response = daemon
            .client
            .workspace_update()
            .id(api.id)
            .body_map(|patch| patch.unlink(vec![widgets()]))
            .send()
            .await
            .expect("update");

        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            list(&daemon.client).await,
            [listed(RestRepo::new(WIDGETS), &[web.id])]
        );
    }

    #[tokio::test]
    async fn should_link_an_archived_workspace_200() {
        let github = MockServer::start_async().await;
        let (daemon, web) = web_linking_widgets(&github).await;
        let archived = daemon.client.workspace_archive().id(web.id).send().await;
        archived.expect("archive");

        daemon
            .client
            .workspace_update()
            .id(web.id)
            .body_map(|patch| patch.link(repository_names(&[GADGETS])))
            .send()
            .await
            .expect("update");

        assert_eq!(
            names(&list_for(&daemon.client, web.id).await),
            [GADGETS, WIDGETS]
        );
    }

    #[tokio::test]
    async fn should_tell_websocket_clients_each_repository_then_the_workspace_changed_200() {
        let github = MockServer::start_async().await;
        let (daemon, web) = web_linking_widgets(&github).await;
        let mut ws = Ws::connect(&daemon).await;

        daemon
            .client
            .workspace_update()
            .id(web.id)
            .body_map(|patch| {
                patch
                    .link(repository_names(&[GADGETS]))
                    .unlink(vec![widgets()])
            })
            .send()
            .await
            .expect("update");

        let mut changed = vec![
            next_link_change(&mut ws).await,
            next_link_change(&mut ws).await,
        ];
        changed.sort_by(|a, b| a.1.cmp(&b.1));
        let mut expected = vec![
            (Entity::Repository, repository_id(WIDGETS).to_string()),
            (Entity::Repository, repository_id(GADGETS).to_string()),
        ];
        expected.sort_by(|a, b| a.1.cmp(&b.1));
        assert_eq!(changed, expected);
        assert_eq!(
            next_link_change(&mut ws).await,
            (Entity::Workspace, web.id.to_string())
        );
    }
}
