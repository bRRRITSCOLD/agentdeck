//! The Inbox (spec §11), Penpot "Screens" → "Inbox": tasks with no workspace, mostly PRs,
//! each with a status picker, and the PR sync's state with a "Sync now" button. Done tasks
//! are left out. While no GitHub token works, an alert over the columns says so, its **Open
//! Settings** asking the window for Settings with [`OpenSettings`]. Under the heading, once the
//! PR sync status is read, a line says which PRs the age cutoff leaves out, its "Settings" a
//! link that asks for Settings too.
//!
//! Like the Tasks view it shows the sidebar's [`Snapshot`], passed on by the window. A sync
//! asked for here emits [`Synced`] once done, so the window can have the sidebar read again:
//! a sync that changes no task broadcasts nothing, yet moves the time it last ran.
//!
//! Its list draws only the rows in view: an inbox can hold hundreds of PRs.
//!
//! **Select**, before the sync label, turns on Select mode for closing PRs ([`ClosePrs`]):
//! a checkbox on each open PR's row, one in the columns for all of them, and the selection
//! bar along the bottom.

use std::sync::Arc;

use agentdeck_client::types::GhLogin;
use agentdeck_core::{TaskId, TaskStatus};
use brrritscold_core_gpui::{
    ActiveTheme as _, Alert, AlertAction, AlertTitle, Button, ButtonSize, ButtonVariant, IconName,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, ListState, ParentElement as _, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, list,
};

use crate::api::Api;
use crate::close_prs::{self, ClosePrs};
use crate::overview::{PrSync, Snapshot};
use crate::task_list::{self, Query, Row, Scope};
use crate::task_row::{self, Layout, Table};
use crate::task_sheet::{EditTask, OpenTask};
use crate::workspace::failed;

/// The description's first line.
const ABOUT: &str =
    "Tasks with no workspace. A PR lands here when no workspace has a clone of its repo.";

/// The end of a cutoff line that names Settings; its "Settings" is drawn as a link.
const SETTINGS_TAIL: &str = "Settings.";

/// Which open PRs sync, for a cutoff of `days` (`0` for none).
fn cutoff(days: u32) -> String {
    let unit = match days {
        0 => return "Every open PR syncs.".to_owned(),
        1 => "day",
        _ => "days",
    };
    format!("PRs not updated in the last {days} {unit} are left out; change that in Settings.")
}

/// A sync asked for in the Inbox has finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Synced;

/// The banner's title while no GitHub token works.
pub const LOGGED_OUT: &str = "Not signed in to GitHub, so PRs don't sync.";

/// The cutoff line's "Settings" or the banner's Open Settings was clicked: show Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenSettings;

/// The Inbox view.
pub struct InboxView {
    api: Arc<dyn Api>,
    snapshot: Snapshot,
    /// Whether a sync asked for here is still running.
    syncing: bool,
    /// The rows the list draws from, as last rendered.
    shown: Vec<Row>,
    list: ListState,
    /// The columns shown, fitted to the width the table was last drawn at.
    layout: Layout,
    /// Select mode, the selection and the confirm dialog.
    close: Entity<ClosePrs>,
    _close: Subscription,
}

impl EventEmitter<Synced> for InboxView {}
impl EventEmitter<OpenTask> for InboxView {}
impl EventEmitter<EditTask> for InboxView {}
impl EventEmitter<OpenSettings> for InboxView {}

impl InboxView {
    /// A view that syncs and changes statuses through `api`; it shows nothing until given a
    /// snapshot.
    pub fn new(api: Arc<dyn Api>, cx: &mut Context<Self>) -> Self {
        let close = cx.new(|_| ClosePrs::new(api.clone(), "inbox"));
        let _close = cx.observe(&close, |this, _, cx| {
            this.list.remeasure();
            cx.notify();
        });
        InboxView {
            api,
            snapshot: Snapshot::default(),
            syncing: false,
            shown: Vec::new(),
            list: task_row::list_state(),
            layout: Table::Inbox.widest(),
            close,
            _close,
        }
    }

    /// Select mode, the selection and the confirm dialog.
    #[cfg(test)]
    pub fn close_prs(&self) -> &Entity<ClosePrs> {
        &self.close
    }

    /// Fits the columns to the table's `width`, as drawn. A row's height follows its title's
    /// width, so a change measures the rows again. It runs while the frame is drawn, which
    /// ignores a notify, so the redraw is asked for once the frame is done.
    fn fit(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        let layout = Table::Inbox.fit(width, self.close.read(cx).active());
        if layout != self.layout {
            self.layout = layout;
            self.list.remeasure();
            cx.defer_in(window, |_, _, cx| cx.notify());
        }
    }

    /// Shows `snapshot`.
    pub fn set_snapshot(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.close
            .update(cx, |close, cx| close.saw(&snapshot.tasks, cx));
        self.snapshot = snapshot;
        cx.notify();
    }

    /// The rows shown.
    pub fn rows(&self) -> Vec<Row> {
        let query = Query {
            scope: Scope::Inbox,
            filter: "",
            show_done: false,
        };
        task_list::rows(&self.snapshot, &query)
    }

