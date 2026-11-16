//! The Tasks view (spec §11), Penpot "Screens" → "Tasks": every task, grouped by status, each
//! row with a status picker. Done tasks are hidden until "Show done" is on; the filter keeps
//! rows whose title, PR or workspace contains it. A group header folds its group. "New task"
//! asks the window for the new task dialog on the Inbox ([`NewTask`]).
//!
//! It shows the sidebar's [`Snapshot`], which the window passes on as it changes, so it reads
//! nothing itself. A status picked is sent at once; the daemon's `changed` brings it back.
//!
//! Its list draws only the headers and rows in view: the inbox's PRs alone can be hundreds.
//!
//! **Select**, before Show done, turns on Select mode for closing PRs ([`ClosePrs`]): a
//! checkbox on each open PR's row and on each group header over one, and the selection bar
//! along the bottom.

use std::collections::HashSet;
use std::sync::Arc;

use agentdeck_core::{TaskId, TaskStatus};
use brrritscold_core_gpui::{ActiveTheme as _, Button, Input, InputState, Label, Switch};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, ListState, ParentElement as _, Pixels, Render, SharedString, Styled as _,
    Subscription, Window, div, list, px,
};
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::close_prs::{self, ClosePrs};
use crate::new_task::NewTask;
use crate::overview::Snapshot;
use crate::task_list::{self, Group, Query, Row, Scope};
use crate::task_row::{self, Layout, Table};
use crate::task_sheet::{EditTask, OpenTask};
use crate::workspace::failed;

/// The filter's width, narrow enough for Select to fit beside it at 1200. Raw in Penpot too.
const FILTER_WIDTH: f32 = 128.;

/// One item of the list: a group's header, or a row under it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    Header {
        status: TaskStatus,
        count: usize,
        collapsed: bool,
        /// Its rows whose PR can close, folded or not: what its checkbox picks.
        selectable: Vec<TaskId>,
    },
    Row(Row),
}

/// The Tasks view.
pub struct TasksView {
    api: Arc<dyn Api>,
    snapshot: Snapshot,
    filter: Entity<InputState>,
    show_done: bool,
    /// The statuses whose group is folded.
    collapsed: HashSet<TaskStatus>,
    /// The items the list draws from, as last rendered.
    shown: Vec<Item>,
    list: ListState,
    /// The columns shown, fitted to the width the table was last drawn at.
    layout: Layout,
    /// Select mode, the selection and the confirm dialog.
    close: Entity<ClosePrs>,
    _filter: Subscription,
    _close: Subscription,
}

impl EventEmitter<NewTask> for TasksView {}
impl EventEmitter<OpenTask> for TasksView {}
impl EventEmitter<EditTask> for TasksView {}

impl TasksView {
    /// A view that changes statuses through `api`; it shows nothing until given a snapshot.
    pub fn new(api: Arc<dyn Api>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter tasks…"));
        let _filter = cx.subscribe(&filter, |_, _, event, cx| {
            if let InputEvent::Change = event {
                cx.notify();
            }
        });
        let close = cx.new(|_| ClosePrs::new(api.clone(), "tasks"));
        let _close = cx.observe(&close, |this, _, cx| {
            this.list.remeasure();
            cx.notify();
        });
        TasksView {
            api,
            snapshot: Snapshot::default(),
            filter,
            show_done: false,
            collapsed: HashSet::new(),
            shown: Vec::new(),
            list: task_row::list_state(),
            layout: Table::Tasks.widest(),
            close,
            _filter,
            _close,
        }
    }

    /// Select mode, the selection and the confirm dialog.
    #[cfg(test)]
    pub fn close_prs(&self) -> &Entity<ClosePrs> {
        &self.close
    }

    /// The columns to draw: the fitted ones, in Select mode while it's on.
    fn layout(&self, cx: &gpui::App) -> Layout {
        Layout {
            select: self.close.read(cx).active(),
            ..self.layout
        }
    }

