//! `/v1/workspaces`: create, read, rename, mute, archive and delete. A Workspace has no
//! folder: nothing here creates or removes one on disk (spec §3, §4).

use agentdeck_client::Client;
use agentdeck_client::types::{NewWorkspace, Workspace};
use agentdeck_core::AgentState;
use agentdeck_core::ws::ServerMessage;

use crate::support::terminal::{Place, in_state, sh};
use crate::support::ws::Ws;
use crate::support::{TestDaemon, error_response};

async fn create(client: &Client, name: &str) -> Workspace {
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

/// The keys of `/v1/workspaces/{id}`'s JSON, sorted, read without the generated client.
async fn keys(daemon: &TestDaemon, workspace: &Workspace) -> Vec<String> {
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!(
            "{}/v1/workspaces/{}",
            daemon.base_url(),
            workspace.id
        ))
        .bearer_auth(&daemon.info.token)
        .send()
        .await
        .expect("a response")
        .json()
        .await
        .expect("JSON");
    let mut keys: Vec<String> = body
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

mod post_v1_workspaces {
    use super::*;

    #[tokio::test]
    async fn should_create_a_workspace_with_no_folder_201() {
        let daemon = TestDaemon::start().await;

        let response = daemon
            .client
            .workspace_create()
            .body(NewWorkspace {
                name: "Fix the Bug".into(),
                link: Vec::new(),
            })
            .send()
            .await
            .expect("create");

        assert_eq!(response.status().as_u16(), 201);
        let workspace = response.into_inner();
        assert_eq!(workspace.slug, "fix-the-bug");
        assert_eq!(workspace.name, "Fix the Bug");
        assert!(!workspace.muted);
        assert_eq!(workspace.archived_at, None);
        assert_eq!(
            keys(&daemon, &workspace).await,
            ["archived_at", "created_at", "id", "muted", "name", "slug"]
        );
        assert!(
            !daemon.data_dir().join("workspaces").exists(),
            "no scratch folder"
        );
    }

    #[tokio::test]
    async fn should_ignore_a_dir_an_older_client_sends_201() {
        let daemon = TestDaemon::start().await;
        let dir = daemon.new_dir("attached");

        let response = reqwest::Client::new()
            .post(format!("{}/v1/workspaces", daemon.base_url()))
            .bearer_auth(&daemon.info.token)
            .json(&serde_json::json!({"name": "web", "dir": dir}))
            .send()
            .await
            .expect("a response");

        assert_eq!(response.status().as_u16(), 201);
        let body: serde_json::Value = response.json().await.expect("JSON");
        assert_eq!(body["name"], "web");
        assert_eq!(body.get("dir"), None);
    }

    #[tokio::test]
    async fn should_suffix_the_slug_of_a_repeated_name_201() {
        let daemon = TestDaemon::start().await;

        let first = create(&daemon.client, "api").await;
        let second = create(&daemon.client, "api").await;

        assert_eq!(first.slug, "api");
        assert_eq!(second.slug, "api-2");
    }

    #[tokio::test]
    async fn should_not_skip_a_slug_whose_old_scratch_folder_is_left_201() {
        let daemon = TestDaemon::start().await;
        let old = daemon.data_dir().join("workspaces/api");
        std::fs::create_dir_all(&old).expect("an old scratch folder");

        let workspace = create(&daemon.client, "api").await;

        assert_eq!(workspace.slug, "api");
    }

    #[tokio::test]
    async fn should_reject_a_blank_name_400() {
        let daemon = TestDaemon::start().await;

        let (status, error) = error_response(
            daemon
                .client
                .workspace_create()
                .body(NewWorkspace {
                    name: "  ".into(),
                    link: Vec::new(),
                })
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("invalid_name"));
    }
}

mod get_v1_workspaces {
    use super::*;

    #[tokio::test]
    async fn should_list_every_workspace_oldest_first_200() {
        let daemon = TestDaemon::start().await;
        let a = create(&daemon.client, "a").await;
        let b = create(&daemon.client, "b").await;
        daemon
            .client
            .workspace_archive()
            .id(b.id)
            .send()
            .await
            .expect("archive");

        let workspaces = daemon
            .client
            .workspace_list()
            .send()
            .await
            .expect("list")
            .into_inner();

        assert_eq!(
            workspaces.iter().map(|w| w.id).collect::<Vec<_>>(),
            vec![a.id, b.id]
        );
    }
}

mod get_v1_workspaces_id {
    use super::*;

    #[tokio::test]
    async fn should_return_the_workspace_200() {
        let daemon = TestDaemon::start().await;
        let created = create(&daemon.client, "a").await;

        let workspace = daemon
            .client
            .workspace_get()
            .id(created.id)
            .send()
            .await
            .expect("get")
            .into_inner();

        assert_eq!(workspace, created);
    }

    #[tokio::test]
    async fn should_answer_not_found_for_an_unknown_id_404() {
        let daemon = TestDaemon::start().await;

        let (status, error) = error_response(
            daemon
                .client
                .workspace_get()
                .id(uuid::Uuid::now_v7())
                .send()
                .await,
        );

        assert_eq!(status, 404);
        assert_eq!(error.error_code.as_deref(), Some("not_found"));
    }
}

mod patch_v1_workspaces_id {
    use super::*;

    #[tokio::test]
    async fn should_rename_and_mute_keeping_the_slug_200() {
        let daemon = TestDaemon::start().await;
        let created = create(&daemon.client, "old").await;

        let workspace = daemon
            .client
            .workspace_update()
            .id(created.id)
            .body_map(|patch| patch.name(Some("new".to_owned())).muted(Some(true)))
            .send()
            .await
            .expect("update")
            .into_inner();

        assert_eq!(workspace.name, "new");
        assert!(workspace.muted);
        assert_eq!(workspace.slug, "old");
    }

    #[tokio::test]
    async fn should_reject_a_blank_name_and_keep_the_old_one_400() {
        let daemon = TestDaemon::start().await;
        let created = create(&daemon.client, "old").await;

        let (status, error) = error_response(
            daemon
                .client
                .workspace_update()
                .id(created.id)
                .body_map(|patch| patch.name(Some(" ".to_owned())))
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("invalid_name"));
        let kept = daemon.client.workspace_get().id(created.id).send().await;
        assert_eq!(kept.expect("get").into_inner().name, "old");
    }

    #[tokio::test]
    async fn should_answer_not_found_for_an_unknown_id_404() {
        let daemon = TestDaemon::start().await;

        let (status, _) = error_response(
            daemon
                .client
                .workspace_update()
                .id(uuid::Uuid::now_v7())
                .body_map(|patch| patch.muted(Some(true)))
                .send()
                .await,
        );

        assert_eq!(status, 404);
    }
}

mod post_v1_workspaces_id_archive {
    use super::*;

    #[tokio::test]
    async fn should_stamp_archived_at_200() {
        let daemon = TestDaemon::start().await;
        let created = create(&daemon.client, "a").await;

        let workspace = daemon
            .client
            .workspace_archive()
            .id(created.id)
            .send()
            .await
            .expect("archive")
            .into_inner();

        assert!(workspace.archived_at.is_some());
    }

    #[tokio::test]
    async fn should_refuse_while_a_pane_runs_409() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        for _ in 0..2 {
            daemon
                .client
                .pane_create()
                .body(place.new_pane(&sh("sleep 60")))
                .send()
                .await
                .expect("spawn");
        }

        let (status, body) = error_response(
            daemon
                .client
                .workspace_archive()
                .id(place.workspace.id)
                .send()
                .await,
        );

        assert_eq!(status, 409);
        assert_eq!(body.error_code.as_deref(), Some("panes_running"));
        assert_eq!(body.message, "Close its 2 running panes first");
    }

    #[tokio::test]
    async fn should_archive_once_its_panes_have_exited_200() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = daemon
            .client
            .pane_create()
            .body(place.new_pane(&sh("true")))
            .send()
            .await
            .expect("spawn")
            .into_inner();
        in_state(&daemon.client, pane.id, AgentState::Exited).await;

        let workspace = daemon
            .client
            .workspace_archive()
            .id(place.workspace.id)
            .send()
            .await
            .expect("archive")
            .into_inner();

        assert!(workspace.archived_at.is_some());
    }

    #[tokio::test]
    async fn should_archive_as_soon_as_its_last_pane_exit_is_published_200() {
        // The exit is saved before it is published, so the archive check sees it (#172).
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let mut ws = Ws::connect(&daemon).await;
        let pane = place.spawn(&daemon.client, &sh("true")).await;
        ws.expect(ServerMessage::Exit {
            pane: pane.id,
            code: Some(0),
        })
        .await;

        let workspace = daemon
            .client
            .workspace_archive()
            .id(place.workspace.id)
            .send()
            .await
            .expect("archive")
            .into_inner();

        assert!(workspace.archived_at.is_some());
    }

    #[tokio::test]
    async fn should_clear_archived_at_on_unarchive_200() {
        let daemon = TestDaemon::start().await;
        let created = create(&daemon.client, "a").await;
        daemon
            .client
            .workspace_archive()
            .id(created.id)
            .send()
            .await
            .expect("archive");

        let workspace = daemon
            .client
            .workspace_unarchive()
            .id(created.id)
            .send()
            .await
            .expect("unarchive")
            .into_inner();

        assert_eq!(workspace.archived_at, None);
    }
}

