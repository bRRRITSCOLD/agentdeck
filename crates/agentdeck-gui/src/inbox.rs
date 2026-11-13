//! The Inbox (spec §11), Penpot "Screens" → "Inbox": tasks with no workspace, mostly PRs,
//! each with a status picker, and the PR sync's state with a "Sync now" button. Done tasks
//! are left out.
//!
//! Like the Tasks view it shows the sidebar's [`Snapshot`], passed on by the window. A sync
//! asked for here emits [`Synced`] once done, so the window can have the sidebar read again:
//! a sync that changes no task broadcasts nothing, yet moves the time it last ran.

use std::sync::Arc;

use agentdeck_core::{TaskId, TaskStatus};
use brrritscold_core_gpui::{ActiveTheme as _, Button, ButtonVariant};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use crate::api::Api;
use crate::overview::Snapshot;
use crate::task_list::{self, Query, Row, Scope};
use crate::task_row::{self, Table};
use crate::workspace::failed;

/// A sync asked for in the Inbox has finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Synced;

/// The Inbox view.
pub struct InboxView {
    api: Arc<dyn Api>,
    snapshot: Snapshot,
    /// Whether a sync asked for here is still running.
    syncing: bool,
}

impl EventEmitter<Synced> for InboxView {}

impl InboxView {
    /// A view that syncs and changes statuses through `api`; it shows nothing until given a
    /// snapshot.
    pub fn new(api: Arc<dyn Api>) -> Self {
        InboxView {
            api,
            snapshot: Snapshot::default(),
            syncing: false,
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

    /// Penpot `Header`: heading, spacer, when PRs last synced, "Sync now".
    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let synced = if self.syncing {
            "Syncing…".to_owned()
        } else {
            (self.snapshot.overview().pr_sync).short_label(chrono::Utc::now())
        };
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
                    .child(
                        div()
                            .text_size(font_size::SM)
                            .text_color(s.muted_foreground)
                            .child(
                                "Tasks with no workspace. A PR lands here when no workspace \
                                 has a clone of its repo.",
                            ),
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
        let mut list = div()
            .id("inbox-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(space::S0_5)
            .overflow_y_scroll();
        for row in &rows {
            let task = row.id;
            let picker = task_row::status_picker(
                SharedString::from(format!("inbox-status-{task}")),
                row.status,
                cx.listener(move |this, status: &TaskStatus, window, cx| {
                    this.set_status(task, *status, window, cx);
                }),
            );
            list = list.child(task_row::wide(row, picker, Table::Inbox, cx));
        }
        // Columns over List, never narrower than the table; it scrolls sideways instead.
        let table = div()
            .id("inbox-table")
            .flex_1()
            .min_h_0()
            .overflow_x_scroll()
            .child(
                div()
                    .size_full()
                    .min_w(Table::Inbox.min_width())
                    .flex()
                    .flex_col()
                    .gap(space::S4)
                    .child(task_row::columns(Table::Inbox, cx))
                    .child(list),
            );
        // Penpot `Content`: Header / Columns / List, `space.4` apart, padded `space.6`.
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(space::S4)
            .p(space::S6)
            .bg(s.background)
            .child(self.header(cx))
            .child(table)
            .when(rows.is_empty(), |content| {
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
    use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::overview::fixtures::{id, task, workspace};

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
    fn should_report_a_failed_sync_without_saying_synced(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(inbox(), cx);
        harness.api.fail_next("gh is logged out");

        cx.update(|window, cx| harness.view.update(cx, |view, cx| view.sync(window, cx)));
        cx.run_until_parked();

        assert_eq!(*harness.synced.borrow(), 0);
        harness.draw(cx);
    }
}
