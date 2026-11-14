//! Task comments: `/v1/tasks/{id}/comments` and `/v1/comments/{id}`.

use agentdeck_client::Client;
use agentdeck_client::types::{Comment, NewComment, NewTask, Task};
use agentdeck_core::ws::{Entity, ServerMessage};
use agentdeck_core::{Agent, CommentAuthor, PaneId, TaskId};

use crate::support::terminal::{Place, sh};
use crate::support::ws::Ws;
use crate::support::{TestDaemon, error_response};

async fn task(client: &Client) -> Task {
    client
        .task_create()
        .body(NewTask {
            title: "t".into(),
            body: None,
            status: None,
            workspace_id: None,
        })
        .send()
        .await
        .expect("create a task")
        .into_inner()
}

async fn comment(client: &Client, task: TaskId, body: &str, pane: Option<PaneId>) -> Comment {
    client
        .comment_create()
        .id(task)
        .body(NewComment {
            body: body.into(),
            pane,
        })
        .send()
        .await
        .expect("add a comment")
        .into_inner()
}

async fn list(client: &Client, task: TaskId) -> Vec<Comment> {
    client
        .comment_list()
        .id(task)
        .send()
        .await
        .expect("list comments")
        .into_inner()
}

mod post_v1_tasks_id_comments {
    use super::*;

    #[tokio::test]
    async fn should_add_the_users_comment_201() {
        let daemon = TestDaemon::start().await;
        let task = task(&daemon.client).await;

        let response = daemon
            .client
            .comment_create()
            .id(task.id)
            .body(NewComment {
                body: "Also check Firefox".into(),
                pane: None,
            })
            .send()
            .await
            .expect("add");

        assert_eq!(response.status().as_u16(), 201);
        let comment = response.into_inner();
        assert_eq!(comment.task_id, task.id);
        assert_eq!(comment.body, "Also check Firefox");
        assert_eq!(comment.author, CommentAuthor::User);
        assert_eq!((comment.author_agent, comment.author_pane), (None, None));
        assert_eq!(
            (comment.delivered_at, comment.delivered_to_pane),
            (None, None)
        );
        assert_eq!(
            comment.seen_at.as_ref(),
            Some(&comment.created_at),
            "yours is seen"
        );
    }

    #[tokio::test]
    async fn should_author_it_by_the_panes_agent_201() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client).await;
        let pane = place.launch(&daemon.client, Agent::Claude, None).await;

        let comment = comment(&daemon.client, task.id, "PR opened", Some(pane.id)).await;

        assert_eq!(comment.author, CommentAuthor::Agent);
        assert_eq!(comment.author_agent, Some(Agent::Claude));
        assert_eq!(comment.author_pane, Some(pane.id));
        assert_eq!(comment.seen_at, None, "an agent's is new to the user");
    }

    #[tokio::test]
    async fn should_send_changed_for_the_task_when_an_agent_comments_201() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client).await;
        let pane = place.spawn(&daemon.client, &sh("sleep 60")).await;
        let mut ws = Ws::connect(&daemon).await;

        comment(&daemon.client, task.id, "PR opened", Some(pane.id)).await;

        ws.expect(ServerMessage::Changed {
            entity: Entity::Task,
            id: task.id.to_string(),
        })
        .await;
    }

    #[tokio::test]
    async fn should_name_no_agent_for_a_shell_pane_201() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client).await;
        let pane = place.spawn(&daemon.client, &sh("sleep 60")).await;

        let comment = comment(&daemon.client, task.id, "from a shell", Some(pane.id)).await;

        assert_eq!(comment.author, CommentAuthor::Agent);
        assert_eq!(comment.author_agent, None);
        assert_eq!(comment.author_pane, Some(pane.id));
    }

    #[tokio::test]
    async fn should_send_changed_for_the_comment_201() {
        let daemon = TestDaemon::start().await;
        let task = task(&daemon.client).await;
        let mut ws = Ws::connect(&daemon).await;

        let comment = comment(&daemon.client, task.id, "c", None).await;

        ws.expect(ServerMessage::Changed {
            entity: Entity::Comment,
            id: comment.id.to_string(),
        })
        .await;
    }

    #[tokio::test]
    async fn should_reject_an_unknown_pane_400() {
        let daemon = TestDaemon::start().await;
        let task = task(&daemon.client).await;

        let (status, error) = error_response(
            daemon
                .client
                .comment_create()
                .id(task.id)
                .body(NewComment {
                    body: "c".into(),
                    pane: Some(uuid::Uuid::now_v7().into()),
                })
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("unknown_pane"));
    }

    #[tokio::test]
    async fn should_answer_not_found_for_an_unknown_task_404() {
        let daemon = TestDaemon::start().await;

        let (status, error) = error_response(
            daemon
                .client
                .comment_create()
                .id(uuid::Uuid::now_v7())
                .body(NewComment {
                    body: "c".into(),
                    pane: None,
                })
                .send()
                .await,
        );

        assert_eq!(status, 404);
        assert_eq!(error.error_code.as_deref(), Some("not_found"));
    }
}

