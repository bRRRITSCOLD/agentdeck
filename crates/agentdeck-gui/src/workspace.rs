//! The selected workspace (spec §11). For now it shows one pane: the first in its first tab
//! that has one. Tabs, split panes and pane headers follow in the next #12 PR.
//!
//! It reads the workspace's tabs when it opens and again whenever the daemon reports a tab
//! or pane `changed`, keeping the pane's terminal while that pane stays first.

use std::sync::Arc;

use agentdeck_client::types::Tab;
use agentdeck_client::ws;
use agentdeck_core::ws::{Entity as Changed, ServerMessage};
use agentdeck_core::{PaneId, WorkspaceId};
use brrritscold_core_gpui::ActiveTheme as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, Window, div,
};

use crate::api::Api;
use crate::connection::{Connection, DaemonEvent};
use crate::terminal::TerminalView;

/// What the view shows.
enum Content {
    /// Reading the tabs.
    Loading,
    /// No tab has a pane.
    Empty,
    /// The read failed; why.
    Failed(SharedString),
    /// The first pane.
    Pane(Entity<TerminalView>),
}

/// One workspace's panes.
pub struct WorkspaceView {
    workspace: WorkspaceId,
    api: Arc<dyn Api>,
    connection: Entity<Connection>,
    sender: ws::Sender,
    content: Content,
    /// The read in flight; replaced by a newer one.
    read: Option<Task<()>>,
    _events: Subscription,
}

impl WorkspaceView {
    /// `workspace`, read through `api` and attached over `connection`'s socket through
    /// `sender`. It starts reading at once.
    pub fn new(
        workspace: WorkspaceId,
        api: Arc<dyn Api>,
        connection: Entity<Connection>,
        sender: ws::Sender,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let events = cx.subscribe_in(&connection, window, |this, _, event, window, cx| {
            if let DaemonEvent::Message(ServerMessage::Changed {
                entity: Changed::Tab | Changed::Pane,
                ..
            }) = event
            {
                this.reread(window, cx);
            }
        });
        let mut view = WorkspaceView {
            workspace,
            api,
            connection,
            sender,
            content: Content::Loading,
            read: None,
            _events: events,
        };
        view.reread(window, cx);
        view
    }

    /// The workspace it shows.
    pub fn workspace(&self) -> WorkspaceId {
        self.workspace
    }

    /// The terminal it shows, if any.
    #[cfg(test)]
    pub fn terminal(&self) -> Option<&Entity<TerminalView>> {
        match &self.content {
            Content::Pane(view) => Some(view),
            _ => None,
        }
    }

    fn reread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reading = self.api.tabs(self.workspace, cx);
        self.read = Some(cx.spawn_in(window, async move |this, cx| {
            let result = reading.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.show(result, window, cx);
                cx.notify();
            });
        }));
    }

    fn show(
        &mut self,
        tabs: anyhow::Result<Vec<Tab>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tabs = match tabs {
            Ok(tabs) => tabs,
            Err(err) => {
                self.content = Content::Failed(format!("Couldn't read the tabs: {err:#}").into());
                return;
            }
        };
        let Some(pane) = first_pane(&tabs) else {
            self.content = Content::Empty;
            return;
        };
        if let Content::Pane(view) = &self.content
            && view.read(cx).pane() == pane
        {
            return;
        }
        let (connection, sender) = (self.connection.clone(), self.sender.clone());
        self.content =
            Content::Pane(cx.new(|cx| TerminalView::new(pane, &connection, sender, window, cx)));
    }

    fn message(&self, text: SharedString, cx: &mut Context<Self>) -> AnyElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().semantic().muted_foreground)
            .child(text)
            .into_any_element()
    }
}

/// The first pane of the first tab, by position, that has one.
fn first_pane(tabs: &[Tab]) -> Option<PaneId> {
    tabs.iter()
        .filter_map(|tab| Some((tab.position, tab.layout.as_ref()?.panes().first().copied()?)))
        .min_by_key(|(position, _)| *position)
        .map(|(_, pane)| pane)
}

