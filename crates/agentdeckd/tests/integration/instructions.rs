//! Workspace instructions: `/v1/workspaces/{id}/instructions` and `/v1/instructions/{id}`.

use agentdeck_client::Client;
use agentdeck_client::types::{Instruction, NewInstruction, NewWorkspace, Workspace};
use agentdeck_core::WorkspaceId;
use agentdeck_core::ws::{Entity, ServerMessage};

use crate::support::ws::Ws;
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

async fn instruction(client: &Client, workspace: WorkspaceId, body: &str) -> Instruction {
    client
        .instruction_create()
        .id(workspace)
        .body(NewInstruction { body: body.into() })
        .send()
        .await
        .expect("add an instruction")
        .into_inner()
}

async fn list(client: &Client, workspace: WorkspaceId) -> Vec<Instruction> {
    client
        .instruction_list()
        .id(workspace)
        .send()
        .await
        .expect("list instructions")
        .into_inner()
}

mod post_v1_workspaces_id_instructions {
    use super::*;

    #[tokio::test]
    async fn should_add_an_instruction_to_the_workspace_201() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;
        let mut events = Ws::connect(&daemon).await;

        let response = daemon
            .client
            .instruction_create()
            .id(ws.id)
            .body(NewInstruction {
                body: "Push only after 20 green runs".into(),
            })
            .send()
            .await
            .expect("add");

        assert_eq!(response.status().as_u16(), 201);
        let added = response.into_inner();
        assert_eq!(added.workspace_id, ws.id);
        assert_eq!(added.body, "Push only after 20 green runs");
        assert_eq!(added.updated_at, added.created_at);
        events
            .expect(ServerMessage::Changed {
                entity: Entity::Instruction,
                id: added.id.to_string(),
            })
            .await;
    }

    #[tokio::test]
    async fn should_answer_not_found_for_an_unknown_workspace_404() {
        let daemon = TestDaemon::start().await;

        let (status, _) = error_response(
            daemon
                .client
                .instruction_create()
                .id(uuid::Uuid::now_v7())
                .body(NewInstruction { body: "i".into() })
                .send()
                .await,
        );

        assert_eq!(status, 404);
    }
}

mod get_v1_workspaces_id_instructions {
    use super::*;

    #[tokio::test]
    async fn should_list_only_the_workspaces_instructions_oldest_first_200() {
        let daemon = TestDaemon::start().await;
        let (one, other) = (
            workspace(&daemon.client).await,
            workspace(&daemon.client).await,
        );
        let first = instruction(&daemon.client, one.id, "first").await;
        instruction(&daemon.client, other.id, "elsewhere").await;
        let second = instruction(&daemon.client, one.id, "second").await;

        let ids: Vec<_> = list(&daemon.client, one.id)
            .await
            .into_iter()
            .map(|i| i.id)
            .collect();

        assert_eq!(ids, [first.id, second.id]);
    }

    #[tokio::test]
    async fn should_answer_not_found_for_an_unknown_workspace_404() {
        let daemon = TestDaemon::start().await;

        let (status, _) = error_response(
            daemon
                .client
                .instruction_list()
                .id(uuid::Uuid::now_v7())
                .send()
                .await,
        );

        assert_eq!(status, 404);
    }
}

mod patch_v1_instructions_id {
    use super::*;

    #[tokio::test]
    async fn should_edit_the_body_and_stamp_the_update_200() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;
        let added = instruction(&daemon.client, ws.id, "old").await;
        let mut events = Ws::connect(&daemon).await;

        let edited = daemon
            .client
            .instruction_update()
            .id(added.id)
            .body_map(|patch| patch.body(Some("new".to_owned())))
            .send()
            .await
            .expect("edit")
            .into_inner();

        assert_eq!(edited.body, "new");
        assert_eq!(edited.created_at, added.created_at);
        assert!(edited.updated_at > added.updated_at, "{edited:?}");
        assert_eq!(list(&daemon.client, ws.id).await[0].body, "new");
        events
            .expect(ServerMessage::Changed {
                entity: Entity::Instruction,
                id: added.id.to_string(),
            })
            .await;
    }

    #[tokio::test]
    async fn should_answer_not_found_for_an_unknown_instruction_404() {
        let daemon = TestDaemon::start().await;

        let (status, _) = error_response(
            daemon
                .client
                .instruction_update()
                .id(uuid::Uuid::now_v7())
                .body_map(|patch| patch.body(Some("x".to_owned())))
                .send()
                .await,
        );

        assert_eq!(status, 404);
    }
}

mod delete_v1_instructions_id {
    use super::*;

    #[tokio::test]
    async fn should_remove_the_instruction_204() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;
        let added = instruction(&daemon.client, ws.id, "i").await;

        let response = daemon
            .client
            .instruction_delete()
            .id(added.id)
            .send()
            .await
            .expect("delete");

        assert_eq!(response.status().as_u16(), 204);
        assert!(list(&daemon.client, ws.id).await.is_empty());
    }

    #[tokio::test]
    async fn should_answer_not_found_for_an_unknown_instruction_404() {
        let daemon = TestDaemon::start().await;

        let (status, _) = error_response(
            daemon
                .client
                .instruction_delete()
                .id(uuid::Uuid::now_v7())
                .send()
                .await,
        );

        assert_eq!(status, 404);
    }
}
