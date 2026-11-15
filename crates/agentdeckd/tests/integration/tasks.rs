//! The global task list: `/v1/tasks`.

use agentdeck_client::Client;
use agentdeck_client::types::{NewTask, NewWorkspace, Task, Workspace};
use agentdeck_core::{TaskKind, TaskPlace, TaskStatus};

use crate::support::{TestDaemon, error_response};

async fn workspace(client: &Client) -> Workspace {
    client
        .workspace_create()
        .body(NewWorkspace {
            name: "ws".into(),
            dir: None,
        })
        .send()
        .await
        .expect("create a workspace")
        .into_inner()
}

async fn task(client: &Client, title: &str, workspace: Option<&Workspace>) -> Task {
    client
        .task_create()
        .body(NewTask {
            title: title.into(),
            body: None,
            status: None,
            workspace_id: workspace.map(|ws| ws.id),
        })
        .send()
        .await
        .expect("create a task")
        .into_inner()
}

async fn set_status(client: &Client, task: &Task, status: TaskStatus) -> Task {
    client
        .task_update()
        .id(task.id)
        .body_map(|patch| patch.status(Some(status)))
        .send()
        .await
        .expect("update")
        .into_inner()
}

mod post_v1_tasks {
    use super::*;

    #[tokio::test]
    async fn should_default_to_a_plain_todo_in_the_inbox_201() {
        let daemon = TestDaemon::start().await;

        let response = daemon
            .client
            .task_create()
            .body(NewTask {
                title: "write tests".into(),
                body: None,
                status: None,
                workspace_id: None,
            })
            .send()
            .await
            .expect("create");

        assert_eq!(response.status().as_u16(), 201);
        let task = response.into_inner();
        assert_eq!(task.title, "write tests");
        assert_eq!(task.body, "");
        assert_eq!(task.status, TaskStatus::Todo);
        assert_eq!(task.kind, TaskKind::Plain);
        assert_eq!(task.pr, None);
        assert_eq!(task.workspace_id, None);
        assert_eq!(task.done_at, None);
        assert_eq!(task.created_at, task.updated_at);
    }

    #[tokio::test]
    async fn should_stamp_done_at_for_a_task_created_done_201() {
        let daemon = TestDaemon::start().await;

        let task = daemon
            .client
            .task_create()
            .body(NewTask {
                title: "already".into(),
                body: Some("b".into()),
                status: Some(TaskStatus::Done),
                workspace_id: None,
            })
            .send()
            .await
            .expect("create")
            .into_inner();

        assert!(task.done_at.is_some());
    }

    #[tokio::test]
    async fn should_reject_an_unknown_workspace_400() {
        let daemon = TestDaemon::start().await;

        let (status, error) = error_response(
            daemon
                .client
                .task_create()
                .body(NewTask {
                    title: "t".into(),
                    body: None,
                    status: None,
                    workspace_id: Some(uuid::Uuid::now_v7().into()),
                })
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("unknown_workspace"));
    }

    #[tokio::test]
    async fn should_reject_a_blank_title_400() {
        let daemon = TestDaemon::start().await;

        let (status, error) = error_response(
            daemon
                .client
                .task_create()
                .body(NewTask {
                    title: " ".into(),
                    body: None,
                    status: None,
                    workspace_id: None,
                })
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("invalid_title"));
    }
}

mod get_v1_tasks {
    use super::*;

    async fn ids(request: agentdeck_client::builder::TaskList<'_>) -> Vec<Task> {
        request.send().await.expect("list").into_inner()
    }

    #[tokio::test]
    async fn should_list_every_task_oldest_first_200() {
        let daemon = TestDaemon::start().await;
        let a = task(&daemon.client, "a", None).await;
        let b = task(&daemon.client, "b", None).await;

        let tasks = ids(daemon.client.task_list()).await;

        assert_eq!(
            tasks.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![a.id, b.id]
        );
    }