    /// Syncs PRs now: what "Sync now" does. A sync already asked for isn't repeated.
    pub fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.syncing {
            return;
        }
        self.syncing = true;
        cx.notify();
        let syncing = self.api.sync_prs(cx);
        cx.spawn_in(window, async move |this, cx| {
            let synced = syncing.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.syncing = false;
                match synced {
                    Ok(()) => cx.emit(Synced),
                    Err(err) => failed("Couldn't sync PRs", &err, window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Moves `task` to `status`, reporting a failure in a toast.
    pub fn set_status(
        &mut self,
        task: TaskId,
        status: TaskStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let setting = self.api.set_task_status(task, status, cx);
        cx.spawn_in(window, async move |_, cx| {
            if let Err(err) = setting.await {
                let _ = cx.update(|window, cx| {
                    failed("Couldn't change the status", &err, window, cx);
                });
            }
        })
        .detach();
    }

    /// The list's `ix`th row, with its status picker.
    fn item(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.shown.get(ix) else {
            return div().into_any_element();
        };
        let task = row.id;
        let picker = task_row::status_picker(
            SharedString::from(format!("inbox-status-{task}")),
            row.status,
            cx.listener(move |this, status: &TaskStatus, window, cx| {
                this.set_status(task, *status, window, cx);
            }),
        );
        let open = Box::new(cx.listener(move |_, _, _, cx| cx.emit(OpenTask(task))));
        let view = cx.weak_entity();
        let edit = move |_: &mut Window, cx: &mut gpui::App| {
            let _ = view.update(cx, |_, cx| cx.emit(EditTask(task)));
        };
        let send = crate::send_task::targets(&self.snapshot, task);
        let parts = close_prs::row_parts(&self.close, row, send, edit, cx);
        task_row::spaced(task_row::wide(
            row,
            picker,
            self.layout(cx),
            parts,
            open,
            cx,
        ))
    }

    /// The columns to draw: the fitted ones, in Select mode while it's on.
    fn layout(&self, cx: &gpui::App) -> Layout {
        let select = self.close.read(cx).active();
        if select == self.layout.select {
            return self.layout;
        }
        Layout {
            select,
            ..self.layout
        }
    }

    /// Where PR sync stands, at `now`: how far a running sync is, whoever started it, or
    /// "Syncing…" from the moment "Sync now" is clicked until the daemon says more.
    pub fn sync_label(&self, now: chrono::DateTime<chrono::Utc>) -> String {
        let sync = self.snapshot.overview().pr_sync;
        if self.syncing && !matches!(sync, PrSync::Syncing { .. }) {
            return "Syncing…".to_owned();
        }
        sync.short_label(now)
    }

    /// The lines under the heading: what the Inbox holds, then which PRs the age cutoff
    /// leaves out, once the PR sync status is read.
    pub fn description(&self) -> Vec<SharedString> {
        let cutoff = (self.snapshot.pr_sync.as_ref()).map(|status| cutoff(status.max_age_days));
        std::iter::once(SharedString::new_static(ABOUT))
            .chain(cutoff.map(SharedString::from))
            .collect()
    }

    /// Penpot `Header`: heading, spacer, Select, when PRs last synced, "Sync now".
    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let synced = self.sync_label(chrono::Utc::now());
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(space::S3)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::S1)
                    .child(
                        div()
                            .text_size(font_size::XL)
                            .font_weight(font_weight::MEDIUM)
                            .text_color(s.foreground)
                            .child("Inbox"),
                    )
                    .children(
                        self.description()
                            .into_iter()
                            .enumerate()
                            .map(|(ix, line)| {
                                let line_div = div()
                                    .debug_selector(move || format!("inbox description {ix}"))
                                    .text_size(font_size::SM)
                                    .text_color(s.muted_foreground);
                                match line.strip_suffix(SETTINGS_TAIL) {
                                    Some(lead) => line_div
                                        .flex()
                                        .child(SharedString::from(lead.to_owned()))
                                        .child(settings_link(cx))
                                        .child("."),
                                    None => line_div.child(line),
                                }
                            }),
                    ),
            )
            .child(div().flex_1())
            .child(close_prs::select_toggle(&self.close, cx))
            .child(
                div()
                    .text_size(font_size::SM)
                    .text_color(s.muted_foreground)
                    .child(synced),
            )
            .child(
                div().debug_selector(|| "sync now".into()).child(
                    Button::new("sync-now")
                        .variant(ButtonVariant::Outline)
                        .label("Sync now")
                        .disabled(self.syncing)
                        .on_click(cx.listener(|this, _, window, cx| this.sync(window, cx))),
                ),
            )
    }
}

/// "Settings", styled as core-gpui's link button (`primary`, underlined on hover), asking for
/// Settings when clicked.
fn settings_link(cx: &mut Context<InboxView>) -> impl IntoElement {
    let s = cx.theme().semantic();
    div()
        .id("inbox-settings-link")
        .debug_selector(|| "inbox settings link".into())
        .text_color(s.primary)
        .cursor_pointer()
        .hover(|style| style.underline())
        .on_click(cx.listener(|_, _, _, cx| cx.emit(OpenSettings)))
        .child("Settings")
}

/// The banner while no GitHub token works: what's wrong, and **Open Settings** to fix it.
fn logged_out_banner(cx: &mut Context<InboxView>) -> impl IntoElement {
    div()
        .flex_none()
        .debug_selector(|| "logged out banner".into())
        .child(
            Alert::new()
                .icon(IconName::Info)
                .title(AlertTitle::new(LOGGED_OUT))
                // Centred on the one-line alert, as Penpot's `Action` sits.
                .action(
                    AlertAction::new().top(space::S1_5).child(
                        div().debug_selector(|| "open settings".into()).child(
                            Button::new("inbox-open-settings")
                                .variant(ButtonVariant::Link)
                                .size(ButtonSize::Sm)
                                .label("Open Settings")
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(OpenSettings))),
                        ),
                    ),
                ),
        )
}

