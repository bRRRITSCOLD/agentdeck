//! The right panel (spec §11), Penpot "Main window" `RightPanel`. The top bar's panel button
//! toggles it, through [`ToggleRightPanel`](crate::keymap::ToggleRightPanel), which the
//! window handles.
//!
//! It lists the workspace's tasks and PRs, grouped by status, each with a status picker. Done tasks hide behind "Show N done". The Tasks & PRs `+` asks the window for the
//! new task dialog on the workspace ([`NewTask`]). **Select**, before it, turns on Select mode
//! for closing PRs ([`ClosePrs`]), with the selection bar pinned to the panel's bottom.
//!
//! Like the Tasks view it shows the sidebar's [`Snapshot`], passed on by the window.
//!
//! All of it is one GPUI [`list`] of `Item`s, which draws only the items in view: with 70 PRs,
//! drawing every row made each frame rebuild every row's status picker. The window draws it as
//! a cached view, so a frame drawn for a change elsewhere reuses it.

use std::collections::HashSet;
use std::sync::Arc;

use agentdeck_core::{TaskId, TaskStatus, WorkspaceId};
use brrritscold_core_gpui::{ActiveTheme as _, Button, ButtonSize, ButtonVariant, IconName};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, ListState, ParentElement as _, Render, SharedString, Styled as _, Subscription,
    Window, div, list, px,
};

use crate::api::Api;
use crate::close_prs::{self, ClosePrs};
use crate::new_task::NewTask;
use crate::overview::Snapshot;
use crate::scrollbar;
use crate::task_list::{self, Group, Query, Row, Scope};
use crate::task_row;
use crate::task_sheet::{EditTask, OpenTask};
use crate::workspace::failed;

/// The panel's width. Raw in Penpot too: no sizing token is 340px.
pub const WIDTH: f32 = 340.;

/// The right panel.
pub struct RightPanel {
    api: Arc<dyn Api>,
    snapshot: Snapshot,
    workspace: Option<WorkspaceId>,
    show_done: bool,
    /// The statuses whose group is folded; Todo starts folded, as Penpot draws it.
    collapsed: HashSet<TaskStatus>,
    /// The list's items, as last rendered.
    shown: Vec<Item>,
    /// The list: where it is scrolled to, and its items' heights.
    list: ListState,
    /// Select mode, the selection and the confirm dialog.
    close: Entity<ClosePrs>,
    _close: Subscription,
    /// How many list items it has rendered, so a test can tell which items a frame drew.
    #[cfg(test)]
    pub(crate) items_rendered: std::cell::Cell<usize>,
}

impl EventEmitter<NewTask> for RightPanel {}
impl EventEmitter<OpenTask> for RightPanel {}
impl EventEmitter<EditTask> for RightPanel {}

impl RightPanel {
    /// A panel that changes statuses through `api`; it lists nothing until given a
    /// workspace and a snapshot.
    pub fn new(api: Arc<dyn Api>, cx: &mut Context<Self>) -> Self {
        let close = cx.new(|_| ClosePrs::new(api.clone(), "panel"));
        let _close = cx.observe(&close, |_, _, cx| cx.notify());
        RightPanel {
            api,
            snapshot: Snapshot::default(),
            workspace: None,
            show_done: false,
            collapsed: HashSet::from([TaskStatus::Todo]),
            shown: Vec::new(),
            // Every item measured once, then each one that changes, so the scrollbar's thumb is
            // right from the first frame; a redraw renders only the items in view.
            list: task_row::list_state().measure_all(),
            close,
            _close,
            #[cfg(test)]
            items_rendered: std::cell::Cell::new(0),
        }
    }

    /// Select mode, the selection and the confirm dialog.
    #[cfg(test)]
    pub fn close_prs(&self) -> &Entity<ClosePrs> {
        &self.close
    }

    /// Lists `workspace`'s tasks from `snapshot`.
    pub fn show(&mut self, workspace: WorkspaceId, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.workspace = Some(workspace);
        self.close
            .update(cx, |close, cx| close.saw(&snapshot.tasks, cx));
        self.snapshot = snapshot;
        cx.notify();
    }