    #[tokio::test]
    async fn should_filter_by_status_200() {
        let daemon = TestDaemon::start().await;
        task(&daemon.client, "todo", None).await;
        let doing = task(&daemon.client, "doing", None).await;
        set_status(&daemon.client, &doing, TaskStatus::Doing).await;

        let tasks = ids(daemon.client.task_list().status(TaskStatus::Doing)).await;

        assert_eq!(
            tasks.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![doing.id]
        );
    }

    #[tokio::test]
    async fn should_filter_by_workspace_and_by_inbox_200() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;
        let linked = task(&daemon.client, "linked", Some(&ws)).await;
        let inbox = task(&daemon.client, "inbox", None).await;

        let in_ws = ids(daemon.client.task_list().workspace(ws.id)).await;
        let in_inbox = ids(daemon.client.task_list().inbox(true)).await;
        let not_inbox = ids(daemon.client.task_list().inbox(false)).await;

        assert_eq!(
            in_ws.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![linked.id]
        );
        assert_eq!(
            in_inbox.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![inbox.id]
        );
        assert_eq!(
            not_inbox.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![linked.id]
        );
    }

    #[tokio::test]
    async fn should_filter_by_kind_200() {
        let daemon = TestDaemon::start().await;
        task(&daemon.client, "plain", None).await;

        let prs = ids(daemon.client.task_list().kind(TaskKind::Pr)).await;
        let plain = ids(daemon.client.task_list().kind(TaskKind::Plain)).await;

        assert!(prs.is_empty());
        assert_eq!(plain.len(), 1);
    }
}

mod get_v1_tasks_id {
    use super::*;

    #[tokio::test]
    async fn should_answer_not_found_for_an_unknown_task_404() {
        let daemon = TestDaemon::start().await;

        let (status, error) = error_response(
            daemon
                .client
                .task_get()
                .id(uuid::Uuid::now_v7())
                .send()
                .await,
        );

        assert_eq!(status, 404);
        assert_eq!(error.error_code.as_deref(), Some("not_found"));
    }
}

mod patch_v1_tasks_id {
    use super::*;

    #[tokio::test]
    async fn should_stamp_done_at_on_done_and_clear_it_on_leaving_done_200() {
        let daemon = TestDaemon::start().await;
        let created = task(&daemon.client, "t", None).await;

        let done = set_status(&daemon.client, &created, TaskStatus::Done).await;
        let reopened = set_status(&daemon.client, &created, TaskStatus::Doing).await;

        assert!(done.done_at.is_some());
        assert_eq!(reopened.done_at, None);
        assert_eq!(reopened.status, TaskStatus::Doing);
    }

    #[tokio::test]
    async fn should_change_title_and_body_and_link_a_workspace_200() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;
        let created = task(&daemon.client, "t", None).await;

        let updated = daemon
            .client
            .task_update()
            .id(created.id)
            .body_map(|patch| {
                patch
                    .title(Some("new".to_owned()))
                    .body(Some("details".to_owned()))
                    .workspace_id(Some(TaskPlace::Workspace(ws.id)))
            })
            .send()
            .await
            .expect("update")
            .into_inner();

        assert_eq!(updated.title, "new");
        assert_eq!(updated.body, "details");
        assert_eq!(updated.workspace_id, Some(ws.id));
        assert_eq!(updated.status, TaskStatus::Todo);
    }
}

mod patch_v1_tasks_id_workspace {
    use agentdeck_client::types::{PanePatch, TaskPatch};
    use agentdeck_core::ws::{Entity, ServerMessage};

    use super::*;
    use crate::support::terminal::{Place, sh};
    use crate::support::ws::Ws;

