//! The selected workspace (spec §11), Penpot "Main window" `Centre`: a row of tabs over the
//! selected tab's split tree, a [`PaneView`] at each leaf.
//!
//! It reads the workspace's tabs, panes and linked tasks when it opens and again whenever the
//! daemon reports a tab, pane or task `changed`. Each shown pane keeps its view, and so its
//! terminal, across reads; leaving a tab drops its panes' views, which detaches them.

use std::collections::HashMap;
use std::sync::Arc;

use agentdeck_client::types::Tab;
use agentdeck_client::ws;
use agentdeck_core::ws::{Entity as Changed, ServerMessage};
use agentdeck_core::{Layout, PaneId, SplitDirection, TabId, WorkspaceId};
use brrritscold_core_gpui::{
    ActiveTheme as _, Separator, Tabs, TabsList, TabsListVariant, TabsTrigger,
};
use brrritscold_core_gpui_tokens::primitive::space;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Div, Entity, Focusable as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, Window, div,
    relative,
};

use crate::api::{Api, WorkspacePanes};
use crate::connection::{Connection, DaemonEvent};
use crate::pane::PaneView;

/// Where the read stands.
enum Status {
    /// Reading.
    Loading,
    /// The read failed; why.
    Failed(SharedString),
    /// Showing what was read.
    Ready,
}

