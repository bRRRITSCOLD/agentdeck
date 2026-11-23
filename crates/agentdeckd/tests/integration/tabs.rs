//! Tabs and their layout trees: `/v1/workspaces/{id}/tabs` and `/v1/tabs/{id}`.

use agentdeck_client::Client;
use agentdeck_client::types::{NewTab, NewWorkspace, Tab, Workspace};
use serde_json::json;

use crate::support::{TestDaemon, error_response};

async fn workspace(client: &Client) -> Workspace {
    client
        .workspace_create()
        .body(NewWorkspace { name: "ws".into() })
        .send()
        .await
        .expect("create a workspace")
        .into_inner()
}

async fn tab(client: &Client, workspace: &Workspace, title: &str) -> Tab {
    client
        .tab_create()
        .id(workspace.id)
        .body(NewTab {
            title: title.into(),
            layout: None,
        })
        .send()
        .await
        .expect("create a tab")
        .into_inner()
}

/// A two-pane split, through JSON so the test reads like the spec's shape.
fn split(ratio: f64) -> serde_json::Value {
    json!({
        "split": "h",
        "ratio": ratio,
        "a": {"pane": uuid::Uuid::now_v7()},
        "b": {"pane": uuid::Uuid::now_v7()},
    })
}

mod post_v1_workspaces_id_tabs {
    use super::*;

    #[tokio::test]
    async fn should_append_tabs_in_order_with_no_layout_201() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;

        let first = tab(&daemon.client, &ws, "one").await;
        let second = tab(&daemon.client, &ws, "two").await;

        assert_eq!((first.position, second.position), (0, 1));
        assert_eq!(first.workspace_id, ws.id);
        assert_eq!(first.layout, None);
    }

    #[tokio::test]
    async fn should_keep_the_layout_it_was_given_201() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;
        let layout = serde_json::from_value(split(0.25)).expect("a layout");

        let response = daemon
            .client
            .tab_create()
            .id(ws.id)
            .body(NewTab {
                title: "split".into(),
                layout: Some(layout),
            })
            .send()
            .await
            .expect("create");

        assert_eq!(response.status().as_u16(), 201);
        let tab = response.into_inner();
        assert_eq!(serde_json::to_value(&tab.layout).unwrap()["ratio"], 0.25);
    }

    #[tokio::test]
    async fn should_reject_a_ratio_outside_zero_to_one_400() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;

        let response = reqwest::Client::new()
            .post(format!(
                "{}/v1/workspaces/{}/tabs",
                daemon.base_url(),
                ws.id
            ))
            .bearer_auth(&daemon.info.token)
            .json(&json!({"title": "bad", "layout": split(1.5)}))
            .send()
            .await
            .expect("a response");

        assert_eq!(response.status().as_u16(), 400);
    }

    #[tokio::test]
    async fn should_answer_not_found_for_an_unknown_workspace_404() {
        let daemon = TestDaemon::start().await;

        let (status, error) = error_response(
            daemon
                .client
                .tab_create()
                .id(uuid::Uuid::now_v7())
                .body(NewTab {
                    title: "t".into(),
                    layout: None,
                })
                .send()
                .await,
        );

        assert_eq!(status, 404);
        assert_eq!(error.error_code.as_deref(), Some("not_found"));
    }
}

mod get_v1_workspaces_id_tabs {
    use super::*;

    #[tokio::test]
    async fn should_list_only_this_workspaces_tabs_by_position_200() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;
        let other = workspace(&daemon.client).await;
        let one = tab(&daemon.client, &ws, "one").await;
        let two = tab(&daemon.client, &ws, "two").await;
        tab(&daemon.client, &other, "elsewhere").await;
        daemon
            .client
            .tab_update()
            .id(one.id)
            .body_map(|patch| patch.position(Some(5)))
            .send()
            .await
            .expect("move");

        let tabs = daemon
            .client
            .tab_list()
            .id(ws.id)
            .send()
            .await
            .expect("list")
            .into_inner();

        assert_eq!(
            tabs.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![two.id, one.id]
        );
    }
}

mod patch_v1_tabs_id {
    use super::*;

    #[tokio::test]
    async fn should_retitle_and_relayout_200() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;
        let created = tab(&daemon.client, &ws, "old").await;
        let layout: agentdeck_core::Layout = serde_json::from_value(split(0.5)).expect("a layout");

        let updated = daemon
            .client
            .tab_update()
            .id(created.id)
            .body_map(|patch| {
                patch
                    .title(Some("new".to_owned()))
                    .layout(Some(layout.clone()))
            })
            .send()
            .await
            .expect("update")
            .into_inner();

        assert_eq!(updated.title, "new");
        assert_eq!(updated.layout, Some(layout));
        assert_eq!(updated.position, created.position);
    }

    #[tokio::test]
    async fn should_answer_not_found_for_an_unknown_tab_404() {
        let daemon = TestDaemon::start().await;

        let (status, _) = error_response(
            daemon
                .client
                .tab_update()
                .id(uuid::Uuid::now_v7())
                .body_map(|patch| patch.title(Some("x".to_owned())))
                .send()
                .await,
        );

        assert_eq!(status, 404);
    }
}

mod delete_v1_tabs_id {
    use super::*;

    #[tokio::test]
    async fn should_remove_the_tab_204() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;
        let created = tab(&daemon.client, &ws, "gone").await;

        let response = daemon
            .client
            .tab_delete()
            .id(created.id)
            .send()
            .await
            .expect("delete");

        assert_eq!(response.status().as_u16(), 204);
        let tabs = daemon
            .client
            .tab_list()
            .id(ws.id)
            .send()
            .await
            .expect("list")
            .into_inner();
        assert!(tabs.is_empty());
    }

    #[tokio::test]
    async fn should_answer_not_found_for_an_unknown_tab_404() {
        let daemon = TestDaemon::start().await;

        let (status, _) = error_response(
            daemon
                .client
                .tab_delete()
                .id(uuid::Uuid::now_v7())
                .send()
                .await,
        );

        assert_eq!(status, 404);
    }
}