    /// The request sending `patch` for `task`.
    fn patch<'a>(
        client: &'a Client,
        task: &Task,
        patch: TaskPatch,
    ) -> agentdeck_client::builder::TaskUpdate<'a> {
        client.task_update().id(task.id).body(patch)
    }

    fn move_to(place: TaskPlace) -> TaskPatch {
        TaskPatch {
            workspace_id: Some(place),
            ..TaskPatch::default()
        }
    }

    #[tokio::test]
    async fn should_move_a_task_to_the_inbox_given_null_200() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;
        let created = task(&daemon.client, "t", Some(&ws)).await;

        let moved = patch(&daemon.client, &created, move_to(TaskPlace::Inbox))
            .send()
            .await
            .expect("move to the inbox")
            .into_inner();

        assert_eq!(moved.workspace_id, None);
        let inbox = daemon
            .client
            .task_list()
            .inbox(true)
            .send()
            .await
            .expect("list the inbox");
        assert_eq!(inbox.iter().map(|t| t.id).collect::<Vec<_>>(), [created.id]);
    }

    #[tokio::test]
    async fn should_leave_the_workspace_as_it_is_without_one_200() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;
        let created = task(&daemon.client, "t", Some(&ws)).await;

        let renamed = patch(
            &daemon.client,
            &created,
            TaskPatch {
                title: Some("renamed".into()),
                ..TaskPatch::default()
            },
        )
        .send()
        .await
        .expect("rename")
        .into_inner();

        assert_eq!(renamed.title, "renamed");
        assert_eq!(renamed.workspace_id, Some(ws.id));
    }

    #[tokio::test]
    async fn should_reject_an_unknown_workspace_400() {
        let daemon = TestDaemon::start().await;
        let created = task(&daemon.client, "t", None).await;

        let (status, error) = error_response(
            patch(
                &daemon.client,
                &created,
                move_to(TaskPlace::Workspace(uuid::Uuid::now_v7().into())),
            )
            .send()
            .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("unknown_workspace"));
    }

    #[tokio::test]
    async fn should_reject_an_archived_workspace_400() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;
        daemon
            .client
            .workspace_archive()
            .id(ws.id)
            .send()
            .await
            .expect("archive");
        let created = task(&daemon.client, "t", None).await;

        let (status, error) = error_response(
            patch(
                &daemon.client,
                &created,
                move_to(TaskPlace::Workspace(ws.id)),
            )
            .send()
            .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("unknown_workspace"));
        let read = daemon
            .client
            .task_get()
            .id(created.id)
            .send()
            .await
            .expect("read");
        assert_eq!(read.workspace_id, None);
    }

    #[tokio::test]
    async fn should_reject_a_blank_title_400() {
        let daemon = TestDaemon::start().await;
        let created = task(&daemon.client, "t", None).await;

        let (status, error) = error_response(
            patch(
                &daemon.client,
                &created,
                TaskPatch {
                    title: Some("  ".into()),
                    ..TaskPatch::default()
                },
            )
            .send()
            .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("invalid_title"));
    }

    mod linked_pane {
        use super::*;

        /// A pane in `place` linked to a new task there.
        async fn linked(daemon: &TestDaemon, place: &Place) -> (Task, agentdeck_core::PaneId) {
            let created = task(&daemon.client, "t", Some(&place.workspace)).await;
            let pane = place.spawn(&daemon.client, &sh("sleep 60")).await;
            daemon
                .client
                .pane_update()
                .id(pane.id)
                .body(PanePatch {
                    task_id: Some(created.id),
                })
                .send()
                .await
                .expect("link");
            (created, pane.id)
        }

        async fn pane_task(
            daemon: &TestDaemon,
            pane: agentdeck_core::PaneId,
        ) -> Option<agentdeck_core::TaskId> {
            daemon
                .client
                .pane_get()
                .id(pane)
                .send()
                .await
                .expect("read the pane")
                .into_inner()
                .task_id
        }

        #[tokio::test]
        async fn should_unlink_a_pane_in_the_workspace_it_left_200() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let other = workspace(&daemon.client).await;
            let (created, pane) = linked(&daemon, &place).await;
            let mut ws = Ws::connect(&daemon).await;

            patch(
                &daemon.client,
                &created,
                move_to(TaskPlace::Workspace(other.id)),
            )
            .send()
            .await
            .expect("move");

            assert_eq!(pane_task(&daemon, pane).await, None);
            ws.expect(ServerMessage::Changed {
                entity: Entity::Pane,
                id: pane.to_string(),
            })
            .await;
        }

        #[tokio::test]
        async fn should_unlink_it_when_the_task_moves_to_the_inbox_200() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let (created, pane) = linked(&daemon, &place).await;

            patch(&daemon.client, &created, move_to(TaskPlace::Inbox))
                .send()
                .await
                .expect("move");

            assert_eq!(pane_task(&daemon, pane).await, None);
        }

        #[tokio::test]
        async fn should_keep_it_linked_while_the_task_stays_in_its_workspace_200() {
            let daemon = TestDaemon::start().await;
            let place = Place::new(&daemon).await;
            let (created, pane) = linked(&daemon, &place).await;

            patch(
                &daemon.client,
                &created,
                move_to(TaskPlace::Workspace(place.workspace.id)),
            )
            .send()
            .await
            .expect("pick the same workspace");
            patch(
                &daemon.client,
                &created,
                TaskPatch {
                    body: Some("more".into()),
                    ..TaskPatch::default()
                },
            )
            .send()
            .await
            .expect("edit the body");

            assert_eq!(pane_task(&daemon, pane).await, Some(created.id));
        }
    }
}