mod get_v1_workspaces_id_dirty {
    use super::*;

    #[tokio::test]
    async fn should_be_gone_with_the_workspace_folder_404() {
        let daemon = TestDaemon::start().await;
        let workspace = create(&daemon.client, "web").await;

        let response = reqwest::Client::new()
            .get(format!(
                "{}/v1/workspaces/{}/dirty",
                daemon.base_url(),
                workspace.id
            ))
            .bearer_auth(&daemon.info.token)
            .send()
            .await
            .expect("a response");

        assert_eq!(response.status().as_u16(), 404);
        let body: serde_json::Value = response.json().await.expect("JSON");
        assert_eq!(
            body["error_code"],
            serde_json::Value::Null,
            "dropshot's own 404"
        );
    }
}

mod delete_v1_workspaces_id {
    use super::*;

    #[tokio::test]
    async fn should_delete_the_workspace_and_its_panes_204() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place.spawn(&daemon.client, &sh("sleep 60")).await;

        let response = daemon
            .client
            .workspace_delete()
            .id(place.workspace.id)
            .send()
            .await
            .expect("delete");

        assert_eq!(response.status().as_u16(), 204);
        let (status, _) = error_response(
            daemon
                .client
                .workspace_get()
                .id(place.workspace.id)
                .send()
                .await,
        );
        assert_eq!(status, 404);
        let (status, _) = error_response(daemon.client.pane_get().id(pane.id).send().await);
        assert_eq!(status, 404);
    }

    #[tokio::test]
    async fn should_touch_no_folder_on_disk_204() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        std::fs::write(place.dir.join("notes.md"), "work in progress").expect("a file");
        let repo = place.dir.join("repo");
        crate::support::git::init_with_commit(&repo);
        std::fs::write(repo.join("wip.txt"), "uncommitted").expect("uncommitted work");
        place
            .spawn(&daemon.client, &sh("cd repo && exec sleep 60"))
            .await;
        // A scratch folder from before Workspaces lost theirs, named by the slug.
        let scratch = daemon
            .data_dir()
            .join("workspaces")
            .join(&place.workspace.slug);
        std::fs::create_dir_all(&scratch).expect("an old scratch folder");
        std::fs::write(scratch.join("keep.txt"), "keep").expect("a file in it");

        daemon
            .client
            .workspace_delete()
            .id(place.workspace.id)
            .send()
            .await
            .expect("delete");

        assert_eq!(
            std::fs::read_to_string(place.dir.join("notes.md")).expect("the file stays"),
            "work in progress"
        );
        assert!(repo.join("wip.txt").is_file(), "the repo's work stays");
        assert!(
            scratch.join("keep.txt").is_file(),
            "the old scratch folder stays"
        );
    }

    #[tokio::test]
    async fn should_take_no_force_404() {
        let daemon = TestDaemon::start().await;

        let response = reqwest::Client::new()
            .delete(format!(
                "{}/v1/workspaces/{}?force=true",
                daemon.base_url(),
                uuid::Uuid::now_v7()
            ))
            .bearer_auth(&daemon.info.token)
            .send()
            .await
            .expect("a response");

        // An older client's `force` is ignored: the unknown workspace is still not found.
        assert_eq!(response.status().as_u16(), 404);
        let body: serde_json::Value = response.json().await.expect("JSON");
        assert_eq!(body["error_code"], "not_found");
    }

    #[tokio::test]
    async fn should_answer_not_found_for_an_unknown_id_404() {
        let daemon = TestDaemon::start().await;

        let (status, error) = error_response(
            daemon
                .client
                .workspace_delete()
                .id(uuid::Uuid::now_v7())
                .send()
                .await,
        );

        assert_eq!(status, 404);
        assert_eq!(error.error_code.as_deref(), Some("not_found"));
    }
}
