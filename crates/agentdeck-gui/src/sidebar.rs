//! The main window's sidebar (spec §11), the `Sidebar` board of Penpot's "Main window": the
//! brand, Inbox and Tasks, then the Workspaces header with its new workspace `+`, every
//! workspace with its rolled-up state, open PRs and a `BellOff` while muted, the archived ones
//! in a foldable Archived group, and when PRs last synced at the bottom, ending in a gear that
//! selects Settings. The `+` asks the window for the dialog with a [`SidebarEvent`].
//!
//! It reads a [`Snapshot`] through the session's [`Api`] when it opens and again whenever the
//! daemon reports a `changed`; `state` and `exit` messages patch pane states in place. What
//! changed that the user should hear about goes to the window as [`Alerts`] (spec §11).
//!
//! The workspace list draws only the rows in view, through GPUI's [`list`]: each step of a
//! working workspace's spinner redraws the sidebar, and drawing every row made that step cost
//! more the more workspaces there were (#199).

use std::sync::Arc;
use std::time::Duration;

use agentdeck_core::ws::ServerMessage;
use agentdeck_core::{AgentState, WorkspaceId};
use brrritscold_core_gpui::{
    ActiveTheme as _, Button, ButtonSize, ButtonVariant, Icon, IconName, Separator, Sidebar,
    SidebarMenuButton, StateDot, StateDotState,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, ElementId, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ListState, ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, Window,
    div, list, px,
};

use crate::api::Api;
use crate::connection::{Connection, DaemonEvent};
use crate::count::Count;
use crate::notifications::{self, Alert, PrBatch};
use crate::overview::{Counts, Overview, Snapshot};
use crate::rereads::Rereads;
use crate::scrollbar;
use crate::state_dot;
use crate::task_row;

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
    /// The settings, from the footer's gear; no row is highlighted.
    Settings,
}

/// What the sidebar asks of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarEvent {
    /// Its `+` was clicked: open the new workspace dialog.
    NewWorkspace,
}

/// What changed since the last read or `state` message that the user should hear about:
/// agent panes turning blocked or done, and PR changes once their sync ends. The window
/// decides which panes are out of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alerts(pub Vec<Alert>);