/// One workspace's tabs and panes.
pub struct WorkspaceView {
    workspace: WorkspaceId,
    api: Arc<dyn Api>,
    connection: Entity<Connection>,
    sender: ws::Sender,
    status: Status,
    /// The last read.
    read: WorkspacePanes,
    /// The tab shown.
    selected: Option<TabId>,
    /// The selected tab's panes.
    panes: HashMap<PaneId, Entity<PaneView>>,
    /// The read in flight; replaced by a newer one.
    reading: Option<Task<()>>,
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
                entity: Changed::Tab | Changed::Pane | Changed::Task,
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
            status: Status::Loading,
            read: WorkspacePanes::default(),
            selected: None,
            panes: HashMap::new(),
            reading: None,
            _events: events,
        };
        view.reread(window, cx);
        view
    }

    /// The workspace it shows.
    pub fn workspace(&self) -> WorkspaceId {
        self.workspace
    }

    /// Shows the tab `tab`, if the workspace has it.
    pub fn select(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected != Some(tab) && self.read.tabs.iter().any(|t| t.id == tab) {
            self.selected = Some(tab);
            self.sync(window, cx);
            cx.notify();
        }
    }

    /// The tab it shows.
    #[cfg(test)]
    pub fn selected(&self) -> Option<TabId> {
        self.selected
    }

    /// The panes it shows, left to right and top to bottom.
    #[cfg(test)]
    pub fn shown(&self) -> Vec<Entity<PaneView>> {
        self.layout()
            .map(|layout| layout.panes())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|pane| self.panes.get(&pane).cloned())
            .collect()
    }

    fn reread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reading = self.api.panes(self.workspace, cx);
        self.reading = Some(cx.spawn_in(window, async move |this, cx| {
            let result = reading.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.show(result, window, cx);
                cx.notify();
            });
        }));
    }

    fn show(
        &mut self,
        read: anyhow::Result<WorkspacePanes>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut read = match read {
            Ok(read) => read,
            Err(err) => {
                self.status = Status::Failed(format!("Couldn't read the tabs: {err:#}").into());
                self.panes.clear();
                return;
            }
        };
        read.tabs.sort_by_key(|tab| tab.position);
        self.read = read;
        self.status = Status::Ready;
        let kept = self
            .selected
            .filter(|selected| self.read.tabs.iter().any(|tab| tab.id == *selected));
        self.selected = kept.or_else(|| self.read.tabs.first().map(|tab| tab.id));
        self.sync(window, cx);
    }

    /// The selected tab's layout, if it has a pane.
    fn layout(&self) -> Option<&Layout> {
        let selected = self.selected?;
        let tab = self.read.tabs.iter().find(|tab| tab.id == selected)?;
        tab.layout.as_ref()
    }

    /// Makes `panes` the selected tab's: keeps and updates the views of panes still in it,
    /// makes the new ones, and drops the rest. A tab shown afresh focuses its first pane.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.layout().map(Layout::panes).unwrap_or_default();
        let fresh = !wanted.iter().any(|pane| self.panes.contains_key(pane));
        let mut panes = HashMap::new();
        for id in &wanted {
            let Some(pane) = self.read.panes.iter().find(|pane| pane.id == *id).cloned() else {
                continue;
            };
            let task = (pane.task_id)
                .and_then(|task| self.read.tasks.iter().find(|t| t.id == task))
                .map(|task| SharedString::from(task.title.clone()));
            let view = match self.panes.remove(id) {
                Some(view) => {
                    view.update(cx, |view, cx| view.update(pane, task, cx));
                    view
                }
                None => {
                    let (connection, sender) = (&self.connection, self.sender.clone());
                    cx.new(|cx| PaneView::new(pane, task, connection, sender, cx))
                }
            };
            panes.insert(*id, view);
        }
        self.panes = panes;
        if fresh && let Some(first) = wanted.first().and_then(|pane| self.panes.get(pane)) {
            window.focus(&first.focus_handle(cx), cx);
        }
    }

    fn message(&self, text: impl Into<SharedString>, cx: &mut Context<Self>) -> AnyElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().semantic().muted_foreground)
            .child(text.into())
            .into_any_element()
    }

    /// Penpot `TabsRow`: a `line` tab list, `space.4` in from each side.
    fn tabs_row(&self, cx: &mut Context<Self>) -> Div {
        let list = self.read.tabs.iter().fold(
            TabsList::new().variant(TabsListVariant::Line),
            |list, tab: &Tab| list.trigger(TabsTrigger::new(tab.id.to_string(), tab.title.clone())),
        );
        let on_select = cx.listener(|this, value: &SharedString, window, cx| {
            if let Ok(tab) = value.parse() {
                this.select(tab, window, cx);
            }
        });
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(space::S2)
            .px(space::S4)
            .child(
                Tabs::new("workspace-tabs")
                    .when_some(self.selected, |tabs, tab| tabs.value(tab.to_string()))
                    .list(list)
                    .on_value_change(on_select),
            )
    }

    /// The subtree `layout`, taking `grow` shares of its split's free space.
    fn split(&self, layout: &Layout, grow: f32) -> Div {
        let mut side = div().flex().min_w_0().min_h_0().flex_basis(relative(0.));
        side.style().flex_grow = Some(grow);
        match layout {
            Layout::Pane { pane } => side.children(self.panes.get(pane).cloned()),
            // Penpot `Panes` and `Stack`: the sides `space.2` apart.
            Layout::Split { split, ratio, a, b } => side
                .gap(space::S2)
                .when(*split == SplitDirection::Vertical, |side| side.flex_col())
                .child(self.split(a, ratio.get()))
                .child(self.split(b, 1. - ratio.get())),
        }
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = div().flex_1().min_w_0().min_h_0().flex().flex_col();
        match &self.status {
            Status::Loading => view,
            Status::Failed(reason) => view.child(self.message(reason.clone(), cx)),
            Status::Ready if self.read.tabs.is_empty() => {
                view.child(self.message("No tabs in this workspace yet", cx))
            }
            // Penpot `Centre`: TabsRow / Separator / Panes, the panes `space.2` in.
            Status::Ready => {
                view.child(self.tabs_row(cx))
                    .child(Separator::new())
                    .child(match self.layout() {
                        Some(layout) => self.split(layout, 1.).p(space::S2).into_any_element(),
                        None => self.message("No panes in this tab yet", cx),
                    })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use agentdeck_client::types::Pane;
    use agentdeck_client::ws::{Event, Remote};
    use agentdeck_core::{AgentState, Ratio, TaskKind};
    use brrritscold_core_gpui::{Root, Theme, ThemePreference};
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::connection::fake::FakeConnector;
    use crate::connection::{Connector, Status as Connected};
    use crate::overview::fixtures::{id, tab, task};
    use crate::pane::fixtures::shell_pane;

    struct Harness {
        api: Arc<FakeApi>,
        remote: Remote,
        _connection: Entity<Connection>,
        view: Entity<WorkspaceView>,
    }

    impl Harness {
        /// A window on workspace 1, whose tabs are `tabs` and whose panes are `panes`.
        fn open(
            tabs: Vec<Tab>,
            panes: Vec<Pane>,
            cx: &mut TestAppContext,
        ) -> (Harness, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            cx.update(crate::terminal::init);
            let api = Arc::new(FakeApi::default());
            api.set_tabs(id(1), tabs);
            api.set_panes(panes);
            let connector = Rc::new(FakeConnector::default());
            let remote = connector.then_connect_to(api.clone());
            let connector: Rc<dyn Connector> = connector;
            let connection = cx.update(|cx| cx.new(|cx| Connection::new(connector, cx)));
            cx.run_until_parked();
            let Connected::Connected(session) = connection.read_with(cx, |c, _| c.status().clone())
            else {
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

        /// The panes shown, in layout order.
        fn shown(&self, cx: &mut VisualTestContext) -> Vec<PaneId> {
            self.view.read_with(cx, |view, cx| {
                view.shown().iter().map(|pane| pane.read(cx).id()).collect()
            })
        }

        fn pane(&self, n: usize, cx: &mut VisualTestContext) -> Entity<PaneView> {
            self.view.read_with(cx, |view, _| view.shown()[n].clone())
        }

        fn selected(&self, cx: &mut VisualTestContext) -> Option<TabId> {
            self.view.read_with(cx, |view, _| view.selected())
        }

        fn select(&self, tab: TabId, cx: &mut VisualTestContext) {
            cx.update(|window, cx| {
                self.view
                    .update(cx, |view, cx| view.select(tab, window, cx));
            });
            cx.run_until_parked();
        }

        fn deliver(&self, message: ServerMessage, cx: &mut VisualTestContext) {
            self.remote
                .events
                .try_send(Event::Message(message))
                .unwrap();
            cx.run_until_parked();
        }

        fn changed(&self, entity: Changed, cx: &mut VisualTestContext) {
            self.deliver(
                ServerMessage::Changed {
                    entity,
                    id: "x".into(),
                },
                cx,
            );
        }

        fn draw(&self, cx: &mut VisualTestContext) {
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }

    /// Tab `n` at `position`, split side by side between panes `a` and `b`.
    fn split_tab(n: u32, position: i64, a: u32, b: u32) -> Tab {
        Tab {
            layout: Some(Layout::Split {
                split: SplitDirection::Horizontal,
                ratio: Ratio::HALF,
                a: Box::new(Layout::single(id(a))),
                b: Box::new(Layout::single(id(b))),
            }),
            ..tab(n, id(1), position, None)
        }
    }

    mod tabs {
        use super::*;

        #[gpui::test]
        fn should_show_the_lowest_positioned_tab_first(cx: &mut TestAppContext) {
            let tabs = vec![
                tab(1, id(1), 1, Some(id(11))),
                tab(2, id(1), 0, Some(id(21))),
            ];
            let panes = vec![shell_pane(11, id(1)), shell_pane(21, id(2))];

            let (harness, cx) = Harness::open(tabs, panes, cx);

            assert_eq!(harness.selected(cx), Some(id(2)));
            assert_eq!(harness.shown(cx), [id(21)]);
        }

        #[gpui::test]
        fn should_show_the_panes_of_the_tab_selected(cx: &mut TestAppContext) {
            let tabs = vec![tab(1, id(1), 0, Some(id(11))), split_tab(2, 1, 21, 22)];
            let panes = vec![
                shell_pane(11, id(1)),
                shell_pane(21, id(2)),
                shell_pane(22, id(2)),
            ];
            let (harness, cx) = Harness::open(tabs, panes, cx);

            harness.select(id(2), cx);

            assert_eq!(harness.shown(cx), [id(21), id(22)]);
        }

        #[gpui::test]
        fn should_keep_the_selected_tab_across_reads(cx: &mut TestAppContext) {
            let tabs = vec![tab(1, id(1), 0, None), tab(2, id(1), 1, None)];
            let (harness, cx) = Harness::open(tabs, vec![], cx);
            harness.select(id(2), cx);

            harness.changed(Changed::Tab, cx);

            assert_eq!(harness.selected(cx), Some(id(2)));
        }

        #[gpui::test]
        fn should_show_the_first_tab_once_the_selected_one_is_gone(cx: &mut TestAppContext) {
            let tabs = vec![tab(1, id(1), 0, None), tab(2, id(1), 1, None)];
            let (harness, cx) = Harness::open(tabs, vec![], cx);
            harness.select(id(2), cx);
            harness.api.set_tabs(id(1), vec![tab(1, id(1), 0, None)]);

            harness.changed(Changed::Tab, cx);

            assert_eq!(harness.selected(cx), Some(id(1)));
        }

        #[gpui::test]
        fn should_ignore_a_tab_the_workspace_lacks(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(vec![tab(1, id(1), 0, None)], vec![], cx);

            harness.select(id(9), cx);

            assert_eq!(harness.selected(cx), Some(id(1)));
        }
    }

    mod panes {
        use super::*;

        #[gpui::test]
        fn should_show_every_pane_of_a_split(cx: &mut TestAppContext) {
            let panes = vec![shell_pane(7, id(1)), shell_pane(8, id(1))];

            let (harness, cx) = Harness::open(vec![split_tab(1, 0, 7, 8)], panes, cx);

            assert_eq!(harness.shown(cx), [id(7), id(8)]);
            harness.draw(cx);
        }

        #[gpui::test]
        fn should_show_no_pane_when_the_tab_has_none(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(vec![tab(1, id(1), 0, None)], vec![], cx);

            assert!(harness.shown(cx).is_empty());
            harness.draw(cx);
        }

        #[gpui::test]
        fn should_keep_each_panes_view_across_reads(cx: &mut TestAppContext) {
            let panes = vec![shell_pane(7, id(1)), shell_pane(8, id(1))];
            let (harness, cx) = Harness::open(vec![split_tab(1, 0, 7, 8)], panes, cx);
            let before = harness.pane(1, cx);

            harness.changed(Changed::Pane, cx);

            assert_eq!(harness.pane(1, cx), before);
        }

        #[gpui::test]
        fn should_keep_a_panes_view_when_its_tab_is_split(cx: &mut TestAppContext) {
            let panes = vec![shell_pane(7, id(1)), shell_pane(8, id(1))];
            let (harness, cx) = Harness::open(vec![tab(1, id(1), 0, Some(id(7)))], panes, cx);
            let before = harness.pane(0, cx);
            harness.api.set_tabs(id(1), vec![split_tab(1, 0, 7, 8)]);

            harness.changed(Changed::Tab, cx);

            assert_eq!(harness.shown(cx), [id(7), id(8)]);
            assert_eq!(harness.pane(0, cx), before);
        }

        #[gpui::test]
        fn should_not_read_again_for_other_changes(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(
                vec![tab(1, id(1), 0, Some(id(7)))],
                vec![shell_pane(7, id(1))],
                cx,
            );
            harness
                .api
                .set_tabs(id(1), vec![tab(1, id(1), 0, Some(id(8)))]);
            harness.api.set_panes(vec![shell_pane(8, id(1))]);

            harness.changed(Changed::Note, cx);

            assert_eq!(harness.shown(cx), [id(7)]);
        }

        #[gpui::test]
        fn should_focus_the_first_pane_of_a_tab_it_shows(cx: &mut TestAppContext) {
            let tabs = vec![tab(1, id(1), 0, Some(id(11))), split_tab(2, 1, 21, 22)];
            let panes = vec![
                shell_pane(11, id(1)),
                shell_pane(21, id(2)),
                shell_pane(22, id(2)),
            ];
            let (harness, cx) = Harness::open(tabs, panes, cx);

            harness.select(id(2), cx);

            let first = harness.pane(0, cx);
            let focused = cx.update(|window, cx| first.focus_handle(cx).is_focused(window));
            assert!(focused);
        }

        #[gpui::test]
        fn should_draw_a_split_in_both_themes(cx: &mut TestAppContext) {
            let tab = Tab {
                layout: Some(Layout::Split {
                    split: SplitDirection::Horizontal,
                    ratio: Ratio::HALF,
                    a: Box::new(Layout::single(id(7))),
                    b: Box::new(Layout::Split {
                        split: SplitDirection::Vertical,
                        ratio: Ratio::HALF,
                        a: Box::new(Layout::single(id(8))),
                        b: Box::new(Layout::single(id(9))),
                    }),
                }),
                ..tab(1, id(1), 0, None)
            };
            let panes = (7..=9).map(|n| shell_pane(n, id(1))).collect();
            let (harness, cx) = Harness::open(vec![tab], panes, cx);

            for preference in [ThemePreference::Light, ThemePreference::Dark] {
                cx.update(|_, cx| Theme::set_preference(preference, cx));
                harness.draw(cx);
            }

            assert_eq!(harness.shown(cx), [id(7), id(8), id(9)]);
        }
    }

    mod header {
        use super::*;

        fn one_pane(pane: Pane, cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            let shown = pane.id;
            Harness::open(vec![tab(1, id(1), 0, Some(shown))], vec![pane], cx)
        }

        #[gpui::test]
        fn should_title_a_pane_from_its_terminal(cx: &mut TestAppContext) {
            let (harness, cx) = one_pane(shell_pane(7, id(1)), cx);

            // The pane's terminal attaches on the connection's first channel.
            harness.deliver(
                ServerMessage::Attached {
                    channel: 1,
                    pane: id(7),
                    cols: 80,
                    rows: 24,
                    title: "✳ Claude Code".into(),
                    state: AgentState::Idle,
                },
                cx,
            );

            let title = harness.pane(0, cx).read_with(cx, |pane, _| pane.title());
            assert_eq!(title, "✳ Claude Code");
        }

        #[gpui::test]
        fn should_follow_state_broadcasts_between_reads(cx: &mut TestAppContext) {
            let (harness, cx) = one_pane(shell_pane(7, id(1)), cx);

            harness.deliver(
                ServerMessage::State {
                    pane: id(7),
                    state: AgentState::Blocked,
                },
                cx,
            );

            let state = harness
                .pane(0, cx)
                .read_with(cx, |pane, _| pane.pane().state);
            assert_eq!(state, AgentState::Blocked);
        }

        #[gpui::test]
        fn should_record_the_exit_code(cx: &mut TestAppContext) {
            let (harness, cx) = one_pane(shell_pane(7, id(1)), cx);

            harness.deliver(
                ServerMessage::Exit {
                    pane: id(7),
                    code: Some(1),
                },
                cx,
            );

            let pane = harness
                .pane(0, cx)
                .read_with(cx, |pane, _| pane.pane().clone());
            assert_eq!((pane.state, pane.exit_code), (AgentState::Exited, Some(1)));
        }

        #[gpui::test]
        fn should_ignore_another_panes_state(cx: &mut TestAppContext) {
            let (harness, cx) = one_pane(shell_pane(7, id(1)), cx);

            harness.deliver(
                ServerMessage::State {
                    pane: id(8),
                    state: AgentState::Blocked,
                },
                cx,
            );

            let state = harness
                .pane(0, cx)
                .read_with(cx, |pane, _| pane.pane().state);
            assert_eq!(state, AgentState::Idle);
        }

        #[gpui::test]
        fn should_show_the_linked_tasks_title(cx: &mut TestAppContext) {
            let linked = task(3, TaskKind::Plain, Some(id(1)));
            let pane = Pane {
                task_id: Some(linked.id),
                ..shell_pane(7, id(1))
            };
            let (harness, cx) = one_pane(pane, cx);
            harness.api.set_tasks(vec![linked]);

            harness.changed(Changed::Task, cx);

            let task = harness
                .pane(0, cx)
                .read_with(cx, |pane, _| pane.task().cloned());
            assert_eq!(task.as_deref(), Some("task 3"));
            harness.draw(cx);
        }
    }
}
