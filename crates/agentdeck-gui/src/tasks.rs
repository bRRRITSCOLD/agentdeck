//! The Tasks view (spec §11), Penpot "Screens" → "Tasks": every task, grouped by status, each
//! row with a status picker. Done tasks are hidden until "Show done" is on; the filter keeps
//! rows whose title, PR or workspace contains it. A group header folds its group.
//!
//! It shows the sidebar's [`Snapshot`], which the window passes on as it changes, so it reads
//! nothing itself. A status picked is sent at once; the daemon's `changed` brings it back.

use std::collections::HashSet;
use std::sync::Arc;

use agentdeck_core::{TaskId, TaskStatus};
use brrritscold_core_gpui::{ActiveTheme as _, Input, InputState, Label, Switch};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
    px,
};
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::overview::Snapshot;
use crate::task_list::{self, Group, Query};
use crate::task_row;
use crate::workspace::failed;

/// The filter's width. Raw in Penpot too.
const FILTER_WIDTH: f32 = 240.;

/// The Tasks view.
pub struct TasksView {
    api: Arc<dyn Api>,
    snapshot: Snapshot,
    filter: Entity<InputState>,
    show_done: bool,
    /// The statuses whose group is folded.
    collapsed: HashSet<TaskStatus>,
    _filter: Subscription,
}

impl TasksView {
    /// A view that changes statuses through `api`; it shows nothing until given a snapshot.
    pub fn new(api: Arc<dyn Api>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter tasks…"));
        let _filter = cx.subscribe(&filter, |_, _, event, cx| {
            if let InputEvent::Change = event {
                cx.notify();
            }
        });
        TasksView {
            api,
            snapshot: Snapshot::default(),
            filter,
            show_done: false,
            collapsed: HashSet::new(),
            _filter,
        }
    }

    /// Shows `snapshot`.
    pub fn set_snapshot(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.snapshot = snapshot;
        cx.notify();
    }

    /// Shows or hides done tasks.
    pub fn set_show_done(&mut self, show: bool, cx: &mut Context<Self>) {
        self.show_done = show;
        cx.notify();
    }

    /// Folds `status`'s group, or unfolds it.
    pub fn toggle(&mut self, status: TaskStatus, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&status) {
            self.collapsed.insert(status);
        }
        cx.notify();
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
                let _ =
                    cx.update(|window, cx| failed("Couldn't change the status", &err, window, cx));
            }
        })
        .detach();
    }

    /// The groups shown, with their rows; a folded group has none.
    pub fn groups(&self, cx: &gpui::App) -> Vec<Group> {
        let filter = self.filter.read(cx).value();
        let query = Query {
            filter: &filter,
            show_done: self.show_done,
        };
        let mut groups = task_list::groups(&self.snapshot, &query);
        for group in &mut groups {
            if self.collapsed.contains(&group.status) {
                group.rows.clear();
            }
        }
        groups
    }

    /// The count each group header shows: its rows before folding.
    fn counts(&self, cx: &gpui::App) -> Vec<(TaskStatus, usize)> {
        let filter = self.filter.read(cx).value();
        let query = Query {
            filter: &filter,
            show_done: self.show_done,
        };
        (task_list::groups(&self.snapshot, &query).iter())
            .map(|g| (g.status, g.rows.len()))
            .collect()
    }

    /// Penpot `Header`: heading, spacer, filter, "Show done", `space.3` apart.
    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let show_done = cx.listener(|this, checked: &bool, _, cx| this.set_show_done(*checked, cx));
        let description = if self.show_done {
            "Every task across workspaces, grouped by status."
        } else {
            "Every task across workspaces, grouped by status. Done is hidden."
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
                            .child("Tasks"),
                    )
                    .child(
                        div()
                            .text_size(font_size::SM)
                            .text_color(s.muted_foreground)
                            .child(description),
                    ),
            )
            .child(div().flex_1())
            .child(div().w(px(FILTER_WIDTH)).child(Input::new(&self.filter)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::S2)
                    .child(
                        Switch::new("show-done")
                            .checked(self.show_done)
                            .accessibility_label("Show done")
                            .on_change(move |checked, window, cx| show_done(&checked, window, cx)),
                    )
                    .child(Label::new().child("Show done")),
            )
    }
}