    /// Folds `status`'s group, or unfolds it.
    pub fn toggle(&mut self, status: TaskStatus, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&status) {
            self.collapsed.insert(status);
        }
        cx.notify();
    }

    /// Shows or hides done tasks: what "Show N done" does.
    pub fn set_show_done(&mut self, show: bool, cx: &mut Context<Self>) {
        self.show_done = show;
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
                let _ = cx.update(|window, cx| {
                    failed("Couldn't change the status", &err, window, cx);
                });
            }
        })
        .detach();
    }

    /// The workspace's groups, done ones only when shown, before folding.
    pub fn groups(&self) -> Vec<Group> {
        let Some(workspace) = self.workspace else {
            return Vec::new();
        };
        let query = Query {
            scope: Scope::Workspace(workspace),
            filter: "",
            show_done: true,
        };
        task_list::groups(&self.snapshot, &query)
            .into_iter()
            .filter(|group| self.show_done || group.status != TaskStatus::Done)
            .collect()
    }

    /// How many of the workspace's tasks are done.
    fn done(&self) -> usize {
        let Some(workspace) = self.workspace else {
            return 0;
        };
        (self.snapshot.tasks.iter())
            .filter(|t| t.workspace_id == Some(workspace) && t.status == TaskStatus::Done)
            .count()
    }

    /// Whether `status`'s group is folded.
    pub fn is_collapsed(&self, status: TaskStatus) -> bool {
        self.collapsed.contains(&status)
    }

    /// The list's items, top to bottom, over `groups`, the groups shown.
    fn items(&self, groups: &[Group], cx: &App) -> Vec<Item> {
        let mut items = vec![Item::TasksHeader];
        let close = self.close.read(cx);
        let selecting = close.active();
        for group in groups {
            let collapsed = self.is_collapsed(group.status);
            items.push(Item::Group {
                status: group.status,
                count: group.rows.len(),
                collapsed,
                selectable: close_prs::selectable(&group.rows),
                selecting,
            });
            if !collapsed {
                items.extend(group.rows.iter().map(|row| Item::Task {
                    row: Box::new(row.clone()),
                    selecting,
                    error: close.error(row.id).cloned(),
                }));
            }
        }
        let done = self.done();
        if done > 0 {
            items.push(Item::ShowDone {
                done,
                shown: self.show_done,
            });
        }
        items
    }

    /// Makes the list hold `items`, keeping its place. Only the items that changed are
    /// measured again.
    fn relist(&mut self, items: Vec<Item>) {
        let (old, new) = (self.shown.len(), items.len());
        if old == 0 {
            // An empty list keeps its top at item 0, which a splice at 0 would push past the
            // items it adds.
            self.list.reset(new);
        } else {
            if new > old {
                self.list.splice(old..old, new - old);
            } else if new < old {
                self.list.splice(new..old, 0);
            }
            let changed = |ix: &usize| self.shown[*ix] != items[*ix];
            let first = (0..old.min(new)).find(changed);
            let last = (0..old.min(new)).rfind(changed);
            if let (Some(first), Some(last)) = (first, last) {
                self.list.remeasure_items(first..last + 1);
            }
        }
        self.shown = items;
    }

    /// The list's `ix`th item, `space.1` from the next: a list has no gap of its own.
    fn item(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(test)]
        self.items_rendered.set(self.items_rendered.get() + 1);
        let Some(item) = self.shown.get(ix).cloned() else {
            return div().into_any_element();
        };
        let s = cx.theme().semantic();
        let element = match item {
            // The Tasks & PRs header with its `+`, then the groups.
            Item::TasksHeader => section_header("Tasks & PRs", s)
                .debug_selector(|| "tasks header".into())
                .child(close_prs::select_toggle(&self.close, true, cx))
                .child(
                    div().debug_selector(|| "add task".into()).child(
                        Button::new("add-task")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::IconXs)
                            .icon(IconName::Plus)
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.emit(NewTask(this.workspace));
                            })),
                    ),
                )
                .into_any_element(),
            Item::Group {
                status,
                count,
                collapsed,
                selectable,
                ..
            } => {
                let select = close_prs::select_all(&self.close, status.as_str(), selectable, cx);
                task_row::group_header(
                    status.to_string(),
                    task_row::status_label(status),
                    count,
                    collapsed,
                    select,
                    cx.listener(move |this, _, _, cx| this.toggle(status, cx)),
                    cx,
                )
            }
            Item::Task { row, .. } => {
                let task = row.id;
                let picker = task_row::status_picker(
                    SharedString::from(format!("panel-status-{task}")),
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
                let parts = close_prs::row_parts(&self.close, &row, edit, cx);
                task_row::panel(&row, picker, parts, open, cx)
            }
            // Penpot `ShowDone`: a link button at xs, at the start of its row, after the
            // groups.
            Item::ShowDone { done, shown } => {
                let label = if shown {
                    "Hide done".to_owned()
                } else {
                    format!("Show {done} done")
                };
                div()
                    .flex()
                    .flex_none()
                    .px(space::S2)
                    .child(
                        div().debug_selector(|| "show done".into()).child(
                            Button::new("panel-show-done")
                                .variant(ButtonVariant::Link)
                                .size(ButtonSize::Xs)
                                .label(label)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_show_done(!shown, cx);
                                })),
                        ),
                    )
                    .into_any_element()
            }
        };
        // As wide as the list, as the rows of a column are: a list lays each item out alone.
        div()
            .w_full()
            .flex()
            .flex_col()
            .when(ix + 1 < self.shown.len(), |item| item.pb(space::S1))
            .child(element)
            .into_any_element()
    }
}

