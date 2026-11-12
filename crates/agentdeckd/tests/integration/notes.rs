//! Notes on workspaces and tasks: `/v1/notes`.

use agentdeck_client::Client;
use agentdeck_client::types::{NewNote, NewTask, NewWorkspace, Note, Task, Workspace};

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

async fn note_on_workspace(client: &Client, ws: &Workspace, body: &str) -> Note {
    client
        .note_create()
        .body(NewNote {
            body: body.into(),
            workspace_id: Some(ws.id),
            task_id: None,
        })
        .send()
        .await
        .expect("create a note")
        .into_inner()
}

async fn note_on_task(client: &Client, task: &Task, body: &str) -> Note {
    client
        .note_create()
        .body(NewNote {
            body: body.into(),
            workspace_id: None,
            task_id: Some(task.id),
        })
        .send()
        .await
        .expect("create a note")
        .into_inner()
}

mod post_v1_notes {
    use super::*;

    #[tokio::test]
    async fn should_leave_an_unread_note_on_a_workspace_201() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;

        let response = daemon
            .client
            .note_create()
            .body(NewNote {
                body: "check the logs".into(),
                workspace_id: Some(ws.id),
                task_id: None,
            })
            .send()
            .await
            .expect("create");

        assert_eq!(response.status().as_u16(), 201);
        let note = response.into_inner();
        assert_eq!(note.body, "check the logs");
        assert_eq!(note.workspace_id, Some(ws.id));
        assert_eq!(note.task_id, None);
        assert_eq!(note.read_at, None);
    }

    #[tokio::test]
    async fn should_reject_a_note_on_both_a_workspace_and_a_task_400() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;
        let task = task(&daemon.client).await;

        let (status, error) = error_response(
            daemon
                .client
                .note_create()
                .body(NewNote {
                    body: "n".into(),
                    workspace_id: Some(ws.id),
                    task_id: Some(task.id),
                })
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("invalid_target"));
    }

    #[tokio::test]
    async fn should_reject_a_note_on_neither_400() {
        let daemon = TestDaemon::start().await;

        let (status, error) = error_response(
            daemon
                .client
                .note_create()
                .body(NewNote {
                    body: "n".into(),
                    workspace_id: None,
                    task_id: None,
                })
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("invalid_target"));
    }

    #[tokio::test]
    async fn should_reject_an_unknown_task_400() {
        let daemon = TestDaemon::start().await;

        let (status, error) = error_response(
            daemon
                .client
                .note_create()
                .body(NewNote {
                    body: "n".into(),
                    workspace_id: None,
                    task_id: Some(uuid::Uuid::now_v7().into()),
                })
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(error.error_code.as_deref(), Some("unknown_task"));
    }
}

mod get_v1_notes {
    use super::*;

    async fn list(request: agentdeck_client::builder::NoteList<'_>) -> Vec<uuid::Uuid> {
        request
            .send()
            .await
            .expect("list")
            .into_inner()
            .into_iter()
            .map(|n| n.id)
            .collect()
    }

    #[tokio::test]
    async fn should_filter_by_workspace_task_and_unread_200() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;
        let task = task(&daemon.client).await;
        let on_ws = note_on_workspace(&daemon.client, &ws, "ws").await;
        let on_task = note_on_task(&daemon.client, &task, "task").await;
        daemon
            .client
            .note_update()
            .id(on_task.id)
            .body_map(|patch| patch.read(Some(true)))
            .send()
            .await
            .expect("mark read");

        assert_eq!(
            list(daemon.client.note_list()).await,
            vec![on_ws.id, on_task.id]
        );
        assert_eq!(
            list(daemon.client.note_list().workspace(ws.id)).await,
            vec![on_ws.id]
        );
        assert_eq!(
            list(daemon.client.note_list().task(task.id)).await,
            vec![on_task.id]
        );
        assert_eq!(
            list(daemon.client.note_list().unread(true)).await,
            vec![on_ws.id]
        );
        assert_eq!(
            list(daemon.client.note_list().unread(false)).await,
            vec![on_task.id]
        );
    }
}

mod patch_v1_notes_id {
    use super::*;

    #[tokio::test]
    async fn should_mark_read_by_a_pane_and_back_to_unread_200() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;
        let note = note_on_workspace(&daemon.client, &ws, "n").await;
        let pane = uuid::Uuid::now_v7();

        let read = daemon
            .client
            .note_update()
            .id(note.id)
            .body_map(|patch| patch.read(Some(true)).read_by_pane(Some(pane.into())))
            .send()
            .await
            .expect("mark read")
            .into_inner();
        let unread = daemon
            .client
            .note_update()
            .id(note.id)
            .body_map(|patch| patch.read(Some(false)))
            .send()
            .await
            .expect("mark unread")
            .into_inner();

        assert!(read.read_at.is_some());
        assert_eq!(read.read_by_pane, Some(pane.into()));
        assert_eq!(unread.read_at, None);
        assert_eq!(unread.read_by_pane, None);
    }

    #[tokio::test]
    async fn should_edit_the_body_200() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;
        let note = note_on_workspace(&daemon.client, &ws, "old").await;

        let edited = daemon
            .client
            .note_update()
            .id(note.id)
            .body_map(|patch| patch.body(Some("new".to_owned())))
            .send()
            .await
            .expect("edit")
            .into_inner();

        assert_eq!(edited.body, "new");
        assert_eq!(edited.read_at, None);
    }
}

mod delete_v1_notes_id {
    use super::*;

    #[tokio::test]
    async fn should_remove_the_note_204() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;
        let note = note_on_workspace(&daemon.client, &ws, "n").await;

        let response = daemon
            .client
            .note_delete()
            .id(note.id)
            .send()
            .await
            .expect("delete");

        assert_eq!(response.status().as_u16(), 204);
        let notes = daemon
            .client
            .note_list()
            .send()
            .await
            .expect("list")
            .into_inner();
        assert!(notes.is_empty());
    }

    #[tokio::test]
    async fn should_answer_not_found_for_an_unknown_note_404() {
        let daemon = TestDaemon::start().await;

        let (status, _) = error_response(
            daemon
                .client
                .note_delete()
                .id(uuid::Uuid::now_v7())
                .send()
                .await,
        );

        assert_eq!(status, 404);
    }
}