impl Render for TasksView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let counts = self.counts(cx);
        let mut list = div()
            .id("task-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(space::S0_5)
            .overflow_y_scroll();
        for group in self.groups(cx) {
            let status = group.status;
            let count = (counts.iter())
                .find(|(s, _)| *s == status)
                .map_or(0, |(_, n)| *n);
            list = list.child(task_row::group_header(
                status,
                count,
                self.collapsed.contains(&status),
                cx.listener(move |this, _, _, cx| this.toggle(status, cx)),
                cx,
            ));
            for row in &group.rows {
                let task = row.id;
                let picker = task_row::status_picker(
                    SharedString::from(format!("status-{task}")),
                    row.status,
                    cx.listener(move |this, status: &TaskStatus, window, cx| {
                        this.set_status(task, *status, window, cx);
                    }),
                );
                list = list.child(task_row::wide(row, picker, cx));
            }
        }
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
            .child(task_row::columns(cx))
            .child(list)
            .when(counts.is_empty(), |content| {
                content.child(
                    div()
                        .text_size(font_size::SM)
                        .text_color(s.muted_foreground)
                        .px(space::S2)
                        .child("No tasks to show."),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use agentdeck_client::types::{PullRequest, Task};
    use agentdeck_core::{
        Agent, AgentState, ChecksState, PrState, ReviewState, TaskKind, WorkspaceId,
    };
    use brrritscold_core_gpui::{Root, Theme, ThemePreference};
    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::overview::PaneState;
    use crate::overview::fixtures::{id, pane, task, workspace};

    struct Harness {
        api: Arc<FakeApi>,
        view: Entity<TasksView>,
    }

    impl Harness {
        fn open(snapshot: Snapshot, cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            let api = Arc::new(FakeApi::with(snapshot.clone()));
            let mut view = None;
            let window = cx.add_window(|window, cx| {
                let api: Arc<dyn Api> = api.clone();
                let tasks = cx.new(|cx| {
                    let mut tasks = TasksView::new(api, window, cx);
                    tasks.snapshot = snapshot;
                    tasks
                });
                view = Some(tasks.clone());
                Root::new(tasks, window, cx)
            });
            let cx = VisualTestContext::from_window(*window, cx).into_mut();
            cx.run_until_parked();
            let harness = Harness {
                api,
                view: view.unwrap(),
            };
            (harness, cx)
        }

        /// The rows shown, by group.
        fn shown(&self, cx: &mut VisualTestContext) -> Vec<(TaskStatus, Vec<TaskId>)> {
            self.view.read_with(cx, |view, cx| {
                (view.groups(cx).into_iter())
                    .map(|g| (g.status, g.rows.into_iter().map(|r| r.id).collect()))
                    .collect()
            })
        }

        fn draw(&self, cx: &mut VisualTestContext) {
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        fn click(&self, selector: &'static str, cx: &mut VisualTestContext) {
            self.draw(cx);
            let bounds = cx.debug_bounds(selector).expect(selector);
            cx.simulate_click(bounds.center(), Modifiers::default());
            cx.run_until_parked();
        }
    }

    fn with_status(n: u32, status: TaskStatus, ws: Option<WorkspaceId>) -> Task {
        Task {
            status,
            ..task(n, TaskKind::Plain, ws)
        }
    }

    /// Workspace 1 with a doing task an agent works on, a PR in review, a todo in the inbox
    /// and a done task.
    fn board() -> Snapshot {
        let ws = workspace(1, "tabs-a11y");
        let pr = PullRequest {
            checks: Some(ChecksState::Pending),
            draft: true,
            head_ref: "feat/x".into(),
            number: 142,
            repo: "bRRRITSCOLD/ts-packages".into(),
            review: Some(ReviewState::Approved),
            state: PrState::Open,
            synced_at: "2026-11-13T00:00:00Z".into(),
            updated_at: "2026-11-13T00:00:00Z".into(),
            url: "https://github.com/bRRRITSCOLD/ts-packages/pull/142".into(),
        };
        Snapshot {
            tasks: vec![
                with_status(1, TaskStatus::Doing, Some(ws.id)),
                Task {
                    pr: Some(pr),
                    status: TaskStatus::Review,
                    ..task(2, TaskKind::Pr, Some(ws.id))
                },
                with_status(3, TaskStatus::Todo, None),
                with_status(4, TaskStatus::Done, None),
            ],
            panes: vec![PaneState {
                agent: Some(Agent::Claude),
                task: Some(id(1)),
                ..pane(10, ws.id, AgentState::Blocked)
            }],
            workspaces: vec![ws],
            ..Snapshot::default()
        }
    }

    #[gpui::test]
    fn should_group_every_task_but_the_done_ones(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(board(), cx);

        assert_eq!(
            harness.shown(cx),
            [
                (TaskStatus::Doing, vec![id(1)]),
                (TaskStatus::Review, vec![id(2)]),
                (TaskStatus::Todo, vec![id(3)]),
            ]
        );
    }

    #[gpui::test]
    fn should_draw_in_both_themes(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(board(), cx);
        harness
            .view
            .update(cx, |view, cx| view.set_show_done(true, cx));

        for preference in [ThemePreference::Light, ThemePreference::Dark] {
            cx.update(|_, cx| Theme::set_preference(preference, cx));
            cx.run_until_parked();
            harness.draw(cx);
        }
    }

    #[gpui::test]
    fn should_show_done_tasks_once_asked(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(board(), cx);

        harness
            .view
            .update(cx, |view, cx| view.set_show_done(true, cx));

        assert_eq!(
            harness.shown(cx).last(),
            Some(&(TaskStatus::Done, vec![id(4)]))
        );
    }

    #[gpui::test]
    fn should_fold_a_group_when_its_header_is_clicked(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(board(), cx);

        harness.click("group review", cx);
        assert_eq!(harness.shown(cx)[1], (TaskStatus::Review, vec![]));

        harness.click("group review", cx);
        assert_eq!(harness.shown(cx)[1], (TaskStatus::Review, vec![id(2)]));
    }

    #[gpui::test]
    fn should_keep_rows_matching_the_filter(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(board(), cx);
        cx.update(|window, cx| {
            let filter = harness.view.read(cx).filter.clone();
            filter.update(cx, |filter, cx| filter.set_value("ts-packages", window, cx));
        });

        assert_eq!(harness.shown(cx), [(TaskStatus::Review, vec![id(2)])]);
    }

    #[gpui::test]
    fn should_send_a_picked_status(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(board(), cx);

        cx.update(|window, cx| {
            harness.view.update(cx, |view, cx| {
                view.set_status(id(3), TaskStatus::Doing, window, cx);
            });
        });
        cx.run_until_parked();

        let snapshot = cx.update(|_, cx| harness.api.snapshot(cx));
        let status = cx
            .foreground_executor()
            .block_test(snapshot)
            .unwrap()
            .tasks
            .iter()
            .find(|t| t.id == id(3))
            .map(|t| t.status);
        assert_eq!(status, Some(TaskStatus::Doing));
    }

    #[gpui::test]
    fn should_keep_going_when_a_status_cant_be_sent(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(board(), cx);
        harness.api.fail_next("refused");

        cx.update(|window, cx| {
            harness.view.update(cx, |view, cx| {
                view.set_status(id(3), TaskStatus::Doing, window, cx);
            });
        });
        cx.run_until_parked();

        harness.draw(cx);
    }
}
