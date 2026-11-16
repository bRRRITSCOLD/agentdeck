//! `agentdeck_client::panes`: the pane operations that take more than one call, against a
//! real daemon. Each must leave every tab's layout naming only panes that exist.

use agentdeck_client::Client;
use agentdeck_client::panes::{
    PaneAt, PaneOpError, close_pane, link_task, new_shell_tab, pane_if_any, respawn, split_pane,
};
use agentdeck_client::types::{NewWorkspace, Tab, Workspace};
use agentdeck_core::{Layout, LayoutError, PaneId, PaneKind, Ratio, SplitDirection, TabId};

use crate::support::TestDaemon;

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

async fn tabs(client: &Client, workspace: &Workspace) -> Vec<Tab> {
    client
        .tab_list()
        .id(workspace.id)
        .send()
        .await
        .expect("list tabs")
        .into_inner()
}

async fn layout(client: &Client, workspace: &Workspace, tab: TabId) -> Option<Layout> {
    let tabs = tabs(client, workspace).await;
    tabs.into_iter()
        .find(|t| t.id == tab)
        .and_then(|t| t.layout)
}

/// Every pane the workspace's layouts name, and whether it still exists.
async fn named_panes(client: &Client, workspace: &Workspace) -> Vec<(PaneId, bool)> {
    let mut named = Vec::new();
    for tab in tabs(client, workspace).await {
        for pane in tab.layout.map(|l| l.panes()).unwrap_or_default() {
            let exists = pane_if_any(client, pane).await.expect("read the pane");
            named.push((pane, exists.is_some()));
        }
    }
    named
}

/// A workspace with one shell tab: the tab and its pane.
async fn shell_tab(client: &Client) -> (Workspace, TabId, PaneId) {
    let ws = workspace(client).await;
    let tab = new_shell_tab(client, ws.id, ws.dir.clone())
        .await
        .expect("open a shell tab");
    let Some(Layout::Pane { pane }) = layout(client, &ws, tab).await else {
        panic!("a one-pane layout");
    };
    (ws, tab, pane)
}

fn at(ws: &Workspace, tab: TabId, pane: PaneId) -> PaneAt {
    PaneAt {
        workspace: ws.id,
        tab,
        pane,
    }
}

mod new_shell_tab {
    use super::*;

    #[tokio::test]
    async fn should_open_a_tab_whose_layout_is_one_shell_in_the_dir() {
        let daemon = TestDaemon::start().await;

        let (ws, _, pane) = shell_tab(&daemon.client).await;

        let pane = pane_if_any(&daemon.client, pane)
            .await
            .expect("read the pane")
            .expect("the pane exists");
        assert_eq!((pane.kind, pane.cwd), (PaneKind::Shell, ws.dir));
    }

    #[tokio::test]
    async fn should_delete_the_tab_again_when_the_shell_cant_start() {
        let daemon = TestDaemon::start().await;
        let ws = workspace(&daemon.client).await;

        let err = new_shell_tab(&daemon.client, ws.id, "/no/such/dir".into())
            .await
            .unwrap_err();

        let PaneOpError::Api(err) = err else {
            panic!("an API error, got {err:?}");
        };
        assert_eq!(err.code.as_deref(), Some("invalid_dir"));
        assert!(tabs(&daemon.client, &ws).await.is_empty());
    }
}

mod split_pane {
    use super::*;

    #[tokio::test]
    async fn should_put_a_new_shell_on_the_second_side() {
        let daemon = TestDaemon::start().await;
        let (ws, tab, first) = shell_tab(&daemon.client).await;

        let new = split_pane(
            &daemon.client,
            at(&ws, tab, first),
            SplitDirection::Vertical,
            ws.dir.clone(),
        )
        .await
        .expect("split");

        assert_eq!(
            layout(&daemon.client, &ws, tab).await,
            Some(Layout::Split {
                split: SplitDirection::Vertical,
                ratio: Ratio::HALF,
                a: Box::new(Layout::single(first)),
                b: Box::new(Layout::single(new)),
            })
        );
        assert!(
            named_panes(&daemon.client, &ws)
                .await
                .iter()
                .all(|(_, exists)| *exists)
        );
    }

    #[tokio::test]
    async fn should_keep_the_layout_when_the_pane_is_no_longer_in_it() {
        let daemon = TestDaemon::start().await;
        let (ws, tab, first) = shell_tab(&daemon.client).await;
        let gone = PaneId::from(uuid::Uuid::now_v7());

        let err = split_pane(
            &daemon.client,
            at(&ws, tab, gone),
            SplitDirection::Horizontal,
            ws.dir.clone(),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, PaneOpError::Layout(LayoutError::PaneNotFound(p)) if p == gone));
        assert_eq!(
            layout(&daemon.client, &ws, tab).await,
            Some(Layout::single(first))
        );
    }
}

mod close_pane {
    use super::*;

