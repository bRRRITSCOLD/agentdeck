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
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
    div, px,
};
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::overview::Snapshot;
use crate::task_list::{self, Group, Query, Scope};
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
                let _ = cx.update(|window, cx| failed("Couldn't change the status", &err, window, cx));
            }
        })
        .detach();
    }

    /// The groups shown, with their rows; a folded group has none.
    pub fn groups(&self, cx: &gpui::App) -> Vec<Group> {
        let filter = self.filter.read(cx).value();
        let query = Query {
            scope: Scope::All,
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
            scope: Scope::All,
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