impl Render for WorkspaceView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.content {
            Content::Loading => div().size_full().into_any_element(),
            Content::Empty => self.message("No panes in this workspace yet".into(), cx),
            Content::Failed(reason) => self.message(reason.clone(), cx),
            Content::Pane(view) => view.clone().into_any_element(),
        };
        div().flex_1().min_w_0().min_h_0().child(content)
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use agentdeck_client::ws::{Event, Remote};
    use brrritscold_core_gpui::Root;
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::connection::fake::FakeConnector;
    use crate::connection::{Connector, Status};
    use crate::overview::fixtures::{id, tab};

    struct Harness {
        api: Arc<FakeApi>,
        remote: Remote,
        _connection: Entity<Connection>,
        view: Entity<WorkspaceView>,
    }

    /// A window on workspace 1, whose tabs are `tabs`.
    fn open(tabs: Vec<Tab>, cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
        cx.update(brrritscold_core_gpui::init);
        cx.update(crate::terminal::init);
        let api = Arc::new(FakeApi::default());
        api.set_tabs(id(1), tabs);
        let connector = Rc::new(FakeConnector::default());
        let remote = connector.then_connect_to(api.clone());
        let connector: Rc<dyn Connector> = connector;
        let connection = cx.update(|cx| cx.new(|cx| Connection::new(connector, cx)));
        cx.run_until_parked();
        let Status::Connected(session) = connection.read_with(cx, |c, _| c.status().clone()) else {
            panic!("the fake connects");
        };
        let mut view = None;
        let window = cx.add_window(|window, cx| {
            let workspace = cx.new(|cx| {
                WorkspaceView::new(
                    id(1),
                    session.api,
                    connection.clone(),
                    session.sender,
                    window,
                    cx,
                )
            });
            view = Some(workspace.clone());
            Root::new(workspace, window, cx)
        });
        let cx = VisualTestContext::from_window(*window, cx).into_mut();
        cx.run_until_parked();
        let harness = Harness {
            api,
            remote,
            _connection: connection,
            view: view.unwrap(),
        };
        (harness, cx)
    }

    fn shown(harness: &Harness, cx: &mut VisualTestContext) -> Option<PaneId> {
        harness.view.read_with(cx, |view, cx| {
            view.terminal().map(|terminal| terminal.read(cx).pane())
        })
    }

    fn changed(harness: &Harness, entity: Changed, cx: &mut VisualTestContext) {
        harness
            .remote
            .events
            .try_send(Event::Message(ServerMessage::Changed {
                entity,
                id: "x".into(),
            }))
            .unwrap();
        cx.run_until_parked();
    }

    mod first_pane {
        use super::*;

        #[test]
        fn should_take_the_lowest_positioned_tab_with_a_pane() {
            let tabs = [
                tab(1, id(1), 2, Some(id(12))),
                tab(2, id(1), 0, None),
                tab(3, id(1), 1, Some(id(31))),
            ];

            assert_eq!(first_pane(&tabs), Some(id(31)));
        }

        #[test]
        fn should_find_none_when_no_tab_has_a_pane() {
            assert_eq!(first_pane(&[tab(1, id(1), 0, None)]), None);
        }
    }

    mod panes {
        use super::*;

        #[gpui::test]
        fn should_show_the_first_pane(cx: &mut TestAppContext) {
            let (harness, cx) = open(vec![tab(1, id(1), 0, Some(id(7)))], cx);

            assert_eq!(shown(&harness, cx), Some(id(7)));
        }

        #[gpui::test]
        fn should_show_no_terminal_when_no_tab_has_a_pane(cx: &mut TestAppContext) {
            let (harness, cx) = open(vec![tab(1, id(1), 0, None)], cx);

            assert_eq!(shown(&harness, cx), None);
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        #[gpui::test]
        fn should_keep_the_terminal_while_its_pane_stays_first(cx: &mut TestAppContext) {
            let (harness, cx) = open(vec![tab(1, id(1), 0, Some(id(7)))], cx);
            let before = harness
                .view
                .read_with(cx, |view, _| view.terminal().cloned());

            changed(&harness, Changed::Tab, cx);

            let after = harness
                .view
                .read_with(cx, |view, _| view.terminal().cloned());
            assert_eq!(before, after);
        }

        #[gpui::test]
        fn should_switch_terminals_when_another_pane_comes_first(cx: &mut TestAppContext) {
            let (harness, cx) = open(vec![tab(1, id(1), 0, Some(id(7)))], cx);
            harness
                .api
                .set_tabs(id(1), vec![tab(1, id(1), 0, Some(id(8)))]);

            changed(&harness, Changed::Pane, cx);

            assert_eq!(shown(&harness, cx), Some(id(8)));
        }

        #[gpui::test]
        fn should_not_read_again_for_other_changes(cx: &mut TestAppContext) {
            let (harness, cx) = open(vec![tab(1, id(1), 0, Some(id(7)))], cx);
            harness
                .api
                .set_tabs(id(1), vec![tab(1, id(1), 0, Some(id(8)))]);

            changed(&harness, Changed::Task, cx);

            assert_eq!(shown(&harness, cx), Some(id(7)));
        }
    }
}