    #[tokio::test]
    async fn should_give_the_sibling_the_space_and_delete_the_pane() {
        let daemon = TestDaemon::start().await;
        let (ws, tab, first) = shell_tab(&daemon.client).await;
        let second = split_pane(
            &daemon.client,
            at(&ws, tab, first),
            SplitDirection::Horizontal,
            ws.dir.clone(),
        )
        .await
        .expect("split");

        close_pane(&daemon.client, at(&ws, tab, second))
            .await
            .expect("close");

        assert_eq!(
            layout(&daemon.client, &ws, tab).await,
            Some(Layout::single(first))
        );
        assert_eq!(named_panes(&daemon.client, &ws).await, [(first, true)]);
        let closed = pane_if_any(&daemon.client, second).await.expect("read");
        assert!(closed.is_none());
    }

    #[tokio::test]
    async fn should_be_done_when_the_pane_is_already_closed() {
        let daemon = TestDaemon::start().await;
        let (ws, tab, first) = shell_tab(&daemon.client).await;
        let second = split_pane(
            &daemon.client,
            at(&ws, tab, first),
            SplitDirection::Horizontal,
            ws.dir.clone(),
        )
        .await
        .expect("split");
        close_pane(&daemon.client, at(&ws, tab, second))
            .await
            .expect("close");

        close_pane(&daemon.client, at(&ws, tab, second))
            .await
            .expect("closing again is done");

        assert_eq!(named_panes(&daemon.client, &ws).await, [(first, true)]);
    }

    #[tokio::test]
    async fn should_be_done_when_the_tab_went_with_the_last_pane() {
        let daemon = TestDaemon::start().await;
        let (ws, tab, pane) = shell_tab(&daemon.client).await;
        close_pane(&daemon.client, at(&ws, tab, pane))
            .await
            .expect("close");

        close_pane(&daemon.client, at(&ws, tab, pane))
            .await
            .expect("closing again is done");
    }

    #[tokio::test]
    async fn should_delete_the_tab_with_its_last_pane() {
        let daemon = TestDaemon::start().await;
        let (ws, tab, pane) = shell_tab(&daemon.client).await;

        close_pane(&daemon.client, at(&ws, tab, pane))
            .await
            .expect("close");

        assert!(tabs(&daemon.client, &ws).await.is_empty());
        let closed = pane_if_any(&daemon.client, pane).await.expect("read");
        assert!(closed.is_none());
    }
}

mod pane_if_any {
    use super::*;

    #[tokio::test]
    async fn should_be_none_for_a_pane_that_does_not_exist() {
        let daemon = TestDaemon::start().await;

        let pane = pane_if_any(&daemon.client, PaneId::from(uuid::Uuid::now_v7()))
            .await
            .expect("not found is not an error");

        assert!(pane.is_none());
    }
}

mod link_task {
    use agentdeck_client::types::NewTask;

    use super::*;

    #[tokio::test]
    async fn should_link_the_pane_to_the_task_then_unlink_it() {
        let daemon = TestDaemon::start().await;
        let (ws, _, pane) = shell_tab(&daemon.client).await;
        let task = daemon
            .client
            .task_create()
            .body(NewTask {
                title: "t".into(),
                body: None,
                status: None,
                workspace_id: Some(ws.id),
            })
            .send()
            .await
            .expect("create a task")
            .into_inner();

        let linked = link_task(&daemon.client, pane, Some(task.id))
            .await
            .expect("link");
        assert_eq!(linked.task_id, Some(task.id));

        let unlinked = link_task(&daemon.client, pane, None).await.expect("unlink");
        assert_eq!(unlinked.task_id, None);
    }

    #[tokio::test]
    async fn should_carry_the_daemons_refusal() {
        let daemon = TestDaemon::start().await;
        let (_, _, pane) = shell_tab(&daemon.client).await;

        let err = link_task(&daemon.client, pane, Some(uuid::Uuid::now_v7().into()))
            .await
            .expect_err("an unknown task");

        assert_eq!(err.code.as_deref(), Some("unknown_task"));
    }
}

mod respawn {
    use agentdeck_client::types::{NewPane, PaneRun};
    use agentdeck_core::AgentState;

    use super::*;
    use crate::support::terminal::in_state;

    #[tokio::test]
    async fn should_start_an_exited_pane_again_as_a_shell() {
        let daemon = TestDaemon::start().await;
        let (ws, tab, _) = shell_tab(&daemon.client).await;
        let done = daemon
            .client
            .pane_create()
            .body(NewPane {
                tab_id: tab,
                kind: PaneKind::Shell,
                agent: None,
                argv: Some(vec!["true".into()]),
                cwd: ws.dir.clone(),
                task_id: None,
            })
            .send()
            .await
            .expect("spawn a pane")
            .into_inner();
        in_state(&daemon.client, done.id, AgentState::Exited).await;

        let again = respawn(&daemon.client, done.id, PaneRun::Shell)
            .await
            .expect("respawn");

        assert_eq!((again.id, again.kind), (done.id, PaneKind::Shell));
        assert_ne!(again.state, AgentState::Exited);
    }

    #[tokio::test]
    async fn should_carry_the_daemons_refusal() {
        let daemon = TestDaemon::start().await;
        let (_, _, pane) = shell_tab(&daemon.client).await;

        let err = respawn(&daemon.client, pane, PaneRun::Shell)
            .await
            .expect_err("a running pane");

        assert_eq!(err.code.as_deref(), Some("running"));
    }
}