impl Render for InboxView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let rows = self.rows();
        if rows != self.shown {
            task_row::relist(&self.list, rows.len());
            self.shown = rows;
            let shown = close_prs::selectable(&self.shown);
            self.close.update(cx, |close, cx| close.retain(&shown, cx));
        }
        let layout = self.layout(cx);
        let select_all =
            close_prs::select_all(&self.close, "inbox", close_prs::selectable(&self.shown), cx);
        let bar = close_prs::selection_bar(
            &self.close,
            close_prs::picked(&self.close, &self.shown, cx),
            cx,
        );
        let rows = div()
            .debug_selector(|| "inbox list".into())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, ix, _, cx| this.item(ix, cx)),
                )
                .flex_1()
                .min_h_0(),
            );
        let table = task_row::table(
            "inbox-table",
            "inbox table",
            layout,
            select_all,
            rows,
            &self.list,
            cx.listener(|this, width: &Pixels, window, cx| this.fit(*width, window, cx)),
            cx,
        );
        // Penpot `Content`: Header / Columns / List, `space.4` apart, padded `space.6`.
        div()
            .size_full()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(space::S4)
            .p(space::S6)
            .bg(s.background)
            .child(self.header(cx))
            // Penpot "Inbox · logged out": a default `Alert` over the columns, its action a
            // link to Settings.
            .when(
                (self.snapshot.pr_sync.as_ref()).is_some_and(|s| s.login == GhLogin::LoggedOut),
                |content| content.child(logged_out_banner(cx)),
            )
            .child(table)
            .when(self.shown.is_empty(), |content| {
                content.child(
                    div()
                        .text_size(font_size::SM)
                        .text_color(s.muted_foreground)
                        .px(space::S2)
                        .child("Nothing in the inbox."),
                )
            })
            .children(bar)
            // The confirm dialog's host: out of the flow, so it takes no gap.
            .child(div().absolute().child(self.close.clone()))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use agentdeck_client::types::Task;
    use agentdeck_core::TaskKind;
    use brrritscold_core_gpui::{Root, Theme, ThemePreference};
    use gpui::{
        Entity, Modifiers, ScrollDelta, ScrollEvent, TestAppContext, VisualTestContext, point, px,
        size,
    };

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::overview::fixtures::{id, pr_sync, task, workspace};

    struct Harness {
        api: Arc<FakeApi>,
        view: Entity<InboxView>,
        synced: Rc<RefCell<usize>>,
        _synced: gpui::Subscription,
    }

    impl Harness {
        fn open(snapshot: Snapshot, cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            let api = Arc::new(FakeApi::with(snapshot.clone()));
            let mut view = None;
            let window = cx.add_window(|window, cx| {
                let api: Arc<dyn Api> = api.clone();
                let inbox = cx.new(|cx| InboxView {
                    snapshot,
                    ..InboxView::new(api, cx)
                });
                view = Some(inbox.clone());
                Root::new(inbox, window, cx)
            });
            let cx = VisualTestContext::from_window(*window, cx).into_mut();
            let view = view.unwrap();
            let synced = Rc::new(RefCell::new(0));
            let seen = synced.clone();
            let _synced = cx.update(|_, cx| {
                cx.subscribe(&view, move |_, _: &Synced, _| *seen.borrow_mut() += 1)
            });
            cx.run_until_parked();
            let harness = Harness {
                api,
                view,
                synced,
                _synced,
            };
            (harness, cx)
        }

        fn shown(&self, cx: &mut VisualTestContext) -> Vec<TaskId> {
            self.view.read_with(cx, |view, _| {
                view.rows().into_iter().map(|r| r.id).collect()
            })
        }

        fn draw(&self, cx: &mut VisualTestContext) {
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }

    /// An inbox PR and an inbox todo, a done inbox task, and a task in workspace 1.
    fn inbox() -> Snapshot {
        let ws = workspace(1, "a");
        Snapshot {
            tasks: vec![
                task(1, TaskKind::Pr, None),
                task(2, TaskKind::Plain, None),
                Task {
                    status: TaskStatus::Done,
                    ..task(3, TaskKind::Plain, None)
                },
                task(4, TaskKind::Plain, Some(ws.id)),
            ],
            workspaces: vec![ws],
            ..Snapshot::default()
        }
    }

    #[gpui::test]
    fn should_list_open_tasks_with_no_workspace(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(inbox(), cx);

        assert_eq!(harness.shown(cx), [id(1), id(2)]);
    }

    #[gpui::test]
    fn should_draw_in_both_themes(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(inbox(), cx);

        for preference in [ThemePreference::Light, ThemePreference::Dark] {
            cx.update(|_, cx| Theme::set_preference(preference, cx));
            cx.run_until_parked();
            harness.draw(cx);
        }
    }

    /// `n` todo tasks with no workspace.
    fn crowded(n: u32) -> Snapshot {
        Snapshot {
            tasks: (1..=n).map(|n| task(n, TaskKind::Pr, None)).collect(),
            ..Snapshot::default()
        }
    }

    fn bounds(task: u32, cx: &mut VisualTestContext) -> Option<gpui::Bounds<gpui::Pixels>> {
        cx.debug_bounds(format!("task {}", id::<TaskId>(task)).leak())
    }

    fn drawn(task: u32, cx: &mut VisualTestContext) -> bool {
        bounds(task, cx).is_some()
    }

    #[gpui::test]
    fn should_ask_for_the_task_sheet_when_a_title_is_clicked(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(inbox(), cx);
        let asked = Rc::new(RefCell::new(Vec::new()));
        let seen = asked.clone();
        let _asks = cx.update(|_, cx| {
            cx.subscribe(&harness.view, move |_, event: &OpenTask, _| {
                seen.borrow_mut().push(*event)
            })
        });
        harness.draw(cx);

        let title = format!("task title {}", id::<TaskId>(1));
        let bounds = cx.debug_bounds(title.leak()).expect("the title shows");
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        cx.run_until_parked();

        assert_eq!(*asked.borrow(), [OpenTask(id(1))]);
    }

    #[gpui::test]
    fn should_mark_a_row_whose_agent_comments_are_unseen_after_its_title(cx: &mut TestAppContext) {
        let mut snapshot = inbox();
        snapshot.tasks[1].unseen_agent_comments = 3;
        let (harness, cx) = Harness::open(snapshot, cx);
        harness.draw(cx);

        let mark =
            (cx.debug_bounds(format!("unseen {}", id::<TaskId>(2)).leak())).expect("task 2's mark");
        let title = (cx.debug_bounds(format!("task title {}", id::<TaskId>(2)).leak()))
            .expect("task 2's title");
        assert_eq!(mark.size, gpui::size(gpui::px(14.), gpui::px(14.)));
        assert!(mark.left() >= title.right(), "{mark:?} after {title:?}");
        assert!(
            cx.debug_bounds(format!("unseen {}", id::<TaskId>(1)).leak())
                .is_none()
        );
    }

    #[gpui::test]
    fn should_draw_only_the_rows_in_view(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(crowded(300), cx);
        harness.draw(cx);

        assert!(drawn(1, cx));
        assert!(!drawn(300, cx));
    }

    #[gpui::test]
    fn should_draw_the_last_row_once_scrolled_to(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(crowded(300), cx);
        harness.draw(cx);
        let at = bounds(1, cx).expect("the first row").center();

        // A wheel scrolls a step at a time; the list measures rows as they come into view.
        for _ in 0..100 {
            if drawn(300, cx) {
                break;
            }
            cx.simulate_event(ScrollEvent {
                position: at,
                delta: ScrollDelta::Pixels(point(px(0.), px(-2000.))),
                ..ScrollEvent::default()
            });
            harness.draw(cx);
        }

        assert!(drawn(300, cx));
        assert!(!drawn(1, cx));
    }

    #[gpui::test]
    fn should_scroll_a_narrow_table_down_not_sideways(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(crowded(300), cx);
        cx.simulate_resize(size(px(800.), px(600.)));
        harness.draw(cx);
        let before = bounds(2, cx).expect("the second row");

        cx.simulate_event(ScrollEvent {
            position: before.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(-20.))),
            ..ScrollEvent::default()
        });
        harness.draw(cx);

        let after = bounds(2, cx).expect("the second row");
        assert_eq!(after.origin.x, before.origin.x);
        assert!(after.origin.y < before.origin.y);
    }

    #[gpui::test]
    fn should_stay_at_the_top_when_tasks_arrive(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(crowded(300), cx);
        harness.draw(cx);
        let top = bounds(1, cx).expect("the first row").origin.y;

        harness
            .view
            .update(cx, |view, cx| view.set_snapshot(crowded(400), cx));
        harness.draw(cx);
        harness.draw(cx);

        assert_eq!(bounds(1, cx).map(|b| b.origin.y), Some(top));
    }

    #[gpui::test]
    fn should_start_at_the_top_when_tasks_come_to_an_empty_list(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(Snapshot::default(), cx);
        harness.draw(cx);

        harness
            .view
            .update(cx, |view, cx| view.set_snapshot(crowded(300), cx));
        harness.draw(cx);

        assert!(drawn(1, cx));
    }

    #[gpui::test]
    fn should_sync_when_sync_now_is_clicked_and_say_so(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(inbox(), cx);
        harness.draw(cx);

        let button = cx.debug_bounds("sync now").expect("the Sync now button");
        cx.simulate_click(button.center(), Modifiers::default());
        cx.run_until_parked();

        assert_eq!(*harness.api.syncs.lock().unwrap(), 1);
        assert_eq!(*harness.synced.borrow(), 1);
    }

    #[gpui::test]
    fn should_say_how_far_a_sync_it_did_not_start_is(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(inbox(), cx);
        let mut snapshot = inbox();
        pr_sync(&mut snapshot).progress = Some(agentdeck_client::types::PrSyncProgress {
            prs_done: 50,
            prs: 271,
            stage: agentdeck_client::types::PrSyncStage::Listing,
        });

        cx.update(|_, cx| {
            harness
                .view
                .update(cx, |view, cx| view.set_snapshot(snapshot, cx))
        });

        let label = cx.update(|_, cx| harness.view.read(cx).sync_label(chrono::Utc::now()));
        assert_eq!(label, "Syncing… 50 of 271 PRs");
    }

    /// [`inbox`] signed out of GitHub.
    fn logged_out() -> Snapshot {
        let mut snapshot = inbox();
        pr_sync(&mut snapshot).login = agentdeck_client::types::GhLogin::LoggedOut;
        pr_sync(&mut snapshot).last_error = Some("gh is not logged in".into());
        snapshot
    }

    #[gpui::test]
    fn should_show_the_banner_while_not_signed_in(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(logged_out(), cx);
        harness.draw(cx);

        assert!(cx.debug_bounds("logged out banner").is_some());
        let label = cx.update(|_, cx| harness.view.read(cx).sync_label(chrono::Utc::now()));
        assert_eq!(label, "Not signed in");
    }

    #[gpui::test]
    fn should_ask_for_settings_from_the_banner(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(logged_out(), cx);
        let asked = Rc::new(RefCell::new(0));
        let seen = asked.clone();
        let _asks = cx.update(|_, cx| {
            cx.subscribe(&harness.view, move |_, _: &OpenSettings, _| {
                *seen.borrow_mut() += 1;
            })
        });
        harness.draw(cx);

        let banner = cx.debug_bounds("logged out banner").expect("the banner");
        let link = cx.debug_bounds("open settings").expect("Open Settings");
        assert!(banner.contains(&link.center()), "{link:?} in {banner:?}");
        cx.simulate_click(link.center(), Modifiers::default());
        cx.run_until_parked();

        assert_eq!(*asked.borrow(), 1);
    }

    #[gpui::test]
    fn should_hide_the_banner_once_signed_in(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(logged_out(), cx);
        harness.draw(cx);

        cx.update(|_, cx| {
            harness
                .view
                .update(cx, |view, cx| view.set_snapshot(inbox(), cx))
        });
        harness.draw(cx);

        assert!(cx.debug_bounds("logged out banner").is_none());
    }

    #[gpui::test]
    fn should_report_a_failed_sync_without_saying_synced(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(inbox(), cx);
        harness.api.fail_next("gh is logged out");

        cx.update(|window, cx| harness.view.update(cx, |view, cx| view.sync(window, cx)));
        cx.run_until_parked();

        assert_eq!(*harness.synced.borrow(), 0);
        harness.draw(cx);
    }

    mod description {
        use super::*;

        const ABOUT: &str = "Tasks with no workspace. A PR lands here when no workspace has a \
                             clone of its repo.";

        /// [`inbox`] with the PR sync status read and a cutoff of `days`.
        fn with_cutoff(days: u32) -> Snapshot {
            let mut snapshot = inbox();
            pr_sync(&mut snapshot).max_age_days = days;
            snapshot
        }

        fn description(harness: &Harness, cx: &mut VisualTestContext) -> Vec<String> {
            harness.view.read_with(cx, |view, _| {
                view.description().iter().map(|l| l.to_string()).collect()
            })
        }

        #[gpui::test]
        fn should_say_which_prs_the_cutoff_leaves_out(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(with_cutoff(90), cx);

            assert_eq!(
                description(&harness, cx),
                [
                    ABOUT,
                    "PRs not updated in the last 90 days are left out; change that in Settings."
                ]
            );
        }

        #[gpui::test]
        fn should_say_day_not_days_for_a_one_day_cutoff(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(with_cutoff(1), cx);

            assert_eq!(
                description(&harness, cx)[1..],
                ["PRs not updated in the last 1 day are left out; change that in Settings."]
            );
        }

        #[gpui::test]
        fn should_say_every_open_pr_syncs_with_no_cutoff(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(with_cutoff(0), cx);

            assert_eq!(description(&harness, cx), [ABOUT, "Every open PR syncs."]);
        }

        #[gpui::test]
        fn should_show_only_the_first_line_before_the_status_is_read(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(inbox(), cx);
            harness.draw(cx);

            assert_eq!(description(&harness, cx), [ABOUT]);
            assert!(cx.debug_bounds("inbox description 0").is_some());
            assert!(cx.debug_bounds("inbox description 1").is_none());
        }

        #[gpui::test]
        fn should_ask_for_settings_when_its_link_is_clicked(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(with_cutoff(90), cx);
            let asked = Rc::new(RefCell::new(0));
            let seen = asked.clone();
            let _asks = cx.update(|_, cx| {
                cx.subscribe(&harness.view, move |_, _: &OpenSettings, _| {
                    *seen.borrow_mut() += 1;
                })
            });
            harness.draw(cx);

            let line = cx.debug_bounds("inbox description 1").expect("line 2");
            let link = cx.debug_bounds("inbox settings link").expect("the link");
            assert!(line.contains(&link.center()), "{link:?} in {line:?}");
            cx.simulate_click(link.center(), Modifiers::default());
            cx.run_until_parked();

            assert_eq!(*asked.borrow(), 1);
        }

        #[gpui::test]
        fn should_link_nothing_when_every_open_pr_syncs(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(with_cutoff(0), cx);
            harness.draw(cx);

            assert!(cx.debug_bounds("inbox description 1").is_some());
            assert!(cx.debug_bounds("inbox settings link").is_none());
        }

        #[gpui::test]
        fn should_draw_the_cutoff_under_the_first_line(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(with_cutoff(90), cx);
            harness.draw(cx);

            let first = cx.debug_bounds("inbox description 0").expect("line 1");
            let second = cx.debug_bounds("inbox description 1").expect("line 2");
            assert!(second.origin.y >= first.bottom());
        }

        #[gpui::test]
        fn should_say_the_cutoff_while_gh_is_logged_out(cx: &mut TestAppContext) {
            let mut snapshot = logged_out();
            pr_sync(&mut snapshot).max_age_days = 30;
            let (harness, cx) = Harness::open(snapshot, cx);
            harness.draw(cx);

            assert!(cx.debug_bounds("logged out banner").is_some());
            assert_eq!(
                description(&harness, cx)[1..],
                ["PRs not updated in the last 30 days are left out; change that in Settings."]
            );
        }
    }

    mod columns {
        use gpui::{px, size};

        use super::*;

        /// The Inbox in a window `width` wide, beside the sidebar, its columns fitted.
        fn open_in(width: f32, cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            let (harness, cx) = Harness::open(inbox(), cx);
            cx.simulate_resize(size(px(width - 261.), px(800.)));
            // No draw here: the redraw the refit asks for must come by itself, as in the app.
            cx.run_until_parked();
            (harness, cx)
        }

        fn shown(selector: String, cx: &mut VisualTestContext) -> bool {
            cx.debug_bounds(selector.leak()).is_some()
        }

        #[gpui::test]
        fn should_not_scroll_sideways_at_1200(cx: &mut TestAppContext) {
            let (_, cx) = open_in(1200., cx);

            let table = cx.debug_bounds("inbox table").expect("the table");
            let row = bounds(1, cx).expect("a row");
            assert!(row.right() <= table.right(), "{row:?} overflows {table:?}");
            for column in ["Checks", "Review"] {
                assert!(shown(format!("column {column}"), cx), "{column}");
            }
        }

        #[gpui::test]
        fn should_drop_the_checks_column_first_when_the_table_does_not_fit(
            cx: &mut TestAppContext,
        ) {
            let (_, cx) = open_in(1061., cx);

            assert!(!shown("column Checks".into(), cx));
            assert!(!shown(format!("checks {}", id::<TaskId>(1)), cx));
            assert!(shown("column Review".into(), cx));
            assert!(shown(format!("review {}", id::<TaskId>(1)), cx));
        }

        #[gpui::test]
        fn should_drop_review_after_checks(cx: &mut TestAppContext) {
            let (_, cx) = open_in(961., cx);

            assert!(!shown("column Checks".into(), cx));
            assert!(!shown("column Review".into(), cx));
            assert!(!shown(format!("review {}", id::<TaskId>(1)), cx));
        }
    }

    mod scrollbar {
        use gpui::{ScrollDelta, ScrollEvent, point, px, size};

        use super::*;
        use crate::scrollbar::testing::thumb;

        fn list(cx: &mut VisualTestContext) -> gpui::Bounds<gpui::Pixels> {
            cx.debug_bounds("inbox list").expect("the list")
        }

        #[gpui::test]
        fn should_show_a_scrollbar_when_the_list_overflows(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(crowded(300), cx);
            harness.draw(cx);

            let list = list(cx);
            assert!(thumb(task_row::scrollbar_area(list), cx).is_some());
        }

        #[gpui::test]
        fn should_hide_the_scrollbar_when_everything_fits(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(inbox(), cx);
            harness.draw(cx);

            let list = list(cx);
            assert_eq!(thumb(task_row::scrollbar_area(list), cx), None);
        }

        #[gpui::test]
        fn should_move_the_thumb_when_the_list_scrolls(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(crowded(300), cx);
            harness.draw(cx);
            let list = list(cx);
            let before = thumb(task_row::scrollbar_area(list), cx).expect("a thumb");

            cx.simulate_event(ScrollEvent {
                position: list.center(),
                delta: ScrollDelta::Pixels(point(px(0.), px(-2000.))),
                ..ScrollEvent::default()
            });
            harness.draw(cx);

            let after = thumb(task_row::scrollbar_area(list), cx).expect("a thumb");
            assert!(after.origin.y > before.origin.y);
        }

        #[gpui::test]
        fn should_keep_the_scrollbar_in_view_when_the_table_is_wider_than_the_window(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(crowded(300), cx);
            cx.simulate_resize(size(px(600.), px(600.)));
            harness.draw(cx);
            let table = cx.debug_bounds("inbox table").expect("the table");
            assert!(
                list(cx).right() > table.right(),
                "the table fits the window"
            );

            let shown = thumb(task_row::scrollbar_area(table), cx).expect("a thumb in view");
            let list = list(cx);
            assert!(shown.top() >= list.top() && shown.bottom() <= list.bottom());
            cx.simulate_event(ScrollEvent {
                position: table.center(),
                delta: ScrollDelta::Pixels(point(px(-200.), px(0.))),
                ..ScrollEvent::default()
            });
            harness.draw(cx);
            assert!(
                thumb(task_row::scrollbar_area(table), cx).is_some(),
                "scrolled sideways"
            );
        }
    }

    mod closing_prs {
        use agentdeck_core::PrState;

        use super::*;
        use crate::close_prs::Target;
        use crate::overview::fixtures::pr_task;

        /// Open PRs 1 and 4, merged PR 2 (still in review) and plain task 3, in the inbox.
        fn prs() -> Snapshot {
            Snapshot {
                tasks: vec![
                    pr_task(1, "acme/widgets", PrState::Open, None),
                    pr_task(2, "acme/widgets", PrState::Merged, None),
                    task(3, TaskKind::Plain, None),
                    pr_task(4, "acme/gadgets", PrState::Open, None),
                ],
                ..Snapshot::default()
            }
        }

        fn click(selector: String, cx: &mut VisualTestContext) {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let bounds = cx.debug_bounds(selector.clone().leak()).expect(&selector);
            cx.simulate_click(bounds.center(), Modifiers::default());
            cx.run_until_parked();
        }

        fn shown_at(selector: String, cx: &mut VisualTestContext) -> bool {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.debug_bounds(selector.leak()).is_some()
        }

        fn picked(harness: &Harness, cx: &mut VisualTestContext) -> Vec<TaskId> {
            harness.view.read_with(cx, |view, cx| {
                view.close_prs().read(cx).selection().ids().to_vec()
            })
        }

        fn selecting(harness: &Harness, cx: &mut VisualTestContext) -> bool {
            harness
                .view
                .read_with(cx, |view, cx| view.close_prs().read(cx).active())
        }

        fn asking(harness: &Harness, cx: &mut VisualTestContext) -> bool {
            harness
                .view
                .read_with(cx, |view, cx| view.close_prs().read(cx).asking())
        }

        fn toasts(cx: &mut VisualTestContext) -> Vec<String> {
            cx.update(|_, cx| crate::notice::shown(cx))
                .into_iter()
                .map(|notice| notice.title.to_string())
                .collect()
        }

        fn row_selector(what: &str, task: u32) -> String {
            format!("inbox {what} {}", id::<TaskId>(task))
        }

        /// Select mode on, PRs 1 and 4 picked from the columns' checkbox, and Close PRs
        /// pressed.
        fn asked_to_close_both(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            click("inbox select".into(), cx);
            click("inbox select all inbox".into(), cx);
            click("inbox close prs".into(), cx);
            (harness, cx)
        }

        #[gpui::test]
        fn should_show_a_checkbox_only_on_open_pr_rows_in_select_mode(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(prs(), cx);
            assert!(!shown_at(row_selector("select", 1), cx));

            click("inbox select".into(), cx);

            assert!(shown_at(row_selector("select", 1), cx));
            assert!(shown_at(row_selector("select", 4), cx));
            assert!(!shown_at(row_selector("select", 2), cx));
            assert!(!shown_at(row_selector("select", 3), cx));
            assert!(shown_at("inbox selection bar".into(), cx));
        }

        #[gpui::test]
        fn should_keep_titles_aligned_on_rows_with_nothing_to_select(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(prs(), cx);
            click("inbox select".into(), cx);

            let left = |task: u32, cx: &mut VisualTestContext| {
                let selector = format!("task title {}", id::<TaskId>(task));
                cx.debug_bounds(selector.leak()).expect("a title").left()
            };
            assert_eq!(left(1, cx), left(3, cx));
        }

        #[gpui::test]
        fn should_put_a_menu_on_every_row_plain_ones_too(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(prs(), cx);

            assert!(shown_at(row_selector("menu", 1), cx));
            assert!(shown_at(row_selector("menu", 2), cx));
            assert!(shown_at(row_selector("menu", 3), cx));
        }

        #[gpui::test]
        fn should_pick_every_open_pr_from_the_columns_checkbox_then_none(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            click("inbox select".into(), cx);

            click("inbox select all inbox".into(), cx);
            assert_eq!(picked(&harness, cx), [id(1), id(4)]);
            click("inbox select all inbox".into(), cx);
            assert!(picked(&harness, cx).is_empty());
        }

        #[gpui::test]
        fn should_pick_all_from_an_indeterminate_columns_checkbox(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            click("inbox select".into(), cx);
            click(row_selector("select", 4), cx);
            assert_eq!(picked(&harness, cx), [id(4)]);

            click("inbox select all inbox".into(), cx);

            assert_eq!(picked(&harness, cx), [id(4), id(1)]);
        }

        #[gpui::test]
        fn should_ask_before_closing_the_picked_prs(cx: &mut TestAppContext) {
            let (harness, cx) = asked_to_close_both(cx);

            assert!(asking(&harness, cx));
            assert!(harness.api.closes.lock().unwrap().is_empty());
        }

        #[gpui::test]
        fn should_close_on_enter_then_end_select_mode(cx: &mut TestAppContext) {
            let (harness, cx) = asked_to_close_both(cx);

            cx.simulate_keystrokes("enter");
            cx.run_until_parked();

            assert_eq!(*harness.api.closes.lock().unwrap(), [vec![id(1), id(4)]]);
            assert!(!asking(&harness, cx));
            assert_eq!(toasts(cx).last().map(String::as_str), Some("Closed 2 PRs"));
            assert!(!selecting(&harness, cx));
        }

        #[gpui::test]
        fn should_close_once_from_the_dialogs_close_button(cx: &mut TestAppContext) {
            let (harness, cx) = asked_to_close_both(cx);
            cx.update(|window, cx| window.draw(cx).clear(cx));

            let button = cx
                .debug_bounds("inbox close confirm")
                .expect("the Close button");
            cx.simulate_click(button.center(), Modifiers::default());
            cx.run_until_parked();

            assert_eq!(harness.api.closes.lock().unwrap().len(), 1);
            assert!(!asking(&harness, cx));
        }

        #[gpui::test]
        fn should_close_nothing_when_cancelled(cx: &mut TestAppContext) {
            let (harness, cx) = asked_to_close_both(cx);

            cx.simulate_keystrokes("escape");
            cx.run_until_parked();

            assert!(!asking(&harness, cx));
            assert!(harness.api.closes.lock().unwrap().is_empty());
            assert!(selecting(&harness, cx));
        }

        #[gpui::test]
        fn should_keep_the_failed_rows_picked_with_their_error(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            harness
                .api
                .refuse_close(id(4), "Resource not accessible by personal access token");
            click("inbox select".into(), cx);
            click("inbox select all inbox".into(), cx);
            click("inbox close prs".into(), cx);

            cx.simulate_keystrokes("enter");
            cx.run_until_parked();

            let shown = cx.update(|_, cx| crate::notice::shown(cx));
            let toast = shown.last().expect("a toast");
            assert_eq!(toast.title, "Closed 1 of 2");
            assert_eq!(
                toast.description.as_deref(),
                Some("Hover the marked PR to see why.")
            );
            assert!(selecting(&harness, cx));
            assert_eq!(picked(&harness, cx), [id(4)]);
            assert!(shown_at(
                row_selector("close error", 4).replace("inbox ", ""),
                cx
            ));
        }

        /// How wide core-gpui's `TooltipContent` is when it reads `text`: the text, xs in the
        /// sans family `Root` sets, with `px-3` either side.
        fn tooltip_width(text: &str, cx: &mut VisualTestContext) -> gpui::Pixels {
            use brrritscold_core_gpui_tokens::primitive::font_family;

            cx.update(|window, _| {
                let run = gpui::TextRun {
                    len: text.len(),
                    font: gpui::font(font_family::SANS.family),
                    color: gpui::Hsla::default(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let size = font_size::XS.to_pixels(window.rem_size());
                let line =
                    (window.text_system()).shape_line(text.to_owned().into(), size, &[run], None);
                line.width + space::S3 * 2.
            })
        }

        /// Select mode on, PRs 1 and 4 picked and closed, with GitHub refusing each of
        /// `refusals`, its task and why; then the daemon's `changed` for the ones that closed.
        fn closed_with_refusals<'a>(
            refusals: &[(u32, &str)],
            cx: &'a mut TestAppContext,
        ) -> (Harness, &'a mut VisualTestContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            for (task, why) in refusals {
                harness.api.refuse_close(id(*task), why);
            }
            click("inbox select".into(), cx);
            click("inbox select all inbox".into(), cx);
            click("inbox close prs".into(), cx);
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
            let mut snapshot = prs();
            for task in &mut snapshot.tasks {
                let refused = refusals.iter().any(|(n, _)| id::<TaskId>(*n) == task.id);
                if let Some(pr) = &mut task.pr
                    && pr.state == PrState::Open
                    && !refused
                {
                    pr.state = PrState::Closed;
                    task.status = TaskStatus::Done;
                }
            }
            harness
                .view
                .update(cx, |view, cx| view.set_snapshot(snapshot, cx));
            cx.run_until_parked();
            (harness, cx)
        }

        fn bounds(selector: String, cx: &mut VisualTestContext) -> gpui::Bounds<gpui::Pixels> {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.debug_bounds(selector.clone().leak()).expect(&selector)
        }

        /// The tooltip shown while the pointer is on `at`, then the pointer moves off.
        fn tooltip_on(
            at: gpui::Point<gpui::Pixels>,
            cx: &mut VisualTestContext,
        ) -> Option<gpui::Bounds<gpui::Pixels>> {
            cx.simulate_mouse_move(at, None, Modifiers::default());
            cx.run_until_parked();
            let tooltip = cx.debug_bounds("tooltip-content");
            cx.simulate_mouse_move(point(px(1.), px(1.)), None, Modifiers::default());
            cx.run_until_parked();
            tooltip
        }

        #[gpui::test]
        fn should_show_why_a_pr_did_not_close_on_hovering_its_title(cx: &mut TestAppContext) {
            let (_harness, cx) = closed_with_refusals(&[(4, "Must have admin rights")], cx);
            let title = bounds(format!("task title {}", id::<TaskId>(4)), cx);
            assert!(cx.debug_bounds("tooltip-content").is_none());

            let tooltip = tooltip_on(title.center(), cx).expect("a tooltip on hover");

            assert!(
                tooltip.bottom() < title.top(),
                "{tooltip:?} above {title:?}"
            );
            let reads = tooltip_width("Must have admin rights", cx);
            assert!(
                (tooltip.size.width - reads).abs() < px(1.),
                "{tooltip:?} is as wide as GitHub's message, {reads:?}"
            );
            assert!(cx.debug_bounds("tooltip-content").is_none());
        }

        #[gpui::test]
        fn should_mark_only_a_failed_row_just_after_its_title(cx: &mut TestAppContext) {
            let (_harness, cx) = closed_with_refusals(&[(4, "Must have admin rights")], cx);

            let title = bounds(format!("task title {}", id::<TaskId>(4)), cx);
            let mark = bounds(format!("close error mark {}", id::<TaskId>(4)), cx);

            assert!(
                (mark.left() - title.right() - px(6.)).abs() < px(0.5),
                "{mark:?} 6px after {title:?}"
            );
            assert_eq!(mark.size.width, px(14.));
            for task in [2, 3] {
                assert!(!shown_at(
                    format!("close error mark {}", id::<TaskId>(task)),
                    cx
                ));
            }
        }

        #[gpui::test]
        fn should_show_why_a_pr_did_not_close_on_hovering_its_mark(cx: &mut TestAppContext) {
            let (_harness, cx) = closed_with_refusals(&[(4, "Must have admin rights")], cx);
            let mark = bounds(format!("close error mark {}", id::<TaskId>(4)), cx);

            let tooltip = tooltip_on(mark.center(), cx).expect("a tooltip on hovering the mark");

            assert!(tooltip.bottom() < mark.top(), "{tooltip:?} above {mark:?}");
            let reads = tooltip_width("Must have admin rights", cx);
            assert!(
                (tooltip.size.width - reads).abs() < px(1.),
                "{tooltip:?} reads GitHub's message, {reads:?}"
            );
        }

        #[gpui::test]
        fn should_wrap_a_long_reason_within_320px(cx: &mut TestAppContext) {
            let long = "Could not resolve to a Repository with the name \
                        'bRRRITSCOLD/ts-packages'.";
            let (_harness, cx) =
                closed_with_refusals(&[(1, "Must have admin rights"), (4, long)], cx);
            let short = bounds(format!("task title {}", id::<TaskId>(1)), cx);
            let short = tooltip_on(short.center(), cx).expect("the short reason's tooltip");
            let title = bounds(format!("task title {}", id::<TaskId>(4)), cx);

            let tooltip = tooltip_on(title.center(), cx).expect("the long reason's tooltip");

            assert!(
                tooltip.size.width <= px(320.),
                "{tooltip:?} no wider than 320px"
            );
            assert!(
                tooltip.size.width > tooltip_width("Could not resolve to a Repository", cx),
                "{tooltip:?} fills its lines"
            );
            assert!(
                tooltip.size.height > short.size.height + px(10.),
                "{tooltip:?} wraps onto a second line, taller than one line's {short:?}"
            );
        }

        #[gpui::test]
        fn should_say_why_when_the_close_request_fails(cx: &mut TestAppContext) {
            let (harness, cx) = asked_to_close_both(cx);
            harness.api.fail_next("connection refused");

            cx.simulate_keystrokes("enter");
            cx.run_until_parked();

            let shown = cx.update(|_, cx| crate::notice::shown(cx));
            let toast = shown.last().expect("a toast");
            assert_eq!(toast.title, "Couldn't close PRs");
            assert_eq!(toast.description.as_deref(), Some("connection refused"));
            assert_eq!(picked(&harness, cx), [id(1), id(4)]);
        }

        #[gpui::test]
        fn should_disable_close_prs_while_nothing_is_picked(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            click("inbox select".into(), cx);

            click("inbox close prs".into(), cx);

            assert!(!asking(&harness, cx));
        }

        #[gpui::test]
        fn should_end_select_mode_from_cancel_clearing_the_selection(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            click("inbox select".into(), cx);
            click(row_selector("select", 1), cx);

            click("inbox cancel select".into(), cx);

            assert!(!selecting(&harness, cx));
            assert!(picked(&harness, cx).is_empty());
            assert!(!shown_at("inbox selection bar".into(), cx));
        }

        #[gpui::test]
        fn should_drop_a_row_that_leaves_the_list_from_the_selection(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            click("inbox select".into(), cx);
            click("inbox select all inbox".into(), cx);

            let mut snapshot = prs();
            snapshot.tasks[0].status = TaskStatus::Done;
            harness
                .view
                .update(cx, |view, cx| view.set_snapshot(snapshot, cx));
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.run_until_parked();

            assert_eq!(picked(&harness, cx), [id(4)]);
        }

        mod while_closing {
            use super::*;

            fn closing(harness: &Harness, cx: &mut VisualTestContext) -> Option<(usize, usize)> {
                harness
                    .view
                    .read_with(cx, |view, cx| view.close_prs().read(cx).closing())
            }

            /// `prs()` with the PRs of `closed` stored closed and their tasks done, as the
            /// daemon leaves them.
            fn with_closed(closed: &[u32]) -> Snapshot {
                let mut snapshot = prs();
                for task in &mut snapshot.tasks {
                    if closed.iter().any(|n| task.id == id::<TaskId>(*n)) {
                        task.status = TaskStatus::Done;
                        if let Some(pr) = &mut task.pr {
                            pr.state = PrState::Closed;
                        }
                    }
                }
                snapshot
            }

            fn show(harness: &Harness, snapshot: Snapshot, cx: &mut VisualTestContext) {
                harness
                    .view
                    .update(cx, |view, cx| view.set_snapshot(snapshot, cx));
                cx.update(|window, cx| window.draw(cx).clear(cx));
                cx.run_until_parked();
            }

            #[gpui::test]
            fn should_count_up_as_each_closed_prs_change_arrives(cx: &mut TestAppContext) {
                let (harness, cx) = asked_to_close_both(cx);
                let release = harness.api.hold_close();

                cx.simulate_keystrokes("enter");
                cx.run_until_parked();
                assert_eq!(closing(&harness, cx), Some((0, 2)));
                assert!(selecting(&harness, cx));

                show(&harness, with_closed(&[1]), cx);
                assert_eq!(closing(&harness, cx), Some((1, 2)));
                show(&harness, with_closed(&[1, 4]), cx);
                assert_eq!(closing(&harness, cx), Some((2, 2)));

                release.send(()).unwrap();
                cx.run_until_parked();
                assert_eq!(closing(&harness, cx), None);
                assert_eq!(toasts(cx).last().map(String::as_str), Some("Closed 2 PRs"));
                assert!(!selecting(&harness, cx));
            }

            #[gpui::test]
            fn should_not_close_them_twice(cx: &mut TestAppContext) {
                let (harness, cx) = asked_to_close_both(cx);
                let _release = harness.api.hold_close();
                cx.simulate_keystrokes("enter");
                cx.run_until_parked();

                click("inbox close prs".into(), cx);

                assert!(!asking(&harness, cx));
                assert_eq!(harness.api.closes.lock().unwrap().len(), 1);
            }

            #[gpui::test]
            fn should_keep_select_mode_and_the_picks_as_they_are(cx: &mut TestAppContext) {
                let (harness, cx) = asked_to_close_both(cx);
                let _release = harness.api.hold_close();
                cx.simulate_keystrokes("enter");
                cx.run_until_parked();

                click("inbox cancel select".into(), cx);
                click("inbox select".into(), cx);
                click(row_selector("select", 4), cx);
                click("inbox select all inbox".into(), cx);

                assert!(selecting(&harness, cx));
                assert_eq!(picked(&harness, cx), [id(1), id(4)]);
            }

            #[gpui::test]
            fn should_keep_the_failed_row_picked_once_answered(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(prs(), cx);
                harness.api.refuse_close(id(4), "Resource not accessible");
                click("inbox select".into(), cx);
                click("inbox select all inbox".into(), cx);
                click("inbox close prs".into(), cx);
                let release = harness.api.hold_close();
                cx.simulate_keystrokes("enter");
                cx.run_until_parked();
                show(&harness, with_closed(&[1]), cx);
                assert_eq!(closing(&harness, cx), Some((1, 2)));

                release.send(()).unwrap();
                cx.run_until_parked();

                assert_eq!(closing(&harness, cx), None);
                assert_eq!(toasts(cx).last().map(String::as_str), Some("Closed 1 of 2"));
                assert_eq!(picked(&harness, cx), [id(4)]);
                click("inbox close prs".into(), cx);
                assert!(asking(&harness, cx), "Close PRs works again");
            }

            #[gpui::test]
            fn should_end_when_the_request_fails(cx: &mut TestAppContext) {
                let (harness, cx) = asked_to_close_both(cx);
                harness.api.fail_next("connection refused");

                cx.simulate_keystrokes("enter");
                cx.run_until_parked();

                assert_eq!(closing(&harness, cx), None);
                assert_eq!(picked(&harness, cx), [id(1), id(4)]);
            }

            #[gpui::test]
            fn should_draw_the_bar_while_closing_in_both_themes(cx: &mut TestAppContext) {
                let (harness, cx) = asked_to_close_both(cx);
                let _release = harness.api.hold_close();
                cx.simulate_keystrokes("enter");
                cx.run_until_parked();

                for preference in [ThemePreference::Light, ThemePreference::Dark] {
                    cx.update(|_, cx| Theme::set_preference(preference, cx));
                    cx.run_until_parked();
                    cx.update(|window, cx| window.draw(cx).clear(cx));
                }
                assert!(shown_at("inbox selection bar".into(), cx));
            }
        }

        mod from_a_rows_menu {
            use super::*;

            /// Opens `task`'s `…` menu and picks its `n`th item by keyboard.
            fn pick(task: u32, n: usize, cx: &mut VisualTestContext) {
                click(row_selector("menu", task), cx);
                cx.update(|window, cx| window.draw(cx).clear(cx));
                cx.simulate_keystrokes(&format!("{}enter", "down ".repeat(n)));
                cx.run_until_parked();
            }

            #[gpui::test]
            fn should_open_a_merged_prs_page_on_github(cx: &mut TestAppContext) {
                let (_harness, cx) = Harness::open(prs(), cx);
                let opener = cx.update(|_, cx| crate::terminal::open::fake::install(cx));

                pick(2, 3, cx);

                assert_eq!(
                    *opener.started.lock().unwrap(),
                    [[
                        "xdg-open".to_owned(),
                        "https://github.com/acme/widgets/pull/2".to_owned()
                    ]]
                );
            }

            #[gpui::test]
            fn should_ask_to_edit_the_task_from_its_first_item(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(prs(), cx);
                let asked = Rc::new(RefCell::new(Vec::new()));
                let _subscription = cx.update(|_, cx| {
                    let asked = asked.clone();
                    cx.subscribe(&harness.view, move |_, event: &EditTask, _| {
                        asked.borrow_mut().push(*event);
                    })
                });

                pick(3, 1, cx);

                assert_eq!(*asked.borrow(), [EditTask(id(3))]);
            }

            #[gpui::test]
            fn should_open_an_open_prs_page_from_the_item_after_send_to_agent(
                cx: &mut TestAppContext,
            ) {
                let (harness, cx) = Harness::open(prs(), cx);
                let opener = cx.update(|_, cx| crate::terminal::open::fake::install(cx));

                pick(1, 3, cx);

                assert_eq!(
                    *opener.started.lock().unwrap(),
                    [[
                        "xdg-open".to_owned(),
                        "https://github.com/acme/widgets/pull/1".to_owned()
                    ]]
                );
                assert!(!asking(&harness, cx));
            }

            #[gpui::test]
            fn should_copy_the_task_id_from_the_item_after_open_on_github(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(prs(), cx);

                pick(2, 4, cx);

                let copied = cx.read_from_clipboard().and_then(|item| item.text());
                assert_eq!(copied, Some(id::<TaskId>(2).to_string()));
                assert_eq!(
                    toasts(cx).last().map(String::as_str),
                    Some("Copied task id")
                );
                assert!(!asking(&harness, cx));
            }

            #[gpui::test]
            fn should_ask_to_close_from_the_item_after_the_separator(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(prs(), cx);
                let opener = cx.update(|_, cx| crate::terminal::open::fake::install(cx));

                pick(1, 5, cx);

                assert!(asking(&harness, cx));
                assert!(opener.started.lock().unwrap().is_empty());
            }

            fn ask(harness: &Harness, task: u32, cx: &mut VisualTestContext) {
                let close = harness
                    .view
                    .read_with(cx, |view, _| view.close_prs().clone());
                let row = harness.view.read_with(cx, |view, _| {
                    (view.rows().into_iter())
                        .find(|row| row.id == id::<TaskId>(task))
                        .expect("the row")
                });
                let target = Target::of(&row).expect("an open PR");
                cx.update(|window, cx| {
                    close.update(cx, |close, cx| close.ask_one(target, window, cx));
                });
                cx.run_until_parked();
            }

            #[gpui::test]
            fn should_ask_then_close_that_pr_alone(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(prs(), cx);
                ask(&harness, 4, cx);
                assert!(asking(&harness, cx));

                cx.simulate_keystrokes("enter");
                cx.run_until_parked();

                assert_eq!(*harness.api.closes.lock().unwrap(), [vec![id(4)]]);
                assert_eq!(toasts(cx).last().map(String::as_str), Some("Closed 1 PR"));
            }

            #[gpui::test]
            fn should_name_the_pr_when_github_refuses(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(prs(), cx);
                harness.api.refuse_close(id(4), "Must have admin rights");
                ask(&harness, 4, cx);

                cx.simulate_keystrokes("enter");
                cx.run_until_parked();

                let shown = cx.update(|_, cx| crate::notice::shown(cx));
                let toast = shown.last().expect("a toast");
                assert_eq!(toast.title, "Couldn't close gadgets #4");
                assert_eq!(toast.description.as_deref(), Some("Must have admin rights"));
            }
        }
    }

    mod spin {
        use agentdeck_core::{ChecksState, PrState};

        use super::*;
        use crate::overview::fixtures::pr_task;
        use crate::state_dot::tests::{activate, redraws_in_one_second};

        /// An inbox PR whose checks are running.
        fn running() -> Snapshot {
            let mut task = pr_task(1, "acme/widgets", PrState::Open, None);
            if let Some(pr) = task.pr.as_mut() {
                pr.checks = Some(ChecksState::Pending);
            }
            Snapshot {
                tasks: vec![task],
                ..Snapshot::default()
            }
        }

        #[gpui::test]
        fn should_hold_running_checks_still_while_the_window_is_active(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(running(), cx);
            activate(cx);
            assert_eq!(redraws_in_one_second(&harness.view, cx), 0);
        }
    }
}
