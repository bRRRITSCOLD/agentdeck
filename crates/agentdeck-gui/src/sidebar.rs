//! The main window's sidebar (spec §11), the `Sidebar` board of Penpot's "Main window": Inbox
//! and Tasks at the top, then every workspace with its rolled-up state, unread notes and open
//! PRs, and when PRs last synced at the bottom.
//!
//! It reads a [`Snapshot`] through the session's [`Api`] when it opens and again whenever the
//! daemon reports a `changed`; `state` and `exit` messages patch pane states in place.

use std::sync::Arc;
use std::time::Duration;

use agentdeck_core::ws::ServerMessage;
use agentdeck_core::{AgentState, WorkspaceId};
use brrritscold_core_gpui::{
    ActiveTheme as _, Icon, IconName, Separator, Sidebar, SidebarMenuButton,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window, div, px,
};

use crate::api::Api;
use crate::connection::{Connection, DaemonEvent};
use crate::count::Count;
use crate::overview::{Counts, Overview, Snapshot};
use crate::state_dot::{StateDot, StateDotState};

/// The sidebar's width. Raw in Penpot too: no sizing token is 260px.
const WIDTH: gpui::Pixels = px(260.);

/// How often the footer's "synced N min ago" is redrawn.
const FOOTER_TICK: Duration = Duration::from_secs(30);

/// What the sidebar has selected, and the window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// Tasks with no workspace.
    Inbox,
    /// Every task.
    Tasks,
    /// One workspace.
    Workspace(WorkspaceId),
}

/// The sidebar view.
pub struct SidebarView {
    api: Arc<dyn Api>,
    snapshot: Snapshot,
    overview: Overview,
    /// Why the last read failed, until one succeeds.
    error: Option<SharedString>,
    /// The user's pick; `None` until they make one.
    picked: Option<Selection>,
    /// The read in flight; replaced by a newer one.
    read: Option<Task<()>>,
    _events: Subscription,
    _tick: Task<()>,
}