/// The sidebar view.
pub struct SidebarView {
    api: Arc<dyn Api>,
    snapshot: Snapshot,
    overview: Overview,
    /// Why the last read failed, until one succeeds.
    error: Option<SharedString>,
    /// The user's pick; `None` until they make one.
    picked: Option<Selection>,
    /// Whether the Archived group is unfolded; it shows anyway while it holds the selection.
    archived_open: bool,
    /// The last read started.
    read: Option<Task<()>>,
    rereads: Rereads,
    /// The PR changes of a sync still running.
    prs: PrBatch,
    _events: Subscription,
    _tick: Task<()>,
    /// The workspace list: where it is scrolled to, and its items' heights.
    list: ListState,
    /// The items `list` was last sized for.
    listed: Items,
    /// How many workspace list items it has rendered, so a test can tell which rows a frame
    /// drew.
    #[cfg(test)]
    items_rendered: std::cell::Cell<usize>,
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
            archived_open: false,
            read: None,
            rereads: Rereads::default(),
            prs: PrBatch::default(),
            _events: events,
            _tick: tick,
            // Every item measured once, after each change to the items, so the scrollbar's
            // thumb is right from the first frame; a redraw then renders the rows in view.
            list: task_row::list_state().measure_all(),
            listed: Items::default(),
            #[cfg(test)]
            items_rendered: std::cell::Cell::new(0),
        };
        sidebar.reread(cx);
        sidebar
    }

    /// What is selected: the user's pick while it still exists, archived or not, else the
    /// first workspace, else the inbox.
    pub fn selection(&self) -> Selection {
        let exists = |picked: &Selection| match picked {
            Selection::Workspace(id) => self.overview.workspace(*id).is_some(),
            Selection::Inbox | Selection::Tasks | Selection::Settings => true,
        };
        self.picked.filter(exists).unwrap_or_else(|| {
            self.overview
                .workspaces
                .first()
                .map_or(Selection::Inbox, |w| Selection::Workspace(w.id))
        })
    }

    /// Whether the Archived group shows its rows: unfolded, or holding the selection.
    fn archived_shown(&self) -> bool {
        let selected_archived = match self.selection() {
            Selection::Workspace(id) => self.overview.workspace(id).is_some_and(|w| w.archived),
            Selection::Inbox | Selection::Tasks | Selection::Settings => false,
        };
        self.archived_open || selected_archived
    }

    /// Reads the daemon's data again.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.reread(cx);
    }

    /// The daemon's data as last read, with live pane states.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// The sidebar's content.
    pub fn overview(&self) -> &Overview {
        &self.overview
    }

    /// Selects `selection`. A workspace not read yet is selected once a read shows it.
    pub fn select(&mut self, selection: Selection, cx: &mut Context<Self>) {
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
        let before = (self.snapshot.panes.iter()).find(|entry| entry.pane == pane);
        let before = before.map(|entry| entry.state);
        self.snapshot.set_state(pane, state);
        self.overview = self.snapshot.overview();
        let now = (self.snapshot.panes.iter()).find(|entry| entry.pane == pane);
        if let (Some(before), Some(now)) = (before, now)
            && let Some(alert) = notifications::pane_alert(before, now, &self.snapshot)
        {
            cx.emit(Alerts(vec![alert]));
        }
        cx.notify();
    }

    /// Reads a fresh snapshot, or once more after the read in flight.
    fn reread(&mut self, cx: &mut Context<Self>) {
        if !self.rereads.ask() {
            return;
        }
        let reading = self.api.snapshot(cx);
        self.read = Some(cx.spawn(async move |this, cx| {
            let result = reading.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(snapshot) => {
                        let mut alerts = notifications::pane_alerts(&this.snapshot, &snapshot);
                        alerts.extend(this.prs.read(&this.snapshot, &snapshot));
                        this.overview = snapshot.overview();
                        this.snapshot = snapshot;
                        this.error = None;
                        if !alerts.is_empty() {
                            cx.emit(Alerts(alerts));
                        }
                    }
                    Err(err) => {
                        this.error = Some(format!("Couldn't read the daemon: {err:#}").into())
                    }
                }
                cx.notify();
                if this.rereads.landed() {
                    this.reread(cx);
                }
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
        let active = self.selection() == selection;
        let (archived, muted) = match selection {
            Selection::Workspace(id) => (self.overview.workspace(id))
                .map_or((false, false), |w| (w.archived, w.muted && !w.archived)),
            Selection::Inbox | Selection::Tasks | Selection::Settings => (false, false),
        };
        // A muted row's bell sits with its counts, so it holds the label short of them too.
        let has_counts = counts != Counts::default() || muted;
        let button = SidebarMenuButton::new(id.clone())
            .label(label)
            .is_active(active)
            // Penpot "Archived": an archived row's label is `muted-foreground` until selected.
            .when(archived && !active, |b| {
                b.text_color(cx.theme().semantic().muted_foreground)
            })
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
            Lead::Dot { state, spin } => {
                (button.pl(space::S8), Some(StateDot::new(state).spin(spin)))
            }
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
            .when_some(dot, |row, dot| {
                row.child(div().absolute().left(space::S2).top(space::S2).child(dot))
            })
            .when(has_counts, |row| {
                let bell = format!("muted {name}");
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
                        // Penpot "Workspace menu": a 12px `BellOff` in `muted-foreground`
                        // before the counts.
                        .when(muted, |c| {
                            c.child(
                                div().debug_selector(move || bell).child(
                                    Icon::new(IconName::BellOff)
                                        .size(space::S3)
                                        .text_color(cx.theme().semantic().muted_foreground),
                                ),
                            )
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
            Selection::Settings => "settings".into(),
            Selection::Workspace(id) => {
                (self.overview.workspace(id)).map_or_else(|| id.to_string(), |w| w.name.to_string())
            }
        }
    }

    /// The workspace list's items: the workspace rows, then the Archived group's header and,
    /// while it shows them, its rows.
    fn items(&self) -> Items {
        let rows = self.overview.workspaces.len();
        let archived = match self.overview.archived.len() {
            0 => 0,
            n if self.archived_shown() => 1 + n,
            _ => 1,
        };
        Items {
            count: rows + archived,
            header: (archived > 0).then_some(rows),
        }
    }

    /// The workspace list's item at `ix`, spaced from the next one: a list has no gap.
    fn item(&self, ix: usize, spin: bool, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(test)]
        self.items_rendered.set(self.items_rendered.get() + 1);
        let Items { count, header } = self.items();
        let rows = &self.overview.workspaces;
        let item = if header == Some(ix) {
            // Penpot "Archived": a `GroupHeader` below the workspace rows, folded at first.
            task_row::group_header(
                "archived",
                "Archived",
                self.overview.archived.len(),
                !self.archived_shown(),
                task_row::SelectCell::Off,
                cx.listener(|this, _, _, cx| {
                    this.archived_open = !this.archived_open;
                    cx.notify();
                }),
                cx,
            )
        } else {
            let w = match ix.checked_sub(rows.len() + 1) {
                Some(archived) => self.overview.archived.get(archived),
                None => rows.get(ix),
            };
            let Some(w) = w.cloned() else {
                return div().into_any_element();
            };
            self.row(
                ElementId::Name(format!("workspace-{}", w.id).into()),
                Selection::Workspace(w.id),
                w.name,
                Lead::Dot {
                    state: state_dot::of(w.state, None),
                    spin,
                },
                w.counts,
                cx,
            )
        };
        div()
            .when(ix + 1 < count, |item| item.pb(space::S1))
            .child(item)
            .into_any_element()
    }
}

/// How many items the workspace list holds, and which is the Archived group's header. The
/// header isn't a row's height, so the list measures its items again when either changes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Items {
    count: usize,
    header: Option<usize>,
}

/// What sits at a row's left end.
enum Lead {
    Icon(IconName),
    /// A `StateDot`, turning only while `spin`.
    Dot {
        state: StateDotState,
        spin: bool,
    },
}

impl EventEmitter<SidebarEvent> for SidebarView {}

impl EventEmitter<Alerts> for SidebarView {}

impl Render for SidebarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        // A turning mark redraws the window, so it holds still while the window is inactive.
        let spin = window.is_window_active();
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
        let items = self.items();
        if items != self.listed {
            task_row::relist(&self.list, items.count);
            self.listed = items;
        }
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
                    .debug_selector(|| "brand".into())
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
            // Penpot gives the separator 8px above and below, on top of the 4px row gap.
            .child(Separator::new().my(space::S2))
            // Penpot `Section`: 32px, the label, a spacer, then `NewWorkspace`, the same ghost
            // icon-xs `Plus` as the right panel's Tasks & PRs header.
            .child(
                small()
                    .debug_selector(|| "workspaces header".into())
                    .h(space::S8)
                    .gap(space::S2)
                    .font_weight(font_weight::MEDIUM)
                    .child("Workspaces")
                    .child(div().flex_1())
                    .child(
                        div().debug_selector(|| "new workspace".into()).child(
                            Button::new("new-workspace")
                                .variant(ButtonVariant::Ghost)
                                .size(ButtonSize::IconXs)
                                .icon(IconName::Plus)
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(SidebarEvent::NewWorkspace);
                                })),
                        ),
                    ),
            )
            // Penpot's `Spacer` pushes the footer down; the list takes that room and scrolls.
            .child(
                div()
                    .debug_selector(|| "workspaces".into())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(
                        list(
                            self.list.clone(),
                            cx.processor(move |this, ix, _, cx| this.item(ix, spin, cx)),
                        )
                        .flex_1()
                        .min_h_0(),
                    ),
            )
            .child(scrollbar::vertical(&self.list, cx))
            // Penpot `Footer`: the sync line, then the Settings gear pushed to the end.
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
                    .child(div().min_w_0().truncate().child(footer))
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex_none()
                            .debug_selector(|| "settings".into())
                            .child(
                                Button::new("settings")
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::IconXs)
                                    .icon(IconName::Settings2)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.select(Selection::Settings, cx);
                                    })),
                            ),
                    ),
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

    mod new_workspace {
        use std::cell::RefCell;

        use super::*;

        #[gpui::test]
        fn should_ask_for_the_dialog_from_its_plus(cx: &mut TestAppContext) {
            let mut open = open(two_workspaces(), cx);
            let asked = Rc::new(RefCell::new(Vec::new()));
            let seen = asked.clone();
            let _asks = open.cx.update(|_, cx| {
                cx.subscribe(&open.sidebar, move |_, event: &SidebarEvent, _| {
                    seen.borrow_mut().push(*event);
                })
            });

            open.click("new workspace");

            assert_eq!(*asked.borrow(), [SidebarEvent::NewWorkspace]);
        }

        #[gpui::test]
        fn should_open_the_new_workspace_dialog_from_the_workspaces_header(
            cx: &mut TestAppContext,
        ) {
            let mut open = open(two_workspaces(), cx);
            let asked = Rc::new(RefCell::new(Vec::new()));
            let seen = asked.clone();
            let _asks = open.cx.update(|_, cx| {
                cx.subscribe(&open.sidebar, move |_, event: &SidebarEvent, _| {
                    seen.borrow_mut().push(*event);
                })
            });

            let bounds =
                |open: &mut Open, selector| open.cx.debug_bounds(selector).expect(selector);
            let (brand, header) = (
                bounds(&mut open, "brand"),
                bounds(&mut open, "workspaces header"),
            );
            let plus = bounds(&mut open, "new workspace");
            assert!(header.contains(&plus.center()), "{plus:?} in {header:?}");
            assert!(!brand.intersects(&plus), "{plus:?} off {brand:?}");
            assert_eq!(brand.size.height, space::S8);
            assert_eq!(header.size.height, space::S8);
            open.click("new workspace");

            assert_eq!(*asked.borrow(), [SidebarEvent::NewWorkspace]);
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

    mod spin {
        use super::*;
        use crate::state_dot::tests::{activate, deactivate, redraws_in_one_second};

        #[gpui::test]
        fn should_spin_a_working_workspaces_mark_while_the_window_is_active(
            cx: &mut TestAppContext,
        ) {
            let open = open(two_workspaces(), cx);
            activate(open.cx);
            assert!(redraws_in_one_second(&open.sidebar, open.cx) > 0);
        }

        #[gpui::test]
        fn should_hold_it_still_while_the_window_is_inactive(cx: &mut TestAppContext) {
            let open = open(two_workspaces(), cx);
            activate(open.cx);
            deactivate(open.cx);
            assert_eq!(redraws_in_one_second(&open.sidebar, open.cx), 0);
        }
    }

    mod muted {
        use agentdeck_client::types::Workspace;

        use super::*;

        /// [`two_workspaces`] with `a` muted and holding an open PR.
        fn a_muted() -> Snapshot {
            let mut snapshot = two_workspaces();
            snapshot.workspaces[0].muted = true;
            let a = snapshot.workspaces[0].id;
            snapshot.tasks = vec![task(20, TaskKind::Pr, Some(a))];
            snapshot
        }

        #[gpui::test]
        fn should_show_a_12px_bell_off_only_on_a_muted_row(cx: &mut TestAppContext) {
            let mut open = open(a_muted(), cx);

            let bell = open.cx.debug_bounds("muted a").expect("a's bell shows");
            assert_eq!(bell.size.width, space::S3);
            assert!(!open.shows("muted b"));
        }

        #[gpui::test]
        fn should_put_the_bell_before_the_counts(cx: &mut TestAppContext) {
            let open = open(a_muted(), cx);

            let bell = open.cx.debug_bounds("muted a").expect("a's bell shows");
            let counts = open.cx.debug_bounds("counts a").expect("a's counts show");
            assert!(counts.contains(&bell.center()), "{bell:?} in {counts:?}");
            assert_eq!(bell.left(), counts.left(), "first in the row's end");
        }

        #[gpui::test]
        fn should_show_the_bell_on_a_muted_row_with_no_counts(cx: &mut TestAppContext) {
            let mut snapshot = two_workspaces();
            snapshot.workspaces[1].muted = true;
            let mut open = open(snapshot, cx);

            assert!(open.shows("muted b"));
        }

        #[gpui::test]
        fn should_not_show_the_bell_on_an_archived_row(cx: &mut TestAppContext) {
            let mut snapshot = two_workspaces();
            snapshot.workspaces.push(Workspace {
                archived_at: Some("2026-11-13T00:00:00Z".into()),
                muted: true,
                ..workspace(3, "old")
            });
            let mut open = open(snapshot, cx);
            open.click("group archived");

            assert!(open.shows("row old"));
            assert!(!open.shows("muted old"));
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
        fn should_select_settings_from_the_gear_at_the_footers_end(cx: &mut TestAppContext) {
            let mut open = open(two_workspaces(), cx);

            let footer = open.cx.debug_bounds("footer").expect("footer");
            let gear = open.cx.debug_bounds("settings").expect("the gear");
            assert!(footer.contains(&gear.center()), "{gear:?} in {footer:?}");
            assert!(
                footer.right() - gear.right() <= space::S2,
                "{gear:?} at the end of {footer:?}"
            );
            open.click("settings");

            assert_eq!(open.selection(), Selection::Settings);
        }

        #[gpui::test]
        fn should_keep_settings_selected_when_the_workspaces_change(cx: &mut TestAppContext) {
            let mut open = open(two_workspaces(), cx);
            open.click("settings");

            open.send(ServerMessage::Changed {
                entity: Changed::Workspace,
                id: id::<WorkspaceId>(2).to_string(),
            });

            assert_eq!(open.selection(), Selection::Settings);
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

    mod archived {
        use agentdeck_client::types::Workspace;

        use super::*;

        /// [`two_workspaces`], plus `old` (id 3) and `older` (id 4), archived.
        fn with_archived() -> Snapshot {
            let archived = |n, name| Workspace {
                archived_at: Some("2026-11-13T00:00:00Z".into()),
                ..workspace(n, name)
            };
            let mut snapshot = two_workspaces();
            snapshot
                .workspaces
                .extend([archived(3, "old"), archived(4, "older")]);
            snapshot
        }

        #[gpui::test]
        fn should_hide_the_group_when_none_is_archived(cx: &mut TestAppContext) {
            let mut open = open(two_workspaces(), cx);

            assert!(!open.shows("group archived"));
        }

        #[gpui::test]
        fn should_start_collapsed_below_the_workspaces(cx: &mut TestAppContext) {
            let mut open = open(with_archived(), cx);

            let group = open.cx.debug_bounds("group archived").expect("shown");
            let last = open.cx.debug_bounds("row b").expect("shown");
            assert!(group.top() > last.top());
            assert!(!open.shows("row old"));
        }

        #[gpui::test]
        fn should_list_the_archived_rows_once_opened_and_hide_them_again(cx: &mut TestAppContext) {
            let mut open = open(with_archived(), cx);

            open.click("group archived");
            let top = |open: &mut Open, selector| open.cx.debug_bounds(selector).unwrap().top();
            let tops = [
                top(&mut open, "group archived"),
                top(&mut open, "row old"),
                top(&mut open, "row older"),
            ];
            assert!(tops.is_sorted_by(|x, y| x < y), "{tops:?}");

            open.click("group archived");
            assert!(!open.shows("row old"));
        }

        #[gpui::test]
        fn should_select_an_archived_row(cx: &mut TestAppContext) {
            let mut open = open(with_archived(), cx);
            open.click("group archived");

            open.click("row old");

            assert_eq!(open.selection(), Selection::Workspace(id(3)));
        }

        #[gpui::test]
        fn should_stay_open_while_the_selected_workspace_is_archived(cx: &mut TestAppContext) {
            let mut open = open(with_archived(), cx);
            open.sidebar.update(open.cx, |sidebar, cx| {
                sidebar.select(Selection::Workspace(id(4)), cx);
            });
            open.settle();

            assert!(open.shows("row older"));
            open.click("group archived");
            assert!(open.shows("row older"), "it can't fold away the selection");
            assert_eq!(open.selection(), Selection::Workspace(id(4)));
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
        fn should_read_at_most_twice_more_for_a_burst_of_changes(cx: &mut TestAppContext) {
            let mut open = open(two_workspaces(), cx);
            let mut more = two_workspaces();
            more.workspaces.push(workspace(3, "c"));
            open.api.set(more);

            for _ in 0..10 {
                open.remote
                    .events
                    .try_send(ws::Event::Message(ServerMessage::Changed {
                        entity: Changed::Task,
                        id: "x".into(),
                    }))
                    .unwrap();
            }
            open.settle();

            assert!(open.api.reads() <= 3, "{} reads", open.api.reads());
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

    mod alerts {
        use std::cell::RefCell;

        use agentdeck_core::Agent;

        use super::*;
        use crate::notifications::Alert;

        /// [`two_workspaces`] with pane 11, in `b`, running Claude Code.
        fn with_an_agent() -> Snapshot {
            let mut snapshot = two_workspaces();
            snapshot.panes[1].agent = Some(Agent::Claude);
            snapshot
        }

        /// Every alert the sidebar emits from now on.
        fn heard(open: &mut Open) -> (Rc<RefCell<Vec<Alert>>>, Subscription) {
            let heard = Rc::new(RefCell::new(Vec::new()));
            let seen = heard.clone();
            let subscription = open.cx.update(|_, cx| {
                cx.subscribe(&open.sidebar, move |_, Alerts(alerts): &Alerts, _| {
                    seen.borrow_mut().extend(alerts.iter().cloned());
                })
            });
            (heard, subscription)
        }

        #[gpui::test]
        fn should_alert_when_an_agent_pane_turns_blocked(cx: &mut TestAppContext) {
            let mut open = open(with_an_agent(), cx);
            let (heard, _subscription) = heard(&mut open);

            open.send(ServerMessage::State {
                pane: id(11),
                state: AgentState::Blocked,
            });

            let titles: Vec<_> = heard.borrow().iter().map(|a| a.title.clone()).collect();
            assert_eq!(titles, ["Claude Code is blocked"]);
        }

        #[gpui::test]
        fn should_alert_for_a_change_a_reread_shows(cx: &mut TestAppContext) {
            let mut open = open(with_an_agent(), cx);
            let (heard, _subscription) = heard(&mut open);
            let mut blocked = with_an_agent();
            blocked.set_state(id(11), AgentState::Blocked);
            open.api.set(blocked);

            open.send(ServerMessage::Changed {
                entity: Changed::Pane,
                id: id::<agentdeck_core::PaneId>(11).to_string(),
            });

            assert_eq!(heard.borrow().len(), 1);
        }

        #[gpui::test]
        fn should_not_alert_for_the_first_read(cx: &mut TestAppContext) {
            let mut blocked = with_an_agent();
            blocked.set_state(id(11), AgentState::Blocked);
            let mut open = open(Snapshot::default(), cx);
            let (heard, _subscription) = heard(&mut open);
            open.api.set(blocked);

            open.send(ServerMessage::Changed {
                entity: Changed::Workspace,
                id: id::<WorkspaceId>(2).to_string(),
            });

            assert_eq!(*heard.borrow(), []);
        }
    }

    mod long_list {
        use agentdeck_client::types::Workspace;
        use gpui::{ScrollDelta, ScrollEvent, point};

        use super::*;
        use crate::state_dot::tests::{activate, redraws_in_one_second};

        /// `w01` to `w60` (ids 1 to 60), each with an agent at work, then `old` (id 61),
        /// archived.
        fn crowded() -> Snapshot {
            let mut workspaces: Vec<_> = (1..=60)
                .map(|n| workspace(n, &format!("w{n:02}")))
                .collect();
            workspaces.push(Workspace {
                archived_at: Some("2026-11-13T00:00:00Z".into()),
                ..workspace(61, "old")
            });
            Snapshot {
                panes: (1..=60)
                    .map(|n| pane(100 + n, id(n), AgentState::Working))
                    .collect(),
                workspaces,
                ..Snapshot::default()
            }
        }

        fn rendered(open: &mut Open) -> usize {
            (open.sidebar).read_with(open.cx, |sidebar, _| sidebar.items_rendered.get())
        }

        /// How many rows the list's height holds, counting one cut off at either end.
        fn in_view(open: &mut Open) -> usize {
            let list = open.cx.debug_bounds("workspaces").expect("the list");
            (list.size.height / space::S8).ceil() as usize + 1
        }

        /// Wheels the list down by `by`.
        fn wheel(open: &mut Open, by: f32) {
            let list = open.cx.debug_bounds("workspaces").expect("the list");
            open.cx.simulate_event(ScrollEvent {
                position: list.center(),
                delta: ScrollDelta::Pixels(point(px(0.), px(-by))),
                ..ScrollEvent::default()
            });
            open.settle();
        }

        fn wheel_to_the_end(open: &mut Open) {
            wheel(open, 10_000.);
        }

        #[gpui::test]
        fn should_draw_only_the_rows_in_view(cx: &mut TestAppContext) {
            let mut open = open(crowded(), cx);

            assert!(open.shows("row w01"));
            assert!(!open.shows("row w60"));
        }

        #[gpui::test]
        fn should_render_only_the_rows_in_view_on_a_spinner_step(cx: &mut TestAppContext) {
            let mut open = open(crowded(), cx);
            activate(open.cx);
            let (before, in_view) = (rendered(&mut open), in_view(&mut open));
            assert!(in_view < 60, "the window holds {in_view} rows");

            let redraws = redraws_in_one_second(&open.sidebar, open.cx);

            assert!(redraws > 0, "the marks turn");
            let per_step = (rendered(&mut open) - before) / redraws;
            assert!(
                per_step <= in_view,
                "{per_step} rows a step, {in_view} in view"
            );
        }

        #[gpui::test]
        fn should_draw_the_rows_a_scroll_brings_into_view(cx: &mut TestAppContext) {
            let mut open = open(crowded(), cx);

            wheel_to_the_end(&mut open);

            assert!(open.shows("row w60"));
            assert!(!open.shows("row w01"));
        }

        #[gpui::test]
        fn should_select_a_row_scrolled_into_view(cx: &mut TestAppContext) {
            let mut open = open(crowded(), cx);
            wheel_to_the_end(&mut open);

            open.click("row w60");

            assert_eq!(open.selection(), Selection::Workspace(id(60)));
        }

        #[gpui::test]
        fn should_open_the_archived_group_at_the_end(cx: &mut TestAppContext) {
            let mut open = open(crowded(), cx);
            wheel_to_the_end(&mut open);

            open.click("group archived");
            wheel_to_the_end(&mut open);

            assert!(open.shows("row old"));
            open.click("row old");
            assert_eq!(open.selection(), Selection::Workspace(id(61)));
        }

        #[gpui::test]
        fn should_keep_its_place_when_the_workspaces_change(cx: &mut TestAppContext) {
            let mut open = open(crowded(), cx);
            wheel_to_the_end(&mut open);
            let mut more = crowded();
            more.workspaces.push(workspace(62, "w62"));
            open.api.set(more);

            open.send(ServerMessage::Changed {
                entity: Changed::Workspace,
                id: id::<WorkspaceId>(62).to_string(),
            });

            assert!(!open.shows("row w01"));
            assert!(open.shows("row w60"));
        }
    }

    mod scrollbar {
        use super::*;
        use crate::scrollbar::testing::thumb;

        #[gpui::test]
        fn should_show_a_scrollbar_when_the_list_overflows(cx: &mut TestAppContext) {
            let crowded = Snapshot {
                workspaces: (1..=60).map(|n| workspace(n, &format!("w{n}"))).collect(),
                ..Snapshot::default()
            };
            let open = open(crowded, cx);

            let list = open.cx.debug_bounds("workspaces").expect("the list");
            assert!(thumb(list, open.cx).is_some());
        }

        #[gpui::test]
        fn should_hide_the_scrollbar_when_everything_fits(cx: &mut TestAppContext) {
            let open = open(two_workspaces(), cx);

            let list = open.cx.debug_bounds("workspaces").expect("the list");
            assert_eq!(thumb(list, open.cx), None);
        }
    }
}