mod unseen_agent_comments {
    use agentdeck_client::types::NewComment;
    use agentdeck_core::{PaneId, TaskId};

    use super::*;
    use crate::support::terminal::{Place, sh};

    async fn comment(daemon: &TestDaemon, task: TaskId, pane: Option<PaneId>) {
        daemon
            .client
            .comment_create()
            .id(task)
            .body(NewComment {
                body: "c".into(),
                pane,
            })
            .send()
            .await
            .expect("comment");
    }

    #[tokio::test]
    async fn should_count_only_the_agents_comments_the_user_hasnt_seen_200() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let created = task(&daemon.client, "t", None).await;
        let pane = place.spawn(&daemon.client, &sh("sleep 60")).await;
        comment(&daemon, created.id, Some(pane.id)).await;
        comment(&daemon, created.id, Some(pane.id)).await;
        comment(&daemon, created.id, None).await;

        let got = daemon
            .client
            .task_get()
            .id(created.id)
            .send()
            .await
            .expect("get")
            .into_inner();
        let listed = daemon
            .client
            .task_list()
            .send()
            .await
            .expect("list")
            .into_inner();

        assert_eq!(created.unseen_agent_comments, 0);
        assert_eq!(got.unseen_agent_comments, 2);
        let row = listed.iter().find(|t| t.id == created.id).expect("listed");
        assert_eq!(row.unseen_agent_comments, 2);
    }
}

mod delete_v1_tasks_id {
    use agentdeck_client::types::NewComment;

    use super::*;

    #[tokio::test]
    async fn should_remove_the_task_and_its_comments_204() {
        let daemon = TestDaemon::start().await;
        let created = task(&daemon.client, "t", None).await;
        let comment = daemon
            .client
            .comment_create()
            .id(created.id)
            .body(NewComment {
                body: "c".into(),
                pane: None,
            })
            .send()
            .await
            .expect("comment")
            .into_inner();

        let response = daemon
            .client
            .task_delete()
            .id(created.id)
            .send()
            .await
            .expect("delete");

        assert_eq!(response.status().as_u16(), 204);
        let (status, _) = error_response(daemon.client.task_get().id(created.id).send().await);
        assert_eq!(status, 404);
        let (status, _) =
            error_response(daemon.client.comment_delete().id(comment.id).send().await);
        assert_eq!(status, 404, "the comment went with the task");
    }
}