    /// Fits the columns to the table's `width`, as drawn. A row's height follows its title's
    /// width, so a change measures the rows again. It runs while the frame is drawn, which
    /// ignores a notify, so the redraw is asked for once the frame is done.
    fn fit(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        let layout = Table::Tasks.fit(width, self.close.read(cx).active());
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

    /// The groups the filter and Show done leave, each with all its rows, folded or not.
    fn unfolded(&self, cx: &gpui::App) -> Vec<Group> {
        let filter = self.filter.read(cx).value();
        let query = Query {
            scope: Scope::All,
            filter: &filter,
            show_done: self.show_done,
        };
        task_list::groups(&self.snapshot, &query)
    }

    /// The groups shown, with their rows; a folded group has none.
    #[cfg(test)]
    pub fn groups(&self, cx: &gpui::App) -> Vec<Group> {
        let mut groups = self.unfolded(cx);
        for group in &mut groups {
            if self.collapsed.contains(&group.status) {
                group.rows.clear();
            }
        }
        groups
    }

    /// The list's items: each group's header, then its rows unless folded.
    fn items(&self, cx: &gpui::App) -> Vec<Item> {
        let mut items = Vec::new();
        for group in self.unfolded(cx) {
            let status = group.status;
            let collapsed = self.collapsed.contains(&status);
            items.push(Item::Header {
                status,
                count: group.rows.len(),
                collapsed,
                selectable: close_prs::selectable(&group.rows),
            });
            if !collapsed {
                items.extend(group.rows.into_iter().map(Item::Row));
            }
        }
        items
    }

    /// The list's `ix`th item: a group header that folds its group, or a row with its status
    /// picker. Its marks turn only while `spin`: the window is active.
    fn item(&mut self, ix: usize, spin: bool, cx: &mut Context<Self>) -> AnyElement {
        match self.shown.get(ix) {
            Some(Item::Header {
                status,
                count,
                collapsed,
                selectable,
            }) => {
                let status = *status;
                let select =
                    close_prs::select_all(&self.close, status.as_str(), selectable.clone(), cx);
                task_row::spaced(task_row::group_header(
                    status.to_string(),
                    task_row::status_label(status),
                    *count,
                    *collapsed,
                    select,
                    cx.listener(move |this, _, _, cx| this.toggle(status, cx)),
                    cx,
                ))
            }
            Some(Item::Row(row)) => {
                let task = row.id;
                let picker = task_row::status_picker(
                    SharedString::from(format!("status-{task}")),
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
                let parts = close_prs::row_parts(&self.close, row, edit, cx);
                task_row::spaced(task_row::wide(
                    row,
                    picker,
                    self.layout(cx),
                    parts,
                    open,
                    spin,
                    cx,
                ))
            }
            None => div().into_any_element(),
        }
    }

    /// Penpot `Header`: heading, spacer, filter, Select, "Show done", "New task", `space.3`
    /// apart.
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
            .child(close_prs::select_toggle(&self.close, cx))
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
            .child(
                div().debug_selector(|| "new task".into()).child(
                    Button::new("new-task")
                        .label("New task")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(NewTask(None)))),
                ),
            )
    }
}