/// One item of the panel's list (Penpot `RightPanel`), holding what its height depends on:
/// the list measures an item again when it changes.
#[derive(Debug, Clone, PartialEq)]
enum Item {
    /// Penpot "Tasks & PRs header", with Select and its `+`.
    TasksHeader,
    /// A status group's header.
    Group {
        status: TaskStatus,
        count: usize,
        collapsed: bool,
        /// Its rows with an open PR, which its checkbox picks.
        selectable: Vec<TaskId>,
        /// Select mode is on.
        selecting: bool,
    },
    /// A task's row.
    Task {
        row: Box<Row>,
        /// Select mode is on.
        selecting: bool,
        /// Why closing its PR failed, under its title.
        error: Option<SharedString>,
    },
    /// Penpot `ShowDone`: Show N done, or Hide done while `shown`.
    ShowDone { done: usize, shown: bool },
}

impl Render for RightPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let groups = self.groups();
        let rows: Vec<_> = groups.iter().flat_map(|g| g.rows.clone()).collect();
        let shown = close_prs::selectable(&rows);
        self.close.update(cx, |close, cx| close.retain(&shown, cx));
        let bar = close_prs::selection_bar(
            &self.close,
            close_prs::picked(&self.close, &rows, cx),
            None,
            cx,
        );
        let items = self.items(&groups, cx);
        if items != self.shown {
            self.relist(items);
        }
        // Only the items in view are drawn. The list reaches the panel's right edge, over its
        // `space.2` padding, and pads its content back in by the scrollbar's gutter (Penpot's
        // `space.3` on the right): the scrollbar sits in it, clear of the rows and their
        // buttons.
        let list = div()
            .debug_selector(|| "panel list".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .mr(-space::S2)
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, ix, _, cx| this.item(ix, cx)),
                )
                .flex_1()
                .min_h_0()
                .pr(scrollbar::GUTTER),
            );
        // Penpot `RightPanel`: `card`, padded `space.3` down and `space.2` across, rows
        // `space.1` apart; in Select mode the selection bar sits on its bottom edge.
        let selecting = bar.is_some();
        div()
            .flex_none()
            .w(px(WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .pt(space::S3)
            .when(!selecting, |panel| panel.pb(space::S3))
            .px(space::S2)
            .bg(s.card)
            .child(list)
            .child(scrollbar::vertical(&self.list, cx))
            .children(bar)
            // The confirm dialog's host: out of the flow.
            .child(div().absolute().child(self.close.clone()))
    }
}

