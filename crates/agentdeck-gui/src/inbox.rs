//! The Inbox (spec §11), Penpot "Screens" → "Inbox": tasks with no workspace, mostly PRs,
//! each with a status picker, and the PR sync's state with a "Sync now" button. Done tasks
//! are left out. While gh is logged out, an alert over the columns says how to log in. Under
//! the heading, once the PR sync status is read, a line says which PRs the age cutoff leaves
//! out.
//!
//! Like the Tasks view it shows the sidebar's [`Snapshot`], passed on by the window. A sync
//! asked for here emits [`Synced`] once done, so the window can have the sidebar read again:
//! a sync that changes no task broadcasts nothing, yet moves the time it last ran.
//!
//! Its list draws only the rows in view: an inbox can hold hundreds of PRs.

use std::sync::Arc;

use agentdeck_client::types::GhLogin;
use agentdeck_core::{TaskId, TaskStatus};
use brrritscold_core_gpui::{
    ActiveTheme as _, Alert, AlertDescription, AlertTitle, Button, ButtonVariant, IconName,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, EventEmitter, InteractiveElement as _, IntoElement, ListState,
    ParentElement as _, Pixels, Render, SharedString, Styled as _, Window, div, list,
};

use crate::api::Api;
use crate::overview::{PrSync, Snapshot};
use crate::task_list::{self, Query, Row, Scope};
use crate::task_row::{self, Layout, Table};
use crate::task_sheet::OpenTask;
use crate::workspace::failed;

/// The description's first line.
const ABOUT: &str =
    "Tasks with no workspace. A PR lands here when no workspace has a clone of its repo.";

/// Which open PRs sync, for a cutoff of `days` (`0` for none). "Settings" is plain text
/// until the Settings screen exists.
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
}

impl EventEmitter<Synced> for InboxView {}
impl EventEmitter<OpenTask> for InboxView {}

impl InboxView {
    /// A view that syncs and changes statuses through `api`; it shows nothing until given a
    /// snapshot.
    pub fn new(api: Arc<dyn Api>) -> Self {
        InboxView {
            api,
            snapshot: Snapshot::default(),
            syncing: false,
            shown: Vec::new(),
            list: task_row::list_state(),
            layout: Table::Inbox.widest(),
        }
    }

    /// Fits the columns to the table's `width`, as drawn. A row's height follows its title's
    /// width, so a change measures the rows again. It runs while the frame is drawn, which
    /// ignores a notify, so the redraw is asked for once the frame is done.
    fn fit(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        let layout = Table::Inbox.fit(width);
        if layout != self.layout {
            self.layout = layout;
            self.list.remeasure();
            cx.defer_in(window, |_, _, cx| cx.notify());
        }
    }

    /// Shows `snapshot`.
    pub fn set_snapshot(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
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
        task_row::spaced(task_row::wide(row, picker, self.layout, open, cx))
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

    /// Penpot `Header`: heading, spacer, when PRs last synced, "Sync now".
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
                                div()
                                    .debug_selector(move || format!("inbox description {ix}"))
                                    .text_size(font_size::SM)
                                    .text_color(s.muted_foreground)
                                    .child(line)
                            }),
                    ),
            )
            .child(div().flex_1())
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

impl Render for InboxView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let rows = self.rows();
        if rows != self.shown {
            task_row::relist(&self.list, rows.len());
            self.shown = rows;
        }
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
            self.layout,
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
            // Penpot "Inbox · gh logged out": a default `Alert` over the columns.
            .when(
                (self.snapshot.pr_sync.as_ref()).is_some_and(|s| s.login == GhLogin::LoggedOut),
                |content| {
                    content.child(
                        div()
                            .flex_none()
                            .debug_selector(|| "gh banner".into())
                            .child(
                            Alert::new()
                                .icon(IconName::Info)
                                .title(AlertTitle::new("GitHub isn't connected"))
                                .description(AlertDescription::new(
                                    "PR sync uses the gh login. Run gh auth login in a terminal, \
                                 then press Sync now.",
                                )),
                        ),
                    )
                },
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
        AppContext as _, Entity, Modifiers, ScrollDelta, ScrollEvent, TestAppContext,
        VisualTestContext, point, px, size,
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
                let inbox = cx.new(|_| InboxView {
                    snapshot,
                    ..InboxView::new(api)
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

    /// [`inbox`] with gh logged out.
    fn logged_out() -> Snapshot {
        let mut snapshot = inbox();
        pr_sync(&mut snapshot).login = agentdeck_client::types::GhLogin::LoggedOut;
        pr_sync(&mut snapshot).last_error = Some("gh is not logged in".into());
        snapshot
    }

    #[gpui::test]
    fn should_show_the_gh_banner_while_gh_is_logged_out(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(logged_out(), cx);
        harness.draw(cx);

        assert!(cx.debug_bounds("gh banner").is_some());
        let label = cx.update(|_, cx| harness.view.read(cx).sync_label(chrono::Utc::now()));
        assert_eq!(label, "gh logged out");
    }

    #[gpui::test]
    fn should_hide_the_gh_banner_once_gh_is_logged_in(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(logged_out(), cx);
        harness.draw(cx);

        cx.update(|_, cx| {
            harness
                .view
                .update(cx, |view, cx| view.set_snapshot(inbox(), cx))
        });
        harness.draw(cx);

        assert!(cx.debug_bounds("gh banner").is_none());
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

            assert!(cx.debug_bounds("gh banner").is_some());
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
            assert!(thumb(list, cx).is_some());
        }

        #[gpui::test]
        fn should_hide_the_scrollbar_when_everything_fits(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(inbox(), cx);
            harness.draw(cx);

            let list = list(cx);
            assert_eq!(thumb(list, cx), None);
        }

        #[gpui::test]
        fn should_move_the_thumb_when_the_list_scrolls(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(crowded(300), cx);
            harness.draw(cx);
            let list = list(cx);
            let before = thumb(list, cx).expect("a thumb");

            cx.simulate_event(ScrollEvent {
                position: list.center(),
                delta: ScrollDelta::Pixels(point(px(0.), px(-2000.))),
                ..ScrollEvent::default()
            });
            harness.draw(cx);

            let after = thumb(list, cx).expect("a thumb");
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

            let shown = thumb(table, cx).expect("a thumb in view");
            let list = list(cx);
            assert!(shown.top() >= list.top() && shown.bottom() <= list.bottom());
            cx.simulate_event(ScrollEvent {
                position: table.center(),
                delta: ScrollDelta::Pixels(point(px(-200.), px(0.))),
                ..ScrollEvent::default()
            });
            harness.draw(cx);
            assert!(thumb(table, cx).is_some(), "scrolled sideways");
        }
    }
}