impl Render for TasksView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let items = self.items(cx);
        if items != self.shown {
            task_row::relist(&self.list, items.len());
            self.shown = items;
            let shown = (self.unfolded(cx).iter())
                .flat_map(|group| close_prs::selectable(&group.rows))
                .collect::<Vec<_>>();
            self.close.update(cx, |close, cx| close.retain(&shown, cx));
        }
        let all_rows: Vec<Row> = (self.unfolded(cx).into_iter())
            .flat_map(|group| group.rows)
            .collect();
        let bar = close_prs::selection_bar(
            &self.close,
            close_prs::picked(&self.close, &all_rows, cx),
            cx,
        );
        let layout = self.layout(cx);
        let rows = div()
            .debug_selector(|| "task list".into())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, ix, window, cx| {
                        this.item(ix, window.is_window_active(), cx)
                    }),
                )
                .flex_1()
                .min_h_0(),
            );
        let table = task_row::table(
            "task-table",
            "task table",
            layout,
            task_row::SelectCell::Off,
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
            .child(table)
            .when(self.shown.is_empty(), |content| {
                content.child(
                    div()
                        .text_size(font_size::SM)
                        .text_color(s.muted_foreground)
                        .px(space::S2)
                        .child("No tasks to show."),
                )
            })
            .children(bar)
            // The confirm dialog's host: out of the flow, so it takes no gap.
            .child(div().absolute().child(self.close.clone()))
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
    use crate::new_task::NewTask;
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

    /// `n` todo tasks.
    fn crowded(n: u32) -> Snapshot {
        Snapshot {
            tasks: (1..=n).map(|n| task(n, TaskKind::Plain, None)).collect(),
            ..Snapshot::default()
        }
    }

    fn drawn(task: u32, cx: &mut VisualTestContext) -> bool {
        (cx.debug_bounds(format!("task {}", id::<TaskId>(task)).leak())).is_some()
    }

    #[gpui::test]
    fn should_ask_for_the_new_task_dialog_on_the_inbox(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(board(), cx);
        let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = asked.clone();
        let _asks = cx.update(|_, cx| {
            cx.subscribe(&harness.view, move |_, event: &NewTask, _| {
                seen.borrow_mut().push(*event)
            })
        });

        harness.click("new task", cx);

        assert_eq!(*asked.borrow(), [NewTask(None)]);
    }

    #[gpui::test]
    fn should_ask_to_edit_a_plain_task_from_its_rows_menu(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(board(), cx);
        let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = asked.clone();
        let _asks = cx.update(|_, cx| {
            cx.subscribe(&harness.view, move |_, event: &EditTask, _| {
                seen.borrow_mut().push(*event)
            })
        });

        harness.click(format!("tasks menu {}", id::<TaskId>(2)).leak(), cx);
        harness.draw(cx);
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();

        assert_eq!(*asked.borrow(), [EditTask(id(2))]);
    }

    #[gpui::test]
    fn should_copy_a_plain_tasks_id_from_its_rows_menu(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(board(), cx);

        harness.click(format!("tasks menu {}", id::<TaskId>(1)).leak(), cx);
        harness.draw(cx);
        cx.simulate_keystrokes("down down enter");
        cx.run_until_parked();

        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied, Some(id::<TaskId>(1).to_string()));
        let shown = cx.update(|_, cx| crate::notice::shown(cx));
        assert_eq!(
            shown.last().map(|toast| toast.title.clone()),
            Some("Copied task id".into())
        );
    }

    #[gpui::test]
    fn should_ask_for_the_task_sheet_when_a_title_is_clicked(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(board(), cx);
        let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = asked.clone();
        let _asks = cx.update(|_, cx| {
            cx.subscribe(&harness.view, move |_, event: &OpenTask, _| {
                seen.borrow_mut().push(*event)
            })
        });

        harness.click(format!("task title {}", id::<TaskId>(2)).leak(), cx);

        assert_eq!(*asked.borrow(), [OpenTask(id(2))]);
    }

    #[gpui::test]
    fn should_mark_a_row_whose_agent_comments_are_unseen_after_its_title(cx: &mut TestAppContext) {
        let mut snapshot = board();
        snapshot.tasks[0].unseen_agent_comments = 1;
        let (harness, cx) = Harness::open(snapshot, cx);
        harness.draw(cx);

        let mark =
            (cx.debug_bounds(format!("unseen {}", id::<TaskId>(1)).leak())).expect("task 1's mark");
        let title = (cx.debug_bounds(format!("task title {}", id::<TaskId>(1)).leak()))
            .expect("task 1's title");
        assert_eq!(mark.size, gpui::size(px(14.), px(14.)));
        assert!(mark.left() >= title.right(), "{mark:?} after {title:?}");
        assert!(
            cx.debug_bounds(format!("unseen {}", id::<TaskId>(2)).leak())
                .is_none()
        );
    }

    #[gpui::test]
    fn should_draw_only_the_rows_in_view(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(crowded(300), cx);
        harness.draw(cx);

        assert!(cx.debug_bounds("group todo").is_some());
        assert!(drawn(1, cx));
        assert!(!drawn(300, cx));
    }

    #[gpui::test]
    fn should_draw_the_rows_after_a_folded_group(cx: &mut TestAppContext) {
        let mut snapshot = crowded(300);
        snapshot
            .tasks
            .push(with_status(301, TaskStatus::Done, None));
        let (harness, cx) = Harness::open(snapshot, cx);
        harness
            .view
            .update(cx, |view, cx| view.set_show_done(true, cx));
        harness.draw(cx);
        assert!(!drawn(301, cx));

        harness.click("group todo", cx);
        harness.draw(cx);

        assert!(drawn(301, cx));
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

    mod columns {
        use gpui::{px, size};

        use super::*;

        /// The view's width in a window `width` wide: the window less the sidebar.
        fn open_in(width: f32, cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            let (harness, cx) = Harness::open(board(), cx);
            cx.simulate_resize(size(px(width - 261.), px(800.)));
            fit(&harness, cx);
            (harness, cx)
        }

        /// Lets the frame a resize draws refit the columns. The redraw that refit asks for
        /// must come by itself, as it has to in the app: nothing here draws.
        fn fit(_: &Harness, cx: &mut VisualTestContext) {
            cx.run_until_parked();
        }

        fn shown(selector: String, cx: &mut VisualTestContext) -> bool {
            cx.debug_bounds(selector.leak()).is_some()
        }

        fn cell(column: &str, task: u32) -> String {
            format!("{column} {}", id::<TaskId>(task))
        }

        #[gpui::test]
        fn should_show_every_column_in_a_wide_window(cx: &mut TestAppContext) {
            let (_, cx) = open_in(1440., cx);

            for column in ["Agent", "Checks", "Review", "Workspace"] {
                assert!(shown(format!("column {column}"), cx), "{column}");
            }
            assert!(shown(cell("agent", 1), cx));
            assert!(shown(cell("checks", 2), cx));
        }

        #[gpui::test]
        fn should_drop_the_agent_column_first_when_the_table_does_not_fit(cx: &mut TestAppContext) {
            let (_, cx) = open_in(1350., cx);

            assert!(!shown("column Agent".into(), cx));
            assert!(!shown(cell("agent", 1), cx));
            assert!(shown("column Checks".into(), cx));
            assert!(shown(cell("checks", 2), cx));
        }

        #[gpui::test]
        fn should_not_scroll_sideways_at_1200(cx: &mut TestAppContext) {
            let (_, cx) = open_in(1200., cx);

            let table = cx.debug_bounds("task table").expect("the table");
            let row = cx.debug_bounds(cell("task", 2).leak()).expect("a row");
            assert!(row.right() <= table.right(), "{row:?} overflows {table:?}");
            for column in ["Agent", "Checks"] {
                assert!(!shown(format!("column {column}"), cx), "{column}");
            }
            assert!(!shown(cell("checks", 2), cx));
        }

        #[gpui::test]
        fn should_keep_review_and_workspace(cx: &mut TestAppContext) {
            let (_, cx) = open_in(1200., cx);

            for (column, cell_name) in [("Review", "review"), ("Workspace", "workspace")] {
                let header = cx.debug_bounds(format!("column {column}").leak());
                let row = cx.debug_bounds(cell(cell_name, 2).leak());
                let header = header.expect(column);
                let row = row.expect(cell_name);
                assert_eq!(header.left(), row.left(), "{column} lines up");
            }
        }

        #[gpui::test]
        fn should_bring_the_columns_back_when_the_window_widens(cx: &mut TestAppContext) {
            let (harness, cx) = open_in(1200., cx);

            cx.simulate_resize(size(px(1440. - 261.), px(800.)));
            fit(&harness, cx);

            assert!(shown("column Agent".into(), cx));
            assert!(shown(cell("agent", 1), cx));
        }
    }

    mod scrollbar {
        use gpui::{ScrollDelta, ScrollEvent, point, px, size};

        use super::*;
        use crate::scrollbar::testing::thumb;

        fn list(cx: &mut VisualTestContext) -> gpui::Bounds<gpui::Pixels> {
            cx.debug_bounds("task list").expect("the list")
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
            let (harness, cx) = Harness::open(board(), cx);
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
            let table = cx.debug_bounds("task table").expect("the table");
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
        use crate::overview::fixtures::pr_task;

        /// Open PRs 1 and 2 and a plain task 3 in review; a doing plain task 4.
        fn prs() -> Snapshot {
            Snapshot {
                tasks: vec![
                    pr_task(1, "acme/widgets", PrState::Open, None),
                    pr_task(2, "acme/gadgets", PrState::Open, None),
                    Task {
                        status: TaskStatus::Review,
                        ..task(3, TaskKind::Plain, None)
                    },
                    Task {
                        status: TaskStatus::Doing,
                        ..task(4, TaskKind::Plain, None)
                    },
                ],
                ..Snapshot::default()
            }
        }

        fn picked(harness: &Harness, cx: &mut VisualTestContext) -> Vec<TaskId> {
            harness.view.read_with(cx, |view, cx| {
                view.close_prs().read(cx).selection().ids().to_vec()
            })
        }

        #[gpui::test]
        fn should_pick_a_groups_open_prs_from_its_header_without_folding_it(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(prs(), cx);
            harness.click("tasks select", cx);

            harness.click("tasks select all review", cx);

            assert_eq!(picked(&harness, cx), [id(1), id(2)]);
            assert_eq!(
                harness.shown(cx)[1],
                (TaskStatus::Review, vec![id(1), id(2), id(3)])
            );
        }

        #[gpui::test]
        fn should_give_a_group_with_no_open_pr_no_checkbox(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            harness.click("tasks select", cx);

            assert!(cx.debug_bounds("tasks select all doing").is_none());
        }

        #[gpui::test]
        fn should_count_the_picked_prs_closing_from_the_snapshots_it_is_shown(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(prs(), cx);
            harness.click("tasks select", cx);
            harness.click("tasks select all review", cx);
            harness.click("tasks close prs", cx);
            let release = harness.api.hold_close();
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
            let closing = |cx: &mut VisualTestContext| {
                harness
                    .view
                    .read_with(cx, |view, cx| view.close_prs().read(cx).closing())
            };
            assert_eq!(closing(cx), Some((0, 2)));

            let mut snapshot = prs();
            snapshot.tasks[0].status = TaskStatus::Done;
            if let Some(pr) = &mut snapshot.tasks[0].pr {
                pr.state = PrState::Closed;
            }
            harness
                .view
                .update(cx, |view, cx| view.set_snapshot(snapshot, cx));
            harness.draw(cx);
            assert_eq!(closing(cx), Some((1, 2)));

            release.send(()).unwrap();
            cx.run_until_parked();
            assert_eq!(closing(cx), None);
        }

        #[gpui::test]
        fn should_put_select_between_the_filter_and_show_done(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            harness.draw(cx);

            let select = cx.debug_bounds("tasks select").expect("Select");
            let new_task = cx.debug_bounds("new task").expect("New task");
            assert!(select.right() < new_task.left());
        }

        #[gpui::test]
        fn should_draw_select_mode_in_both_themes(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            harness.click("tasks select", cx);
            harness.click("tasks select 00000000-0000-7000-8000-000000000001", cx);

            for preference in [ThemePreference::Light, ThemePreference::Dark] {
                cx.update(|_, cx| Theme::set_preference(preference, cx));
                cx.run_until_parked();
                harness.draw(cx);
            }
            assert!(cx.debug_bounds("tasks selection bar").is_some());
        }
    }

    mod spin {
        use super::*;
        use crate::state_dot::tests::{activate, deactivate, redraws_in_one_second};

        #[gpui::test]
        fn should_spin_running_checks_while_the_window_is_active(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(board(), cx);
            activate(cx);
            assert!(redraws_in_one_second(&harness.view, cx) > 0);
        }

        #[gpui::test]
        fn should_hold_them_still_while_the_window_is_inactive(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(board(), cx);
            activate(cx);
            deactivate(cx);
            assert_eq!(redraws_in_one_second(&harness.view, cx), 0);
        }
    }
}