/// Penpot's "Tasks & PRs header": sm medium, 32px, padded
/// `space.1` / `space.2`, its children after a spacer.
fn section_header(title: &'static str, s: &brrritscold_core_gpui_tokens::Semantic) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(space::S2)
        .h(space::S8)
        .py(space::S1)
        .px(space::S2)
        .text_size(font_size::SM)
        .font_weight(font_weight::MEDIUM)
        .text_color(s.foreground)
        .child(title)
        .child(div().flex_1())
}

#[cfg(test)]
mod tests {
    use agentdeck_client::types::Task;
    use agentdeck_core::{AgentState, TaskKind};
    use brrritscold_core_gpui::{Root, Theme, ThemePreference};
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::new_task::NewTask;
    use crate::overview::PaneState;
    use crate::overview::fixtures::{id, pane, task, workspace};

    fn with_status(n: u32, status: TaskStatus, ws: WorkspaceId) -> Task {
        Task {
            status,
            ..task(n, TaskKind::Plain, Some(ws))
        }
    }

    /// Workspace 1 with a doing, a todo and a done task; workspace 2 with one of its own.
    fn snapshot() -> Snapshot {
        let (a, b) = (workspace(1, "a"), workspace(2, "b"));
        Snapshot {
            tasks: vec![
                with_status(1, TaskStatus::Doing, a.id),
                with_status(2, TaskStatus::Todo, a.id),
                with_status(3, TaskStatus::Done, a.id),
                with_status(4, TaskStatus::Doing, b.id),
            ],
            workspaces: vec![a, b],
            ..Snapshot::default()
        }
    }

    fn open(cx: &mut TestAppContext) -> (Entity<RightPanel>, &mut VisualTestContext) {
        let (panel, _, cx) = open_with_api(cx);
        (panel, cx)
    }

