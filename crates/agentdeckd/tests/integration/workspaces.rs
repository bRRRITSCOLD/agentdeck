//! `/v1/workspaces`: create, read, rename, mute, archive, the dirty report and delete.

use std::path::Path;

use agentdeck_client::Client;
use agentdeck_client::types::{NewWorkspace, UnpushedBranch, Workspace};

use crate::support::git::{clone_with_pushed_commit, git, init_with_commit};
use agentdeck_core::AgentState;
use agentdeck_core::ws::ServerMessage;

use crate::support::terminal::{Place, in_state, sh};
use crate::support::ws::Ws;
use crate::support::{TestDaemon, error_response};

async fn create(client: &Client, name: &str, dir: Option<&Path>) -> Workspace {
    client
        .workspace_create()
        .body(NewWorkspace {
            name: name.into(),
            dir: dir.map(|dir| dir.display().to_string()),
        })
        .send()
        .await
        .expect("create a workspace")
        .into_inner()
}

mod post_v1_workspaces {
    use super::*;

    #[tokio::test]
    async fn should_create_a_scratch_dir_named_by_the_slug_201() {
        let daemon = TestDaemon::start().await;

        let response = daemon
            .client
            .workspace_create()
            .body(NewWorkspace {
                name: "Fix the Bug".into(),
                dir: None,
            })
            .send()
            .await
            .expect("create");

        assert_eq!(response.status().as_u16(), 201);
        let workspace = response.into_inner();
        let expected_dir = daemon.data_dir().join("workspaces/fix-the-bug");
        assert_eq!(workspace.slug, "fix-the-bug");
        assert_eq!(workspace.name, "Fix the Bug");
        assert_eq!(workspace.dir, expected_dir.display().to_string());
        assert!(workspace.dir_owned);
        assert!(!workspace.muted);
        assert_eq!(workspace.archived_at, None);
        assert!(expected_dir.is_dir());
    }

    #[tokio::test]
    async fn should_suffix_the_slug_of_a_repeated_name_201() {
        let daemon = TestDaemon::start().await;

        let first = create(&daemon.client, "api", None).await;
        let second = create(&daemon.client, "api", None).await;

        assert_eq!(first.slug, "api");
        assert_eq!(second.slug, "api-2");
        assert_ne!(first.dir, second.dir);
    }

    #[tokio::test]
    async fn should_attach_an_existing_dir_without_owning_it_201() {
        let daemon = TestDaemon::start().await;
        let dir = daemon.new_dir("attached");

        let workspace = create(&daemon.client, "attached", Some(&dir)).await;

        assert_eq!(workspace.dir, dir.display().to_string());
        assert!(!workspace.dir_owned);
    }

    #[tokio::test]
    async fn should_reject_a_relative_dir_400() {
        let daemon = TestDaemon::start().await;

        let (status, error) = error_response(
            daemon
                .client
                .workspace_create()
                .body(NewWorkspace {
                    name: "x".into(),
                    dir: Some("relative/dir".into()),
                })
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("invalid_dir"));
    }