impl SidebarView {
    /// A sidebar over `api`, refreshed by `connection`'s messages. It starts reading at once.
    pub fn new(api: Arc<dyn Api>, connection: &Entity<Connection>, cx: &mut Context<Self>) -> Self {
        let events = cx.subscribe(connection, |this, _, event, cx| this.on_event(event, cx));
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FOOTER_TICK).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        let snapshot = Snapshot::default();
        let mut sidebar = SidebarView {
            api,
            overview: snapshot.overview(),
            snapshot,
            error: None,
            picked: None,
            read: None,
            _events: events,
            _tick: tick,
        };
        sidebar.reread(cx);
        sidebar
    }

    /// What is selected: the user's pick while it still exists, else the first workspace,
    /// else the inbox.
    pub fn selection(&self) -> Selection {
        let exists = |picked: &Selection| match picked {
            Selection::Workspace(id) => self.overview.workspaces.iter().any(|w| w.id == *id),
            Selection::Inbox | Selection::Tasks => true,
        };
        self.picked.filter(exists).unwrap_or_else(|| {
            self.overview
                .workspaces
                .first()
                .map_or(Selection::Inbox, |w| Selection::Workspace(w.id))
        })
    }

    /// The sidebar's content.
    #[cfg(test)]
    pub fn overview(&self) -> &Overview {
        &self.overview
    }

    fn select(&mut self, selection: Selection, cx: &mut Context<Self>) {
        self.picked = Some(selection);
        cx.notify();
    }

    fn on_event(&mut self, event: &DaemonEvent, cx: &mut Context<Self>) {
        match event {
            DaemonEvent::Message(ServerMessage::State { pane, state }) => {
                self.set_state(*pane, *state, cx);
            }
            DaemonEvent::Message(ServerMessage::Exit { pane, .. }) => {
                self.set_state(*pane, AgentState::Exited, cx);
            }
            DaemonEvent::Message(ServerMessage::Changed { .. }) => self.reread(cx),
            _ => {}
        }
    }

    fn set_state(
        &mut self,
        pane: agentdeck_core::PaneId,
        state: AgentState,
        cx: &mut Context<Self>,
    ) {
        self.snapshot.set_state(pane, state);
        self.overview = self.snapshot.overview();
        cx.notify();
    }

    /// Reads a fresh snapshot, dropping any read still in flight.
    fn reread(&mut self, cx: &mut Context<Self>) {
        let reading = self.api.snapshot(cx);
        self.read = Some(cx.spawn(async move |this, cx| {
            let result = reading.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(snapshot) => {
                        this.overview = snapshot.overview();
                        this.snapshot = snapshot;
                        this.error = None;
                    }
                    Err(err) => {
                        this.error = Some(format!("Couldn't read the daemon: {err:#}").into())
                    }
                }
                cx.notify();
            });
        }));
    }

    /// A row: a menu button, with `lead` over its left end and `counts` over its right.
    fn row(
        &self,
        id: impl Into<ElementId>,
        selection: Selection,
        label: SharedString,
        lead: Lead,
        counts: Counts,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = id.into();
        let name = self.debug_name(selection);
        let has_counts = counts != Counts::default();
        let button = SidebarMenuButton::new(id.clone())
            .label(label)
            .is_active(self.selection() == selection)
            .on_click(cx.listener(move |this, _, _, cx| this.select(selection, cx)))
            // A long name stops short of the counts.
            .min_w_0()
            .whitespace_nowrap()
            .overflow_hidden()
            .text_ellipsis()
            .when(has_counts, |b| b.pr(space::S16));
        let (button, dot) = match lead {
            Lead::Icon(icon) => (button.icon(icon), None),
            // The menu button takes an icon, not a part, so the StateDot sits over the gap a
            // 16px icon would leave.
            Lead::Dot(state) => (button.pl(space::S8), Some(state)),
        };
        div()
            .id(id)
            .debug_selector({
                let name = name.clone();
                move || format!("row {name}")
            })
            .relative()
            .flex_none()
            .child(button)
            .when_some(dot, |row, state| {
                row.child(
                    div()
                        .absolute()
                        .left(space::S2)
                        .top(space::S2)
                        .child(StateDot::new().state(state)),
                )
            })
            .when(has_counts, |row| {
                row.child(
                    div()
                        .debug_selector(move || format!("counts {name}"))
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right(space::S2)
                        .flex()
                        .items_center()
                        .gap(space::S2)
                        .when(counts.unread_notes > 0, |c| {
                            c.child(Count::new(IconName::StickyNote, counts.unread_notes))
                        })
                        .when(counts.prs > 0, |c| {
                            c.child(Count::new(IconName::GitPullRequest, counts.prs))
                        }),
                )
            })
            .into_any_element()
    }

    /// The name a row is found by in tests.
    fn debug_name(&self, selection: Selection) -> String {
        match selection {
            Selection::Inbox => "inbox".into(),
            Selection::Tasks => "tasks".into(),
            Selection::Workspace(id) => self
                .overview
                .workspaces
                .iter()
                .find(|w| w.id == id)
                .map_or_else(|| id.to_string(), |w| w.name.to_string()),
        }
    }
}

/// What sits at a row's left end.
enum Lead {
    Icon(IconName),
    Dot(StateDotState),
}