    fn open_with_api(
        cx: &mut TestAppContext,
    ) -> (Entity<RightPanel>, Arc<FakeApi>, &mut VisualTestContext) {
        cx.update(brrritscold_core_gpui::init);
        cx.update(crate::keymap::init);
        let fake = Arc::new(FakeApi::default());
        let api: Arc<dyn Api> = fake.clone();
        let mut panel = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = RightPanel::new(api, cx);
                view.show(id(1), snapshot(), cx);
                view
            });
            panel = Some(view.clone());
            Root::new(view, window, cx)
        });
        let cx = VisualTestContext::from_window(*window, cx).into_mut();
        cx.run_until_parked();
        (panel.unwrap(), fake, cx)
    }

    fn statuses(panel: &Entity<RightPanel>, cx: &mut VisualTestContext) -> Vec<TaskStatus> {
        panel.read_with(cx, |p, _| p.groups().iter().map(|g| g.status).collect())
    }

    #[gpui::test]
    fn should_list_the_workspaces_open_tasks_with_todo_folded(cx: &mut TestAppContext) {
        let (panel, cx) = open(cx);

        assert_eq!(statuses(&panel, cx), [TaskStatus::Doing, TaskStatus::Todo]);
        let rows: Vec<TaskId> = panel.read_with(cx, |p, _| {
            (p.groups().iter())
                .flat_map(|g| g.rows.iter().map(|r| r.id))
                .collect()
        });
        assert_eq!(rows, [id(1), id(2)]);
        assert!(panel.read_with(cx, |p, _| p.is_collapsed(TaskStatus::Todo)));
    }

    #[gpui::test]
    fn should_ask_for_the_new_task_dialog_on_its_workspace_from_the_plus(cx: &mut TestAppContext) {
        let (panel, cx) = open(cx);
        let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = asked.clone();
        let _asks = cx.update(|_, cx| {
            cx.subscribe(&panel, move |_, event: &NewTask, _| {
                seen.borrow_mut().push(*event)
            })
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let plus = cx.debug_bounds("add task").expect("the Tasks & PRs +");
        cx.simulate_click(plus.center(), Modifiers::default());
        cx.run_until_parked();

        assert_eq!(*asked.borrow(), [NewTask(Some(id(1)))]);
    }

    #[gpui::test]
    fn should_ask_to_edit_a_task_from_its_rows_menu(cx: &mut TestAppContext) {
        let (panel, cx) = open(cx);
        let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = asked.clone();
        let _asks = cx.update(|_, cx| {
            cx.subscribe(&panel, move |_, event: &EditTask, _| {
                seen.borrow_mut().push(*event)
            })
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let menu = format!("panel menu {}", id::<TaskId>(1));
        let bounds = cx.debug_bounds(menu.leak()).expect("the row's menu shows");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();

        assert_eq!(*asked.borrow(), [EditTask(id(1))]);
    }

    #[gpui::test]
    fn should_copy_a_tasks_id_from_its_rows_menu(cx: &mut TestAppContext) {
        let (_panel, cx) = open(cx);
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let menu = format!("panel menu {}", id::<TaskId>(1));
        let bounds = cx.debug_bounds(menu.leak()).expect("the row's menu shows");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
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
        let (panel, cx) = open(cx);
        let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = asked.clone();
        let _asks = cx.update(|_, cx| {
            cx.subscribe(&panel, move |_, event: &OpenTask, _| {
                seen.borrow_mut().push(*event)
            })
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let title = format!("task title {}", id::<TaskId>(1));
        let bounds = cx.debug_bounds(title.leak()).expect("the title shows");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();

        assert_eq!(*asked.borrow(), [OpenTask(id(1))]);
    }

    #[gpui::test]
    fn should_show_done_tasks_from_the_link(cx: &mut TestAppContext) {
        let (panel, cx) = open(cx);
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let link = cx.debug_bounds("show done").expect("the show done link");
        cx.simulate_click(link.center(), Modifiers::default());
        cx.run_until_parked();

        assert_eq!(statuses(&panel, cx).last(), Some(&TaskStatus::Done));
    }

    #[gpui::test]
    fn should_draw_in_both_themes(cx: &mut TestAppContext) {
        let (panel, cx) = open(cx);
        panel.update(cx, |p, cx| p.toggle(TaskStatus::Todo, cx));

        for preference in [ThemePreference::Light, ThemePreference::Dark] {
            cx.update(|_, cx| Theme::set_preference(preference, cx));
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }

    mod scrollbar {
        use super::*;
        use crate::scrollbar::testing::thumb;

        fn draw(cx: &mut VisualTestContext) {
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        #[gpui::test]
        fn should_show_a_scrollbar_when_the_list_overflows(cx: &mut TestAppContext) {
            let (panel, cx) = open(cx);
            let ws = workspace(1, "a");
            let crowded = Snapshot {
                tasks: (1..=100)
                    .map(|n| with_status(n, TaskStatus::Doing, ws.id))
                    .collect(),
                workspaces: vec![ws],
                ..Snapshot::default()
            };
            panel.update(cx, |view, cx| view.show(id(1), crowded, cx));
            draw(cx);

            let list = cx.debug_bounds("panel list").expect("the list");
            assert!(thumb(list, cx).is_some());
        }

        #[gpui::test]
        fn should_hide_the_scrollbar_when_everything_fits(cx: &mut TestAppContext) {
            let (_panel, cx) = open(cx);
            draw(cx);

            let list = cx.debug_bounds("panel list").expect("the list");
            assert_eq!(thumb(list, cx), None);
        }

        #[gpui::test]
        fn should_keep_the_thumb_clear_of_the_rows_and_their_buttons(cx: &mut TestAppContext) {
            let (panel, cx) = open(cx);
            let ws = workspace(1, "a");
            let crowded = Snapshot {
                tasks: (1..=100)
                    .map(|n| with_status(n, TaskStatus::Doing, ws.id))
                    .collect(),
                workspaces: vec![ws],
                ..Snapshot::default()
            };
            panel.update(cx, |view, cx| view.show(id(1), crowded, cx));
            draw(cx);
            let list = cx.debug_bounds("panel list").expect("the list");
            let thumb = thumb(list, cx).expect("a thumb");

            // The rows in view: the list draws no others.
            let mut parts: Vec<String> = (1..=100)
                .map(|n| format!("panel task {}", id::<TaskId>(n)))
                .filter(|row| cx.debug_bounds(row.clone().leak()).is_some())
                .collect();
            assert!(parts.len() >= 3, "{} rows in view", parts.len());
            parts.extend(["add task", "group doing"].map(String::from));
            for part in parts {
                let bounds = cx.debug_bounds(part.clone().leak()).expect(&part);
                assert!(
                    bounds.right() < thumb.left(),
                    "{part} {bounds:?} reaches {thumb:?}"
                );
            }
        }
    }

    mod closing_prs {
        use agentdeck_core::PrState;

        use super::*;
        use crate::overview::fixtures::pr_task;

        /// Workspace 1 with an open PR 5 and a plain task 6, both in review, and a todo task.
        fn with_prs(panel: &Entity<RightPanel>, cx: &mut VisualTestContext) {
            let mut snapshot = snapshot();
            let ws = Some(id(1));
            snapshot
                .tasks
                .push(pr_task(5, "acme/widgets", PrState::Open, ws));
            snapshot.tasks.push(Task {
                status: TaskStatus::Review,
                ..task(6, TaskKind::Plain, ws)
            });
            panel.update(cx, |panel, cx| panel.show(id(1), snapshot, cx));
        }

        fn click(selector: &'static str, cx: &mut VisualTestContext) {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let bounds = cx.debug_bounds(selector).expect(selector);
            cx.simulate_click(bounds.center(), Modifiers::default());
            cx.run_until_parked();
        }

        #[gpui::test]
        fn should_count_the_picked_prs_closing_from_the_snapshots_it_is_shown(
            cx: &mut TestAppContext,
        ) {
            let (panel, api, cx) = open_with_api(cx);
            with_prs(&panel, cx);
            click("panel select", cx);
            click("panel select all review", cx);
            click("panel close prs", cx);
            let release = api.hold_close();
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
            let closing = |cx: &mut VisualTestContext| {
                panel.read_with(cx, |panel, cx| panel.close_prs().read(cx).closing())
            };
            assert_eq!(closing(cx), Some((0, 1)));

            let mut snapshot = snapshot();
            let mut pr = pr_task(5, "acme/widgets", PrState::Closed, Some(id(1)));
            pr.status = TaskStatus::Done;
            snapshot.tasks.push(pr);
            panel.update(cx, |panel, cx| panel.show(id(1), snapshot, cx));
            cx.update(|window, cx| window.draw(cx).clear(cx));
            assert_eq!(closing(cx), Some((1, 1)));

            release.send(()).unwrap();
            cx.run_until_parked();
            assert_eq!(closing(cx), None);
        }

        #[gpui::test]
        fn should_mark_a_pr_that_did_not_close_and_say_why_on_hover(cx: &mut TestAppContext) {
            let (panel, api, cx) = open_with_api(cx);
            with_prs(&panel, cx);
            api.refuse_close(id(5), "Must have admin rights");
            click("panel select", cx);
            click("panel select all review", cx);
            click("panel close prs", cx);
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));

            let task: TaskId = id(5);
            let mark = cx
                .debug_bounds(format!("close error mark {task}").leak())
                .expect("the failed row's mark");
            cx.simulate_mouse_move(mark.center(), None, Modifiers::default());
            cx.run_until_parked();

            assert!(cx.debug_bounds("tooltip-content").is_some());
        }

        #[gpui::test]
        fn should_pin_the_selection_bar_to_the_panels_bottom(cx: &mut TestAppContext) {
            let (panel, cx) = open(cx);
            with_prs(&panel, cx);

            click("panel select", cx);
            cx.update(|window, cx| window.draw(cx).clear(cx));

            let bar = cx.debug_bounds("panel selection bar").expect("the bar");
            let viewport = cx.update(|window, _| window.viewport_size());
            assert_eq!(bar.bottom(), viewport.height);
        }

        #[gpui::test]
        fn should_check_only_the_groups_and_rows_with_an_open_pr(cx: &mut TestAppContext) {
            let (panel, cx) = open(cx);
            with_prs(&panel, cx);

            click("panel select", cx);

            let task: TaskId = id(5);
            assert!(cx.debug_bounds("panel select all review").is_some());
            assert!(cx.debug_bounds("panel select all doing").is_none());
            assert!(
                cx.debug_bounds(format!("panel select {task}").leak())
                    .is_some()
            );
            let plain: TaskId = id(6);
            assert!(
                cx.debug_bounds(format!("panel select {plain}").leak())
                    .is_none()
            );
            assert!(
                cx.debug_bounds(format!("panel menu {task}").leak())
                    .is_some()
            );
            assert!(
                cx.debug_bounds(format!("panel menu {plain}").leak())
                    .is_some(),
                "every row has a menu, for Edit task"
            );
        }

        #[gpui::test]
        fn should_pick_from_the_group_checkbox_without_folding_the_group(cx: &mut TestAppContext) {
            let (panel, cx) = open(cx);
            with_prs(&panel, cx);
            click("panel select", cx);

            click("panel select all review", cx);

            let picked = panel.read_with(cx, |panel, cx| {
                panel.close_prs().read(cx).selection().ids().to_vec()
            });
            assert_eq!(picked, [id(5)]);
            assert!(!panel.read_with(cx, |panel, _| panel.is_collapsed(TaskStatus::Review)));
        }
    }

    mod long_list {
        use gpui::{ScrollDelta, ScrollEvent, point};

        use super::*;
        use crate::state_dot::tests::redraw;

        /// Workspace 1 with tasks 1 to 100 in Doing, and an agent at work on task 1.
        fn crowded() -> Snapshot {
            let ws = workspace(1, "a");
            Snapshot {
                tasks: (1..=100)
                    .map(|n| with_status(n, TaskStatus::Doing, ws.id))
                    .collect(),
                panes: vec![PaneState {
                    task: Some(id(1)),
                    ..pane(300, ws.id, AgentState::Working)
                }],
                workspaces: vec![ws],
                ..Snapshot::default()
            }
        }

        fn open_crowded(cx: &mut TestAppContext) -> (Entity<RightPanel>, &mut VisualTestContext) {
            let (panel, cx) = open(cx);
            panel.update(cx, |p, cx| p.show(id(1), crowded(), cx));
            settle(cx);
            (panel, cx)
        }

        fn settle(cx: &mut VisualTestContext) {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        fn shows(selector: String, cx: &mut VisualTestContext) -> bool {
            cx.debug_bounds(selector.leak()).is_some()
        }

        fn task_shows(n: u32, cx: &mut VisualTestContext) -> bool {
            shows(format!("panel task {}", id::<TaskId>(n)), cx)
        }

        fn rendered(panel: &Entity<RightPanel>, cx: &mut VisualTestContext) -> usize {
            panel.read_with(cx, |p, _| p.items_rendered.get())
        }

        /// The most items the list's height holds, counting what it draws past either end:
        /// as many as the shortest item, a 32px header, would fill.
        fn in_view(cx: &mut VisualTestContext) -> usize {
            let list = cx.debug_bounds("panel list").expect("the list");
            ((list.size.height + task_row::OVERDRAW * 2.) / space::S8).ceil() as usize + 2
        }

        /// Wheels the list down by `by`.
        fn wheel(by: f32, cx: &mut VisualTestContext) {
            let list = cx.debug_bounds("panel list").expect("the list");
            cx.simulate_event(ScrollEvent {
                position: list.center(),
                delta: ScrollDelta::Pixels(point(px(0.), px(-by))),
                ..ScrollEvent::default()
            });
            settle(cx);
        }

        fn wheel_to_the_end(cx: &mut VisualTestContext) {
            wheel(20_000., cx);
        }

        #[gpui::test]
        fn should_draw_only_the_rows_in_view(cx: &mut TestAppContext) {
            let (_, cx) = open_crowded(cx);

            assert!(task_shows(1, cx));
            assert!(!task_shows(100, cx));
        }

        #[gpui::test]
        fn should_render_only_the_items_in_view_on_a_redraw(cx: &mut TestAppContext) {
            let (panel, cx) = open_crowded(cx);
            let (before, in_view) = (rendered(&panel, cx), in_view(cx));
            assert!(in_view < 100, "the window holds {in_view} items");

            redraw(&panel, 10, cx);

            let per_redraw = (rendered(&panel, cx) - before) / 10;
            assert!(
                per_redraw <= in_view,
                "{per_redraw} items a redraw, {in_view} in view"
            );
        }

        #[gpui::test]
        fn should_stretch_every_item_across_the_list(cx: &mut TestAppContext) {
            let (panel, cx) = open_crowded(cx);
            let mut short = crowded();
            short.tasks[1].title = "x".into();
            panel.update(cx, |p, cx| p.show(id(1), short, cx));
            settle(cx);

            let width = |selector: String, cx: &mut VisualTestContext| {
                cx.debug_bounds(selector.clone().leak())
                    .expect(&selector)
                    .size
                    .width
            };
            let header = width("tasks header".into(), cx);
            let long = width(format!("panel task {}", id::<TaskId>(1)), cx);
            let short = width(format!("panel task {}", id::<TaskId>(2)), cx);
            assert_eq!([long, short], [header; 2]);
        }

        #[gpui::test]
        fn should_draw_the_rows_a_scroll_brings_into_view(cx: &mut TestAppContext) {
            let (_, cx) = open_crowded(cx);

            wheel_to_the_end(cx);

            assert!(task_shows(100, cx));
            assert!(!task_shows(1, cx));
            assert!(!shows("add task".into(), cx));
        }

        #[gpui::test]
        fn should_open_a_row_scrolled_into_view(cx: &mut TestAppContext) {
            let (panel, cx) = open_crowded(cx);
            let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let seen = asked.clone();
            let _asks = cx.update(|_, cx| {
                cx.subscribe(&panel, move |_, event: &OpenTask, _| {
                    seen.borrow_mut().push(*event)
                })
            });
            wheel_to_the_end(cx);

            let title = format!("task title {}", id::<TaskId>(100));
            let bounds = cx.debug_bounds(title.leak()).expect("the title shows");
            cx.simulate_click(bounds.center(), Modifiers::default());
            cx.run_until_parked();

            assert_eq!(*asked.borrow(), [OpenTask(id(100))]);
        }

        #[gpui::test]
        fn should_keep_its_place_when_the_snapshot_changes(cx: &mut TestAppContext) {
            let (panel, cx) = open_crowded(cx);
            wheel_to_the_end(cx);

            let mut changed = crowded();
            changed.tasks[50].title = "renamed".into();
            panel.update(cx, |p, cx| p.show(id(1), changed, cx));
            settle(cx);

            assert!(task_shows(100, cx));
            assert!(!task_shows(1, cx));
        }

        #[gpui::test]
        fn should_reach_the_rows_a_snapshot_adds(cx: &mut TestAppContext) {
            let (panel, cx) = open_crowded(cx);

            let mut more = crowded();
            more.tasks.extend(
                (101..=120).map(|n| with_status(n, TaskStatus::Doing, more.workspaces[0].id)),
            );
            panel.update(cx, |p, cx| p.show(id(1), more, cx));
            settle(cx);
            wheel_to_the_end(cx);

            assert!(task_shows(120, cx));
        }
    }

    mod spin {
        use super::*;
        use crate::state_dot::tests::{activate, redraws_in_one_second};

        /// Opens the panel on workspace 1, whose doing task an agent is working on.
        fn working(cx: &mut TestAppContext) -> (Entity<RightPanel>, &mut VisualTestContext) {
            let (panel, cx) = open(cx);
            panel.update(cx, |p, cx| {
                let mut snapshot = snapshot();
                snapshot.panes.push(PaneState {
                    task: Some(id(1)),
                    ..pane(10, snapshot.workspaces[0].id, AgentState::Working)
                });
                p.show(id(1), snapshot, cx);
            });
            activate(cx);
            (panel, cx)
        }

        #[gpui::test]
        fn should_hold_a_working_agents_mark_still_while_the_window_is_active(
            cx: &mut TestAppContext,
        ) {
            let (panel, cx) = working(cx);
            assert_eq!(redraws_in_one_second(&panel, cx), 0);
        }
    }
}