    #[tokio::test]
    async fn should_reject_a_dir_that_does_not_exist_400() {
        let daemon = TestDaemon::start().await;
        let missing = daemon.root().join("missing");

        let (status, error) = error_response(
            daemon
                .client
                .workspace_create()
                .body(NewWorkspace {
                    name: "x".into(),
                    dir: Some(missing.display().to_string()),
                })
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("invalid_dir"));
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
                    dir: None,
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
        let a = create(&daemon.client, "a", None).await;
        let b = create(&daemon.client, "b", None).await;
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
        let created = create(&daemon.client, "a", None).await;

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
    async fn should_rename_and_mute_keeping_the_slug_and_dir_200() {
        let daemon = TestDaemon::start().await;
        let created = create(&daemon.client, "old", None).await;

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
        assert_eq!(workspace.dir, created.dir);
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
    async fn should_stamp_archived_at_and_keep_the_dir_200() {
        let daemon = TestDaemon::start().await;
        let created = create(&daemon.client, "a", None).await;

        let workspace = daemon
            .client
            .workspace_archive()
            .id(created.id)
            .send()
            .await
            .expect("archive")
            .into_inner();

        assert!(workspace.archived_at.is_some());
        assert!(Path::new(&workspace.dir).is_dir());
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
        let created = create(&daemon.client, "a", None).await;
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

/// `name`, holding `commits` commits on no remote.
fn unpushed(name: &str, commits: u32) -> UnpushedBranch {
    UnpushedBranch {
        name: name.into(),
        commits,
    }
}

mod get_v1_workspaces_id_dirty {
    use super::*;

    #[tokio::test]
    async fn should_list_uncommitted_files_and_unpushed_branches_200() {
        let daemon = TestDaemon::start().await;
        let dir = daemon.new_dir("ws");
        let repo = dir.join("repo");
        init_with_commit(&repo);
        std::fs::write(repo.join("scratch.txt"), "wip\n").expect("write");
        let workspace = create(&daemon.client, "ws", Some(&dir)).await;

        let report = daemon
            .client
            .workspace_dirty()
            .id(workspace.id)
            .send()
            .await
            .expect("dirty")
            .into_inner();

        assert_eq!(report.repos.len(), 1, "{report:?}");
        assert_eq!(report.repos[0].path, repo.display().to_string());
        assert_eq!(report.repos[0].uncommitted, vec!["scratch.txt"]);
        assert_eq!(report.repos[0].unpushed_branches, [unpushed("main", 1)]);
    }

    #[tokio::test]
    async fn should_list_a_branch_ahead_of_its_remote_200() {
        let daemon = TestDaemon::start().await;
        let dir = daemon.new_dir("ws");
        let repo = dir.join("clone");
        clone_with_pushed_commit(&daemon.root().join("remote.git"), &repo);
        git(&repo, &["switch", "--quiet", "-c", "feature"]);
        std::fs::write(repo.join("feature.txt"), "new\n").expect("write");
        git(&repo, &["add", "feature.txt"]);
        git(&repo, &["commit", "--quiet", "-m", "feature"]);
        let workspace = create(&daemon.client, "ws", Some(&dir)).await;

        let report = daemon
            .client
            .workspace_dirty()
            .id(workspace.id)
            .send()
            .await
            .expect("dirty")
            .into_inner();

        assert_eq!(report.repos.len(), 1, "{report:?}");
        assert!(report.repos[0].uncommitted.is_empty());
        assert_eq!(report.repos[0].unpushed_branches, [unpushed("feature", 1)]);
    }

    #[tokio::test]
    async fn should_count_each_branchs_commits_on_no_remote_200() {
        let daemon = TestDaemon::start().await;
        let dir = daemon.new_dir("ws");
        let repo = dir.join("clone");
        clone_with_pushed_commit(&daemon.root().join("remote.git"), &repo);
        let commit = |name: &str| {
            std::fs::write(repo.join(name), "new\n").expect("write");
            git(&repo, &["add", name]);
            git(&repo, &["commit", "--quiet", "-m", name]);
        };
        // Pushed with an upstream, then one commit ahead of it.
        git(&repo, &["switch", "--quiet", "-c", "tracked"]);
        commit("tracked.txt");
        git(&repo, &["push", "--quiet", "-u", "origin", "tracked"]);
        commit("ahead.txt");
        // Never pushed, so no upstream: `ahead.txt` and its own commit are on no remote.
        git(&repo, &["switch", "--quiet", "-c", "local"]);
        commit("local.txt");
        let workspace = create(&daemon.client, "ws", Some(&dir)).await;

        let report = daemon
            .client
            .workspace_dirty()
            .id(workspace.id)
            .send()
            .await
            .expect("dirty")
            .into_inner();

        assert_eq!(report.repos.len(), 1, "{report:?}");
        assert_eq!(
            report.repos[0].unpushed_branches,
            [unpushed("local", 2), unpushed("tracked", 1)]
        );
    }

    #[tokio::test]
    async fn should_report_nothing_when_everything_is_pushed_200() {
        let daemon = TestDaemon::start().await;
        let dir = daemon.new_dir("ws");
        clone_with_pushed_commit(&daemon.root().join("remote.git"), &dir.join("clone"));
        let workspace = create(&daemon.client, "ws", Some(&dir)).await;

        let report = daemon
            .client
            .workspace_dirty()
            .id(workspace.id)
            .send()
            .await
            .expect("dirty")
            .into_inner();

        assert!(report.repos.is_empty(), "{report:?}");
    }
}

mod delete_v1_workspaces_id {
    use agentdeck_client::types::{NewInstruction, NewTask};

    use super::*;

    #[tokio::test]
    async fn should_refuse_a_dirty_workspace_with_the_report_409() {
        let daemon = TestDaemon::start().await;
        let workspace = create(&daemon.client, "ws", None).await;
        let repo = Path::new(&workspace.dir).join("repo");
        init_with_commit(&repo);

        let (status, error) = error_response(
            daemon
                .client
                .workspace_delete()
                .id(workspace.id)
                .send()
                .await,
        );

        assert_eq!(status, 409);
        assert_eq!(error.error_code.as_deref(), Some("dirty"));
        let report = error.dirty.expect("the dirty report");
        assert_eq!(report.repos[0].unpushed_branches, [unpushed("main", 1)]);
        assert!(repo.is_dir());
        daemon
            .client
            .workspace_get()
            .id(workspace.id)
            .send()
            .await
            .expect("still there");
    }

    #[tokio::test]
    async fn should_delete_a_dirty_workspace_with_force_204() {
        let daemon = TestDaemon::start().await;
        let workspace = create(&daemon.client, "ws", None).await;
        init_with_commit(&Path::new(&workspace.dir).join("repo"));

        let response = daemon
            .client
            .workspace_delete()
            .id(workspace.id)
            .force(true)
            .send()
            .await
            .expect("delete");

        assert_eq!(response.status().as_u16(), 204);
        assert!(!Path::new(&workspace.dir).exists());
    }

    #[tokio::test]
    async fn should_remove_a_clean_scratch_dir_204() {
        let daemon = TestDaemon::start().await;
        let workspace = create(&daemon.client, "ws", None).await;

        daemon
            .client
            .workspace_delete()
            .id(workspace.id)
            .send()
            .await
            .expect("delete");

        assert!(!Path::new(&workspace.dir).exists());
        let (status, _) =
            error_response(daemon.client.workspace_get().id(workspace.id).send().await);
        assert_eq!(status, 404);
    }

    #[tokio::test]
    async fn should_leave_an_attached_dir_in_place_204() {
        let daemon = TestDaemon::start().await;
        let dir = daemon.new_dir("mine");
        std::fs::write(dir.join("keep.txt"), "keep\n").expect("write");
        let workspace = create(&daemon.client, "mine", Some(&dir)).await;

        daemon
            .client
            .workspace_delete()
            .id(workspace.id)
            .send()
            .await
            .expect("delete");

        assert!(dir.join("keep.txt").is_file());
    }

    #[tokio::test]
    async fn should_move_its_tasks_to_the_inbox_and_drop_its_instructions_204() {
        let daemon = TestDaemon::start().await;
        let workspace = create(&daemon.client, "ws", None).await;
        let task = daemon
            .client
            .task_create()
            .body(NewTask {
                title: "t".into(),
                body: None,
                status: None,
                workspace_id: Some(workspace.id),
            })
            .send()
            .await
            .expect("task")
            .into_inner();
        let instruction = daemon
            .client
            .instruction_create()
            .id(workspace.id)
            .body(NewInstruction { body: "i".into() })
            .send()
            .await
            .expect("instruction")
            .into_inner();

        daemon
            .client
            .workspace_delete()
            .id(workspace.id)
            .send()
            .await
            .expect("delete");

        let task = daemon
            .client
            .task_get()
            .id(task.id)
            .send()
            .await
            .expect("the task stays")
            .into_inner();
        assert_eq!(task.workspace_id, None);
        let (status, _) = error_response(
            daemon
                .client
                .instruction_delete()
                .id(instruction.id)
                .send()
                .await,
        );
        assert_eq!(status, 404, "the instruction went with the workspace");
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