mod get_v1_tasks_id_comments {
    use super::*;

    #[tokio::test]
    async fn should_list_only_the_tasks_comments_oldest_first_200() {
        let daemon = TestDaemon::start().await;
        let (one, other) = (task(&daemon.client).await, task(&daemon.client).await);
        let first = comment(&daemon.client, one.id, "first", None).await;
        comment(&daemon.client, other.id, "elsewhere", None).await;
        let second = comment(&daemon.client, one.id, "second", None).await;

        let ids: Vec<_> = list(&daemon.client, one.id)
            .await
            .into_iter()
            .map(|c| c.id)
            .collect();

        assert_eq!(ids, [first.id, second.id]);
    }

    #[tokio::test]
    async fn should_keep_an_agents_comment_once_its_pane_is_gone_200() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let task = task(&daemon.client).await;
        let pane = place.spawn(&daemon.client, &sh("sleep 60")).await;
        let written = comment(&daemon.client, task.id, "done", Some(pane.id)).await;
        (daemon.client.pane_delete().id(pane.id).send())
            .await
            .expect("delete the pane");

        let listed = list(&daemon.client, task.id).await;

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, written.id);
        assert_eq!(listed[0].author, CommentAuthor::Agent);
        assert_eq!(listed[0].author_pane, None);
    }

    #[tokio::test]
    async fn should_answer_not_found_for_an_unknown_task_404() {
        let daemon = TestDaemon::start().await;

        let (status, _) = error_response(
            daemon
                .client
                .comment_list()
                .id(uuid::Uuid::now_v7())
                .send()
                .await,
        );

        assert_eq!(status, 404);
    }
}

mod post_v1_tasks_id_comments_seen {
    use super::*;

    async fn unseen(client: &Client, task: TaskId) -> u32 {
        client
            .task_get()
            .id(task)
            .send()
            .await
            .expect("read the task")
            .into_inner()
            .unseen_agent_comments
    }

    #[tokio::test]
    async fn should_mark_every_agent_comment_on_the_task_seen_204() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let (task, other) = (task(&daemon.client).await, task(&daemon.client).await);
        let pane = place.spawn(&daemon.client, &sh("sleep 60")).await;
        let first = comment(&daemon.client, task.id, "PR opened", Some(pane.id)).await;
        comment(&daemon.client, task.id, "looks good", None).await;
        comment(&daemon.client, task.id, "checks green", Some(pane.id)).await;
        comment(&daemon.client, other.id, "elsewhere", Some(pane.id)).await;
        assert_eq!(unseen(&daemon.client, task.id).await, 2);
        let mut ws = Ws::connect(&daemon).await;

        let response = daemon
            .client
            .comment_seen()
            .id(task.id)
            .send()
            .await
            .expect("mark seen");

        assert_eq!(response.status().as_u16(), 204);
        assert_eq!(unseen(&daemon.client, task.id).await, 0);
        assert_eq!(unseen(&daemon.client, other.id).await, 1);
        let thread = list(&daemon.client, task.id).await;
        assert!(thread.iter().all(|c| c.seen_at.is_some()), "{thread:?}");
        ws.expect(ServerMessage::Changed {
            entity: Entity::Comment,
            id: first.id.to_string(),
        })
        .await;
    }

    #[tokio::test]
    async fn should_answer_not_found_for_an_unknown_task_404() {
        let daemon = TestDaemon::start().await;

        let (status, _) = error_response(
            daemon
                .client
                .comment_seen()
                .id(uuid::Uuid::now_v7())
                .send()
                .await,
        );

        assert_eq!(status, 404);
    }
}

mod delete_v1_comments_id {
    use super::*;

    #[tokio::test]
    async fn should_remove_the_comment_204() {
        let daemon = TestDaemon::start().await;
        let task = task(&daemon.client).await;
        let comment = comment(&daemon.client, task.id, "c", None).await;
        let mut ws = Ws::connect(&daemon).await;

        let response = daemon
            .client
            .comment_delete()
            .id(comment.id)
            .send()
            .await
            .expect("delete");

        assert_eq!(response.status().as_u16(), 204);
        assert!(list(&daemon.client, task.id).await.is_empty());
        ws.expect(ServerMessage::Changed {
            entity: Entity::Comment,
            id: comment.id.to_string(),
        })
        .await;
    }

    #[tokio::test]
    async fn should_answer_not_found_for_an_unknown_comment_404() {
        let daemon = TestDaemon::start().await;

        let (status, _) = error_response(
            daemon
                .client
                .comment_delete()
                .id(uuid::Uuid::now_v7())
                .send()
                .await,
        );

        assert_eq!(status, 404);
    }
}