impl Render for SidebarView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        // Penpot's `Section` and `Footer`: xs muted text, inset 8px across and 4px down.
        let small = || {
            div()
                .flex()
                .flex_none()
                .items_center()
                .px(space::S2)
                .py(space::S1)
                .text_size(font_size::XS)
                .text_color(s.muted_foreground)
        };
        let inbox = self.row(
            "inbox",
            Selection::Inbox,
            "Inbox".into(),
            Lead::Icon(IconName::Inbox),
            self.overview.inbox,
            cx,
        );
        let tasks = self.row(
            "tasks",
            Selection::Tasks,
            "Tasks".into(),
            Lead::Icon(IconName::ListTodo),
            Counts::default(),
            cx,
        );
        let workspaces: Vec<_> = self
            .overview
            .workspaces
            .clone()
            .into_iter()
            .map(|w| {
                self.row(
                    ElementId::Name(format!("workspace-{}", w.id).into()),
                    Selection::Workspace(w.id),
                    w.name,
                    Lead::Dot(StateDotState::of(w.state, None)),
                    w.counts,
                    cx,
                )
            })
            .collect();
        let footer = match &self.error {
            Some(error) => error.clone(),
            None => self.overview.pr_sync.label(chrono::Utc::now()).into(),
        };

        // Penpot `Sidebar`: p-3, rows 4px apart.
        Sidebar::new()
            .w(WIDTH)
            .flex_none()
            .p(space::S3)
            .gap(space::S1)
            // Penpot `Brand`.
            .child(
                div()
                    .flex()
                    .flex_none()
                    .h(space::S8)
                    .items_center()
                    .px(space::S2)
                    .text_size(font_size::SM)
                    .font_weight(font_weight::MEDIUM)
                    .text_color(s.foreground)
                    .child("agentdeck"),
            )
            .child(inbox)
            .child(tasks)
            .child(Separator::new())
            // Penpot `Section`.
            .child(small().font_weight(font_weight::MEDIUM).child("Workspaces"))
            // Penpot's `Spacer` pushes the footer down; the list takes that room and scrolls.
            .child(
                div()
                    .id("workspaces")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap(space::S1)
                    .overflow_y_scroll()
                    .children(workspaces),
            )
            // Penpot `Footer`.
            .child(
                small()
                    .debug_selector(|| "footer".into())
                    .gap(space::S1_5)
                    // The icon keeps the library icon's `foreground`, as Penpot's does.
                    .child(
                        Icon::new(IconName::GitPullRequest)
                            .size(space::S3)
                            .text_color(s.foreground),
                    )
                    .child(footer),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use agentdeck_client::ws::{self, Remote};
    use agentdeck_core::TaskKind;
    use agentdeck_core::ws::Entity as Changed;
    use brrritscold_core_gpui::Root;
    use gpui::{AppContext as _, Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::connection::fake::FakeConnector;
    use crate::overview::fixtures::*;

    /// An open sidebar over a fake daemon.
    struct Open {
        sidebar: Entity<SidebarView>,
        api: Arc<FakeApi>,
        remote: Remote,
        /// Kept alive: the sidebar only subscribes to it.
        _connection: Entity<Connection>,
        cx: &'static mut VisualTestContext,
    }

    impl Open {
        /// Lets the sidebar react to what it was sent, then draws a frame.
        fn settle(&mut self) {
            self.cx.run_until_parked();
            self.cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        fn send(&mut self, message: ServerMessage) {
            self.remote
                .events
                .try_send(ws::Event::Message(message))
                .unwrap();
            self.settle();
        }

        fn selection(&mut self) -> Selection {
            self.sidebar
                .read_with(self.cx, |sidebar, _| sidebar.selection())
        }

        fn shows(&mut self, selector: &'static str) -> bool {
            self.cx.debug_bounds(selector).is_some()
        }

        fn click(&mut self, selector: &'static str) {
            let bounds = self.cx.debug_bounds(selector).expect(selector);
            self.cx
                .simulate_click(bounds.center(), Modifiers::default());
            self.settle();
        }
    }

    /// Opens a `Root` window on a sidebar whose daemon answers with `snapshot`.
    fn open(snapshot: Snapshot, cx: &mut TestAppContext) -> Open {
        cx.update(brrritscold_core_gpui::init);
        let api = Arc::new(FakeApi::with(snapshot));
        let connector = Rc::new(FakeConnector::default());
        let remote = connector.then_connect_to(api.clone());
        let (connection, sidebar) = cx.update(|cx| {
            let connection = cx.new(|cx| Connection::new(connector, cx));
            let sidebar = cx.new(|cx| SidebarView::new(api.clone(), &connection, cx));
            (connection, sidebar)
        });
        let window = cx.add_window({
            let sidebar = sidebar.clone();
            |window, cx| Root::new(sidebar, window, cx)
        });
        let mut open = Open {
            sidebar,
            api,
            remote,
            _connection: connection,
            cx: VisualTestContext::from_window(*window, cx).into_mut(),
        };
        open.settle();
        open
    }

    /// Workspaces `a` (id 1) and `b` (id 2), each with one pane.
    fn two_workspaces() -> Snapshot {
        let (a, b) = (workspace(1, "a"), workspace(2, "b"));
        Snapshot {
            panes: vec![
                pane(10, a.id, AgentState::Working),
                pane(11, b.id, AgentState::Idle),
            ],
            workspaces: vec![a, b],
            ..Snapshot::default()
        }
    }

    mod rows {
        use super::*;

        #[gpui::test]
        fn should_list_the_workspaces_below_inbox_and_tasks(cx: &mut TestAppContext) {
            let mut open = open(two_workspaces(), cx);

            let top = |open: &mut Open, selector| open.cx.debug_bounds(selector).unwrap().top();
            let tops = [
                top(&mut open, "row inbox"),
                top(&mut open, "row tasks"),
                top(&mut open, "row a"),
                top(&mut open, "row b"),
            ];

            assert!(tops.is_sorted_by(|x, y| x < y), "{tops:?}");
        }

        #[gpui::test]
        fn should_show_counts_only_on_rows_that_have_some(cx: &mut TestAppContext) {
            let mut snapshot = two_workspaces();
            let a = snapshot.workspaces[0].id;
            snapshot.tasks = vec![task(20, TaskKind::Pr, Some(a))];
            let mut open = open(snapshot, cx);

            assert!(open.shows("counts a"));
            assert!(!open.shows("counts b"));
            assert!(!open.shows("counts inbox"));
        }
    }

    mod selection {
        use super::*;

        #[gpui::test]
        fn should_select_the_first_workspace_at_first(cx: &mut TestAppContext) {
            let mut open = open(two_workspaces(), cx);

            assert_eq!(open.selection(), Selection::Workspace(id(1)));
        }

        #[gpui::test]
        fn should_select_the_inbox_when_there_are_no_workspaces(cx: &mut TestAppContext) {
            let mut open = open(Snapshot::default(), cx);

            assert_eq!(open.selection(), Selection::Inbox);
        }

        #[gpui::test]
        fn should_select_the_row_clicked(cx: &mut TestAppContext) {
            let mut open = open(two_workspaces(), cx);

            open.click("row b");
            assert_eq!(open.selection(), Selection::Workspace(id(2)));

            open.click("row tasks");
            assert_eq!(open.selection(), Selection::Tasks);
        }

        #[gpui::test]
        fn should_fall_back_to_the_first_when_the_selected_workspace_goes(cx: &mut TestAppContext) {
            let mut open = open(two_workspaces(), cx);
            open.click("row b");
            let mut without_b = two_workspaces();
            without_b.workspaces.pop();
            open.api.set(without_b);

            open.send(ServerMessage::Changed {
                entity: Changed::Workspace,
                id: id::<WorkspaceId>(2).to_string(),
            });

            assert_eq!(open.selection(), Selection::Workspace(id(1)));
        }
    }

    mod daemon_messages {
        use super::*;

        fn state_of(open: &mut Open, name: &str) -> AgentState {
            open.sidebar.read_with(open.cx, |sidebar, _| {
                sidebar
                    .overview()
                    .workspaces
                    .iter()
                    .find(|w| w.name == name)
                    .unwrap()
                    .state
            })
        }

        #[gpui::test]
        fn should_read_again_on_a_change(cx: &mut TestAppContext) {
            let mut open = open(two_workspaces(), cx);
            let mut more = two_workspaces();
            more.workspaces.push(workspace(3, "c"));
            open.api.set(more);

            open.send(ServerMessage::Changed {
                entity: Changed::Workspace,
                id: id::<WorkspaceId>(3).to_string(),
            });

            assert_eq!(open.api.reads(), 2);
            assert!(open.shows("row c"));
        }

        #[gpui::test]
        fn should_roll_up_a_state_change_without_reading_again(cx: &mut TestAppContext) {
            let mut open = open(two_workspaces(), cx);

            open.send(ServerMessage::State {
                pane: id(11),
                state: AgentState::Blocked,
            });

            assert_eq!(state_of(&mut open, "b"), AgentState::Blocked);
            assert_eq!(open.api.reads(), 1);
        }

        #[gpui::test]
        fn should_count_an_exited_pane_out_of_the_rollup(cx: &mut TestAppContext) {
            let mut open = open(two_workspaces(), cx);

            open.send(ServerMessage::Exit {
                pane: id(10),
                code: Some(0),
            });

            assert_eq!(state_of(&mut open, "a"), AgentState::Idle);
        }
    }
}
