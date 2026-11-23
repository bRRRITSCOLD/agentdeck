//! Closing PRs on GitHub from a task list (spec §11, Closing PRs): the Inbox, the Tasks view
//! and the right panel each own a [`ClosePrs`], which holds Select mode, the selection and
//! the confirm dialog, and closes PRs by reference with `POST /v1/prs/close` (spec §8).
//!
//! A list draws the parts from it: the header's **Select** toggle ([`select_toggle`]), each
//! row's checkbox and `…` menu ([`row_parts`]), a group header's or the columns' select-all
//! ([`select_all`]) and the [`selection_bar`] along its bottom edge. The dialog renders with
//! the [`ClosePrs`] view itself, which the list places as a child.
//!
//! The row's menu is on every row: **Edit task** first (spec §11, Editing a task), then on
//! a PR row, merged and closed ones too, **Open on GitHub** (spec §11, Opening PRs, through
//! [`open_pr`]), then **Copy task id** on every row, then **Close PR** after a separator when
//! its PR can close ([`menu_entries`]).
//!
//! Penpot "Screens" → "Inbox · select", "Inbox · row menu", "Inbox · row menu · open PR",
//! "Tasks · row menu · merged PR", "Inbox · close confirm", "Inbox · closing", "Inbox · partial
//! failure" and "Right panel · select"; page-local parts `agentdeck/SelectionBar`,
//! `agentdeck/CheckboxIndeterminate` and `agentdeck/ClosePrsList`.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use agentdeck_client::types::{PrCloseResult, Task};
use agentdeck_core::{PrRef, PrState, TaskId};
use brrritscold_core_gpui::{
    ActiveTheme as _, AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent,
    AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle, Button,
    ButtonSize, ButtonVariant, Checkbox, DropdownMenu, DropdownMenuAlign, DropdownMenuContent,
    DropdownMenuItem, DropdownMenuItemVariant, DropdownMenuSeparator, Icon, IconName, Separator,
    Toggle, ToggleSize,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px,
};
use gpui_base::DialogHandle;
use gpui_base::actions::Confirm;

use crate::api::Api;
use crate::notice::Notice;
use crate::open_pr;
use crate::scrollbar;
use crate::send_task::{self, Sending, TaskTarget};
use crate::task_list::Row;
use crate::task_row::{RowParts, SelectCell};
use crate::workspace::failed;

/// How many repos the confirm dialog lists before its list scrolls.
const LIST_ROWS: usize = 6;

/// A repo line's height in the dialog's list. Raw in Penpot too.
const LIST_ROW_HEIGHT: f32 = 32.;

/// `n` PRs, as a count: "1 PR", "2 PRs".
pub fn prs(n: usize) -> String {
    if n == 1 {
        "1 PR".to_owned()
    } else {
        format!("{n} PRs")
    }
}

/// A selected group's or list's checkbox: how many of its selectable rows are selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// None of them.
    Off,
    /// Some, not all: indeterminate.
    Some,
    /// Every one.
    All,
}

/// Select mode and the PRs picked in it, as a list keeps them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    active: bool,
    /// The tasks picked, in the order picked.
    selected: Vec<TaskId>,
    /// Why each PR that failed to close didn't, while it stays selected.
    errors: HashMap<TaskId, SharedString>,
}

impl Selection {
    /// Whether Select mode is on.
    pub fn active(&self) -> bool {
        self.active
    }

    /// Turns Select mode on, or off, which clears the selection.
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
        if !active {
            self.selected.clear();
            self.errors.clear();
        }
    }

    /// The tasks picked, in the order picked.
    pub fn ids(&self) -> &[TaskId] {
        &self.selected
    }

    /// Whether `task` is picked.
    pub fn contains(&self, task: TaskId) -> bool {
        self.selected.contains(&task)
    }

    /// Picks `task`, or unpicks it, which forgets why it failed to close.
    pub fn set(&mut self, task: TaskId, on: bool) {
        if on {
            if !self.contains(task) {
                self.selected.push(task);
            }
        } else {
            self.selected.retain(|t| *t != task);
            self.errors.remove(&task);
        }
    }

    /// Picks every one of `tasks`, or unpicks them all.
    pub fn set_all(&mut self, tasks: &[TaskId], on: bool) {
        for task in tasks {
            self.set(*task, on);
        }
    }

    /// How many of `tasks` (a group's selectable rows) are picked.
    pub fn check(&self, tasks: &[TaskId]) -> Check {
        let picked = tasks.iter().filter(|t| self.contains(**t)).count();
        match picked {
            0 => Check::Off,
            n if n == tasks.len() => Check::All,
            _ => Check::Some,
        }
    }

    /// Keeps only the picked tasks among `shown`: a row that hides leaves the selection.
    /// Returns whether anything left.
    pub fn retain(&mut self, shown: &[TaskId]) -> bool {
        let before = self.selected.len();
        self.selected.retain(|t| shown.contains(t));
        self.errors.retain(|t, _| shown.contains(t));
        self.selected.len() != before
    }

    /// Why `task`'s PR didn't close, while it stays picked.
    pub fn error(&self, task: TaskId) -> Option<&SharedString> {
        self.errors.get(&task)
    }

    /// Takes in the results of closing the picked PRs, each with its task, and says how it
    /// went. When every one closed, Select mode ends. Otherwise it stays on with the failed
    /// ones still picked, each with its error, and the rest unpicked. When some closed, the
    /// toast says how many and to hover the marked rows; when none closed, it is an error with
    /// the first one's error, as a close from a row's menu says why.
    pub fn closed(&mut self, results: &[(TaskId, PrCloseResult)]) -> Notice {
        let closed = results.iter().filter(|(_, r)| r.ok).count();
        if closed == results.len() {
            self.set_active(false);
            return Notice::new(format!("Closed {}", prs(closed)));
        }
        let failed: Vec<(TaskId, SharedString)> = (results.iter())
            .filter(|(_, r)| !r.ok)
            .map(|(task, r)| {
                let error = r
                    .error
                    .clone()
                    .unwrap_or_else(|| "GitHub didn't close it".into());
                (*task, error.into())
            })
            .collect();
        self.selected = failed.iter().map(|(task, _)| *task).collect();
        self.errors = failed.iter().cloned().collect();
        if closed == 0 {
            let (_, first) = &failed[0];
            return Notice::error(
                format!("Couldn't close {}", prs(failed.len())),
                first.clone(),
            );
        }
        let marked = if failed.len() == 1 {
            "the marked PR"
        } else {
            "the marked PRs"
        };
        Notice::new(format!("Closed {closed} of {}", results.len()))
            .description(format!("Hover {marked} to see why."))
    }
}

/// A close of the picked PRs in flight (spec §11, Closing PRs): the PRs it closes, and those
/// of them the tasks show closed so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Closing {
    targets: Vec<TaskId>,
    closed: Vec<TaskId>,
}

impl Closing {
    /// Closing `targets`, none closed yet.
    pub fn new(targets: Vec<TaskId>) -> Self {
        Closing {
            targets,
            closed: Vec::new(),
        }
    }

    /// How many have closed, of how many.
    pub fn progress(&self) -> (usize, usize) {
        (self.closed.len(), self.targets.len())
    }

    /// Counts the targets `tasks` show closed, each once: the daemon stores a PR closed as
    /// GitHub closes it and sends its task's `changed`, which reads the tasks again. A task
    /// that leaves `tasks` keeps its count. Returns whether the count moved.
    pub fn saw(&mut self, tasks: &[Task]) -> bool {
        let before = self.closed.len();
        for task in tasks {
            let closed = (task.pr.as_ref()).is_some_and(|pr| pr.state == PrState::Closed);
            if closed && self.targets.contains(&task.id) && !self.closed.contains(&task.id) {
                self.closed.push(task.id);
            }
        }
        self.closed.len() != before
    }
}

/// The selection bar's words: "N selected", or "Closing N of M…" while closing.
pub fn bar_label(selected: usize, closing: Option<&Closing>) -> String {
    match closing.map(Closing::progress) {
        Some((done, of)) => format!("Closing {done} of {of}…"),
        None => format!("{selected} selected"),
    }
}

/// A PR the confirm dialog asks about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Its task.
    pub id: TaskId,
    /// Its reference, `owner/name#123`, which closing it sends.
    pub pr: PrRef,
    /// Its repo's name without the owner.
    pub repo: SharedString,
    /// Its repo and number: `ts-packages #151`.
    pub reference: SharedString,
}

impl Target {
    /// `row`'s PR, if it can close.
    pub fn of(row: &Row) -> Option<Target> {
        let pr = row.pr.as_ref().filter(|_| row.closable)?;
        Some(Target {
            id: row.id,
            pr: pr.full_reference.clone()?,
            repo: pr.repo.clone(),
            reference: pr.reference.clone(),
        })
    }
}

/// One entry in a task row's `…` menu (spec §11, Editing a task, Opening PRs and Closing
/// PRs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    /// **Edit task**, opening the task sheet to edit the task.
    EditTask,
    /// **Send to agent**, a submenu of the agent panes the task can go to (spec §11, Send to
    /// agent).
    SendToAgent(Vec<TaskTarget>),
    /// **Open on GitHub**, opening this URL, the PR's.
    OpenOnGitHub(SharedString),
    /// **Copy task id**, putting this task's id on the clipboard.
    CopyTaskId(TaskId),
    /// A line between the entries.
    Separator,
    /// **Close PR**, asking to close this PR.
    ClosePr(Target),
}

/// `row`'s `…` menu, top to bottom: **Edit task** on every row; **Send to agent** over
/// `send`, the panes [`send_task::targets`] offers, unless the task is done (`None`); **Open
/// on GitHub** on every PR row the sync has filled in, whatever its PR's state; **Copy task
/// id** on every row; then, when its PR can close, a separator and **Close PR**.
pub fn menu_entries(row: &Row, send: Option<Vec<TaskTarget>>) -> Vec<MenuEntry> {
    let mut entries = vec![MenuEntry::EditTask];
    entries.extend(send.map(MenuEntry::SendToAgent));
    if let Some(pr) = &row.pr {
        entries.push(MenuEntry::OpenOnGitHub(pr.url.clone()));
    }
    entries.push(MenuEntry::CopyTaskId(row.id));
    if let Some(target) = Target::of(row) {
        entries.extend([MenuEntry::Separator, MenuEntry::ClosePr(target)]);
    }
    entries
}

/// Each repo among `targets` and how many of them it holds, most first, then in the order
/// they come.
pub fn repo_counts(targets: &[Target]) -> Vec<(SharedString, usize)> {
    let mut counts: Vec<(SharedString, usize)> = Vec::new();
    for target in targets {
        match counts.iter_mut().find(|(repo, _)| *repo == target.repo) {
            Some((_, n)) => *n += 1,
            None => counts.push((target.repo.clone(), 1)),
        }
    }
    counts.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    counts
}

/// What the open confirm dialog closes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Asking {
    /// The PRs picked in Select mode.
    Selected(Vec<Target>),
    /// One PR, from its row's menu.
    One(Target),
}

impl Asking {
    fn targets(&self) -> &[Target] {
        match self {
            Asking::Selected(targets) => targets,
            Asking::One(target) => std::slice::from_ref(target),
        }
    }
}

/// One list's Select mode, selection and confirm dialog. `id` keys its elements, so two lists
/// on screen never share one.
pub struct ClosePrs {
    api: Arc<dyn Api>,
    id: &'static str,
    selection: Selection,
    dialog: DialogHandle,
    asking: Option<Asking>,
    /// The close of the picked PRs in flight, which locks Select mode and the selection.
    closing: Option<Closing>,
    /// Scrolls the dialog's list of repos.
    scroll: ScrollHandle,
}

impl ClosePrs {
    /// Select mode off, closing through `api`; `id` keys its elements.
    pub fn new(api: Arc<dyn Api>, id: &'static str) -> Self {
        ClosePrs {
            api,
            id,
            selection: Selection::default(),
            dialog: DialogHandle::new(false),
            asking: None,
            closing: None,
            scroll: ScrollHandle::new(),
        }
    }

    /// Select mode and the selection.
    #[cfg(test)]
    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// Whether Select mode is on.
    pub fn active(&self) -> bool {
        self.selection.active()
    }

    /// Why `task`'s PR didn't close, while it stays picked.
    pub fn error(&self, task: TaskId) -> Option<&SharedString> {
        self.selection.error(task)
    }

    /// Whether a close of the picked PRs is in flight. Select mode and the selection stay as
    /// they are until it answers.
    pub fn is_closing(&self) -> bool {
        self.closing.is_some()
    }

    /// Counts the PRs being closed that `tasks`, the snapshot's, show closed.
    pub fn saw(&mut self, tasks: &[Task], cx: &mut Context<Self>) {
        if let Some(closing) = &mut self.closing
            && closing.saw(tasks)
        {
            cx.notify();
        }
    }

    /// Turns Select mode on, or off, clearing the selection: what Select and Cancel do. Not
    /// while closing.
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.is_closing() {
            return;
        }
        self.selection.set_active(active);
        cx.notify();
    }

    /// Picks `task`, or unpicks it. Not while closing.
    pub fn set(&mut self, task: TaskId, on: bool, cx: &mut Context<Self>) {
        if self.is_closing() {
            return;
        }
        self.selection.set(task, on);
        cx.notify();
    }

    /// Picks every one of `tasks`, or unpicks them. Not while closing.
    pub fn set_all(&mut self, tasks: &[TaskId], on: bool, cx: &mut Context<Self>) {
        if self.is_closing() {
            return;
        }
        self.selection.set_all(tasks, on);
        cx.notify();
    }

    /// Keeps only the picked tasks among `shown`, the selectable rows the list shows.
    pub fn retain(&mut self, shown: &[TaskId], cx: &mut Context<Self>) {
        if self.selection.retain(shown) {
            cx.notify();
        }
    }

    /// Opens the confirm dialog for the picked PRs, `targets`: what Close PRs does. Not while
    /// they are closing.
    pub fn ask_selected(
        &mut self,
        targets: Vec<Target>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if targets.is_empty() || self.is_closing() {
            return;
        }
        self.ask(Asking::Selected(targets), window, cx);
    }

    /// Opens the confirm dialog for one PR: what a row's Close PR does.
    pub fn ask_one(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(Asking::One(target), window, cx);
    }

    fn ask(&mut self, asking: Asking, window: &mut Window, cx: &mut Context<Self>) {
        self.asking = Some(asking);
        self.dialog.open(window, cx);
        cx.notify();
    }

    /// How many of the picked PRs being closed have closed, of how many; `None` while no
    /// close of them is in flight.
    #[cfg(test)]
    pub fn closing(&self) -> Option<(usize, usize)> {
        self.closing.as_ref().map(Closing::progress)
    }

    /// Whether the confirm dialog is open.
    #[cfg(test)]
    pub fn asking(&self) -> bool {
        self.dialog.is_open()
    }

    /// Closes the PRs the dialog asks about, by reference: what its Close button and Enter
    /// do. The dialog closes; the closed PRs' tasks go to done through the daemon's `changed`.
    /// For the picked PRs, the selection bar counts them closing until the daemon answers.
    pub fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(asking) = self.asking.take() else {
            return;
        };
        self.dialog.close(window, cx);
        if let Asking::Selected(targets) = &asking {
            self.closing = Some(Closing::new(targets.iter().map(|t| t.id).collect()));
        }
        let prs: Vec<PrRef> = asking.targets().iter().map(|t| t.pr.clone()).collect();
        let closing = self.api.close_prs(prs, cx);
        cx.spawn_in(window, async move |this, cx| {
            let closed = closing.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Asking::Selected(_) = asking {
                    this.closing = None;
                }
                this.report(&asking, closed, window, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Says how a close went, and updates the selection when it closed the picked PRs.
    fn report(
        &mut self,
        asking: &Asking,
        closed: anyhow::Result<Vec<PrCloseResult>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match (asking, closed) {
            (Asking::Selected(targets), Ok(results)) => {
                // One result per reference, in the order asked.
                let results: Vec<(TaskId, PrCloseResult)> =
                    targets.iter().map(|t| t.id).zip(results).collect();
                self.selection.closed(&results).show(window, cx);
            }
            (Asking::Selected(_), Err(err)) => failed("Couldn't close PRs", &err, window, cx),
            (Asking::One(target), Ok(results)) => match results.first() {
                Some(result) if !result.ok => {
                    let error = result.error.clone().unwrap_or_default();
                    Notice::error(format!("Couldn't close {}", target.reference), error)
                        .show(window, cx);
                }
                _ => {
                    self.selection.set(target.id, false);
                    Notice::new(format!("Closed {}", prs(1))).show(window, cx);
                }
            },
            (Asking::One(target), Err(err)) => {
                failed(
                    format!("Couldn't close {}", target.reference),
                    &err,
                    window,
                    cx,
                );
            }
        }
    }

    /// Penpot `agentdeck/ClosePrsList`: a line per repo with how many of its PRs close, in a
    /// `border` box that scrolls past [`LIST_ROWS`] lines.
    fn repo_list(&self, targets: &[Target], cx: &App) -> AnyElement {
        let s = cx.theme().semantic();
        let counts = repo_counts(targets);
        let scrolls = counts.len() > LIST_ROWS;
        let lines = counts.into_iter().map(|(repo, n)| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_between()
                .h(px(LIST_ROW_HEIGHT))
                // Penpot's row pads `space.3` in, but its text sits `space.6` in, as wide as
                // the room kept for the scrollbar at the other end.
                .px(space::S6)
                .text_size(font_size::SM)
                .child(div().text_color(s.foreground).child(repo))
                .child(div().text_color(s.muted_foreground).child(prs(n)))
        });
        let id = self.id;
        div()
            .debug_selector(move || format!("{id} close list"))
            .relative()
            .flex()
            .flex_col()
            .py(px(1.))
            .border_1()
            .border_color(s.border)
            .rounded(s.radius_md)
            .when(scrolls, |list| {
                list.h(px(LIST_ROW_HEIGHT * LIST_ROWS as f32 + 2.))
            })
            .child(
                div()
                    .id(SharedString::from(format!("{id}-close-list")))
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(lines),
            )
            .when(scrolls, |list| {
                list.child(scrollbar::vertical(&self.scroll, cx))
            })
            .into_any_element()
    }
}

impl Render for ClosePrs {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let targets = (self.asking.as_ref()).map_or(&[][..], |asking| asking.targets());
        let n = targets.len();
        let id = self.id;
        // Enter, and the Close button, dispatch the dialog's `Confirm`; it is caught on its
        // way down to the dialog, which then closes, so both close the PRs once.
        div()
            .capture_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm(window, cx)))
            .child(
                AlertDialog::new(SharedString::from(format!("{id}-close-prs")))
                    .handle(self.dialog.clone())
                    .content(
                        AlertDialogContent::new()
                            .header(
                                AlertDialogHeader::new()
                                    .title(AlertDialogTitle::new(format!(
                                        "Close {} on GitHub?",
                                        prs(n)
                                    )))
                                    .description(AlertDialogDescription::new(
                                        "Their repos' owners will see them closed. You can \
                                         reopen a PR on GitHub.",
                                    )),
                            )
                            // core-gpui's content has no body part, so the list leads the
                            // footer's column, `space.6` above the buttons, as Penpot places
                            // it between the header and the footer.
                            .footer(
                                AlertDialogFooter::new()
                                    .flex_col()
                                    .gap(space::S6)
                                    .child(self.repo_list(targets, cx))
                                    .child(
                                        div()
                                            .flex()
                                            .justify_end()
                                            .gap(space::S2)
                                            .child(AlertDialogCancel::new(
                                                SharedString::from(format!("{id}-close-cancel")),
                                                "Cancel",
                                            ))
                                            .child(
                                                div()
                                                    .debug_selector(move || {
                                                        format!("{id} close confirm")
                                                    })
                                                    .child(
                                                        AlertDialogAction::new(
                                                            SharedString::from(format!(
                                                                "{id}-close-confirm"
                                                            )),
                                                            format!("Close {}", prs(n)),
                                                        )
                                                        .variant(ButtonVariant::Destructive),
                                                    ),
                                            ),
                                    ),
                            ),
                    ),
            )
    }
}

/// The header's **Select**: a ghost (`default`) sm `Toggle`, pressed in Select mode, which
/// turns it on or off. Tests find it as `<id> select`.
pub fn select_toggle(close: &Entity<ClosePrs>, cx: &App) -> AnyElement {
    let (id, active, closing) = {
        let close = close.read(cx);
        (close.id, close.active(), close.is_closing())
    };
    let close = close.clone();
    div()
        .flex_none()
        .debug_selector(move || format!("{id} select"))
        .child(
            Toggle::new(SharedString::from(format!("{id}-select")))
                .size(ToggleSize::Sm)
                .label("Select")
                .pressed(active)
                .disabled(closing)
                .on_change(move |on, _, cx| close.update(cx, |close, cx| close.set_active(on, cx))),
        )
        .into_any_element()
}

/// Penpot `agentdeck/CheckboxIndeterminate`: a checked box with a `Minus` in place of the
/// check, for a select-all over a partial selection. core-gpui's `Checkbox` draws no
/// indeterminate mark (core-react has none), so this is the app's own, styled as its checked
/// arm. Clicking it runs `on_click`, unless `disabled`, which dims it to 50%.
fn checkbox_indeterminate(
    id: SharedString,
    disabled: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let s = cx.theme().semantic();
    // size-4 rounded-sm border shadow-xs, data-checked:border-primary data-checked:bg-primary
    // data-checked:text-primary-foreground data-disabled:opacity-50
    let checkbox = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(space::S4)
        .rounded(s.radius_sm)
        .border_1()
        .border_color(s.primary)
        .bg(s.primary)
        .text_color(s.primary_foreground)
        .child(Icon::new(IconName::Minus).size(px(14.)));
    if disabled {
        return checkbox.opacity(0.5).into_any_element();
    }
    checkbox
        .cursor_pointer()
        .on_click(move |_, window, cx| on_click(window, cx))
        .into_any_element()
}

/// A group header's or the Inbox columns' select-all over `tasks`, the selectable rows under
/// it: off, all, or indeterminate while only some are picked. It picks them all, or, when all
/// are picked, unpicks them. With no selectable row, only its space. Tests find it as
/// `<id> select all <key>`.
pub fn select_all(close: &Entity<ClosePrs>, key: &str, tasks: Vec<TaskId>, cx: &App) -> SelectCell {
    let state = close.read(cx);
    if !state.active() {
        return SelectCell::Off;
    }
    if tasks.is_empty() {
        return SelectCell::Space;
    }
    let check = state.selection.check(&tasks);
    let closing = state.is_closing();
    let id = state.id;
    let element_id = SharedString::from(format!("{id}-select-all-{key}"));
    let selector = format!("{id} select all {key}");
    let close = close.clone();
    let set = move |on: bool, cx: &mut App| {
        close.update(cx, |close, cx| close.set_all(&tasks, on, cx));
    };
    let checkbox = match check {
        Check::Some => checkbox_indeterminate(element_id, closing, move |_, cx| set(true, cx), cx),
        Check::Off | Check::All => Checkbox::new(element_id)
            .checked(check == Check::All)
            .disabled(closing)
            .accessibility_label("Select all")
            .on_change(move |on, _, cx| set(on, cx))
            .into_any_element(),
    };
    SelectCell::Checkbox(
        div()
            .debug_selector(move || selector.clone())
            .child(checkbox)
            .into_any_element(),
    )
}

/// `row`'s `Select` and `Menu` layers and its close error: a checkbox in Select mode, only when
/// its PR can close, and on every row a `…` menu of its [`menu_entries`]: **Edit task**,
/// which calls `on_edit`; **Send to agent** over `send` unless the task is done, which sends
/// it to the pane picked; on a PR row **Open on GitHub**; **Copy task id**; then **Close PR**
/// after a separator when its PR can close. Tests find them as `<id> select <task>` and `<id> menu <task>`.
pub fn row_parts(
    close: &Entity<ClosePrs>,
    row: &Row,
    send: Option<Vec<TaskTarget>>,
    on_edit: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> RowParts {
    let state = close.read(cx);
    let sending = Sending {
        task: row.id,
        title: row.title.clone(),
        inbox: row.workspace.is_none(),
    };
    let api = state.api.clone();
    let on_send: send_task::OnPick = Rc::new(move |target, window, cx| {
        send_task::send(api.clone(), sending.clone(), target, window, cx);
    });
    let id = state.id;
    let task = row.id;
    let target = Target::of(row);
    let select = match (&target, state.active()) {
        (_, false) => SelectCell::Off,
        (None, true) => SelectCell::Space,
        (Some(_), true) => {
            let close = close.clone();
            SelectCell::Checkbox(
                div()
                    .debug_selector(move || format!("{id} select {task}"))
                    .child(
                        Checkbox::new(SharedString::from(format!("{id}-select-{task}")))
                            .checked(state.selection.contains(task))
                            .disabled(state.is_closing())
                            .accessibility_label("Select")
                            .on_change(move |on, _, cx| {
                                close.update(cx, |close, cx| close.set(task, on, cx));
                            }),
                    )
                    .into_any_element(),
            )
        }
    };
    let error = state.selection.error(task).cloned();
    let on_edit = std::rc::Rc::new(on_edit);
    let menu = {
        let content = (menu_entries(row, send).into_iter()).fold(
            DropdownMenuContent::new(),
            |content, entry| match entry {
                MenuEntry::EditTask => {
                    let on_edit = on_edit.clone();
                    content.item(
                        DropdownMenuItem::new("Edit task")
                            .on_click(move |_, window, cx| on_edit(window, cx)),
                    )
                }
                MenuEntry::SendToAgent(targets) => {
                    let (trigger, sub) = send_task::submenu(targets, on_send.clone());
                    content.sub(trigger, sub)
                }
                MenuEntry::OpenOnGitHub(url) => content.item(
                    DropdownMenuItem::new("Open on GitHub")
                        .on_click(move |_, window, cx| open_pr::open(&url, window, cx)),
                ),
                MenuEntry::CopyTaskId(task) => content.item(
                    DropdownMenuItem::new("Copy task id").on_click(move |_, window, cx| {
                        crate::notice::copy(task.to_string(), "Copied task id", window, cx);
                    }),
                ),
                MenuEntry::Separator => content.separator(DropdownMenuSeparator::new()),
                MenuEntry::ClosePr(target) => {
                    let close = close.clone();
                    content.item(
                        DropdownMenuItem::new("Close PR")
                            .variant(DropdownMenuItemVariant::Destructive)
                            .on_click(move |_, window, cx| {
                                let target = target.clone();
                                close.update(cx, |close, cx| close.ask_one(target, window, cx));
                            }),
                    )
                }
            },
        );
        div()
            .debug_selector(move || format!("{id} menu {task}"))
            .child(
                // Penpot's "… · row menu" boards end it on the trigger's right edge.
                DropdownMenu::new(SharedString::from(format!("{id}-menu-{task}")))
                    .align(DropdownMenuAlign::End)
                    .trigger(
                        Button::new(SharedString::from(format!("{id}-menu-trigger-{task}")))
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::IconXs)
                            .icon(IconName::Ellipsis),
                    )
                    .content(content),
            )
            .into_any_element()
    };
    RowParts {
        select,
        menu,
        error,
    }
}

/// Penpot `agentdeck/SelectionBar`, in Select mode: over a `border` line, "N selected" (sm
/// medium), an outline sm **Close PRs** in `destructive`, disabled while nothing is picked,
/// which asks to close `targets` (the picked rows' PRs), and a ghost sm **Cancel**, which
/// ends Select mode. While the picked PRs close it reads "Closing N of M…" with both buttons
/// disabled (Penpot "Screens" → "Inbox · closing"). Tests find it as `<id> selection bar`.
pub fn selection_bar(
    close: &Entity<ClosePrs>,
    targets: Vec<Target>,
    cx: &App,
) -> Option<AnyElement> {
    let state = close.read(cx);
    if !state.active() {
        return None;
    }
    let s = cx.theme().semantic();
    let id = state.id;
    let n = state.selection.ids().len();
    let closing = state.is_closing();
    let label = bar_label(n, state.closing.as_ref());
    let ask = close.clone();
    let cancel = close.clone();
    Some(
        div()
            .debug_selector(move || format!("{id} selection bar"))
            .flex()
            .flex_col()
            .flex_none()
            .child(Separator::new())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::S3)
                    .py(space::S3)
                    .px(space::S2)
                    .child(
                        div()
                            .text_size(font_size::SM)
                            .font_weight(font_weight::MEDIUM)
                            .text_color(s.foreground)
                            .child(label),
                    )
                    .child(
                        div()
                            .debug_selector(move || format!("{id} close prs"))
                            .child(
                                Button::new(SharedString::from(format!("{id}-close-prs-button")))
                                    .variant(ButtonVariant::Outline)
                                    .size(ButtonSize::Sm)
                                    .label("Close PRs")
                                    .text_color(s.destructive)
                                    .disabled(n == 0 || closing)
                                    .on_click(move |_, window, cx| {
                                        let targets = targets.clone();
                                        ask.update(cx, |close, cx| {
                                            close.ask_selected(targets, window, cx);
                                        });
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .debug_selector(move || format!("{id} cancel select"))
                            .child(
                                Button::new(SharedString::from(format!("{id}-cancel-select")))
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::Sm)
                                    .label("Cancel")
                                    .disabled(closing)
                                    .on_click(move |_, _, cx| {
                                        cancel.update(cx, |close, cx| close.set_active(false, cx));
                                    }),
                            ),
                    ),
            )
            .into_any_element(),
    )
}

/// The picked rows' PRs among `rows`, in the rows' order.
pub fn picked(close: &Entity<ClosePrs>, rows: &[Row], cx: &App) -> Vec<Target> {
    let selection = &close.read(cx).selection;
    (rows.iter())
        .filter(|row| selection.contains(row.id))
        .filter_map(Target::of)
        .collect()
}

/// The ids of `rows` whose PR can close: the selectable ones.
pub fn selectable(rows: &[Row]) -> Vec<TaskId> {
    (rows.iter())
        .filter(|row| Target::of(row).is_some())
        .map(|row| row.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use brrritscold_core_gpui::ToastType;

    use super::*;
    use crate::overview::fixtures::id;

    /// Task `task`'s result: its PR, `acme/widgets#<task>`, closed or refused with `error`.
    fn result(task: u32, error: Option<&str>) -> (TaskId, PrCloseResult) {
        let result = PrCloseResult {
            pr: format!("acme/widgets#{task}").parse().unwrap(),
            ok: error.is_none(),
            error: error.map(str::to_owned),
        };
        (id(task), result)
    }

    fn picking(tasks: &[u32]) -> Selection {
        let mut selection = Selection::default();
        selection.set_active(true);
        for task in tasks {
            selection.set(id(*task), true);
        }
        selection
    }

    mod menu_entries {
        use agentdeck_core::{PrState, TaskKind};

        use super::*;
        use crate::overview::Snapshot;
        use crate::overview::fixtures::{pr_task, task};
        use crate::task_list::{Query, Scope, rows};

        /// The menu of the one row `task` makes.
        fn menu(task: agentdeck_client::types::Task) -> Vec<MenuEntry> {
            let snapshot = Snapshot {
                tasks: vec![task],
                ..Snapshot::default()
            };
            let query = Query {
                scope: Scope::All,
                filter: "",
                show_done: true,
            };
            let row = &rows(&snapshot, &query)[0];
            menu_entries(row, crate::send_task::targets(&snapshot, row.id))
        }

        fn open_on_github(n: u32) -> MenuEntry {
            MenuEntry::OpenOnGitHub(format!("https://github.com/acme/widgets/pull/{n}").into())
        }

        #[test]
        fn should_offer_edit_send_open_copy_then_close_on_an_open_pr() {
            assert_eq!(
                menu(pr_task(1, "acme/widgets", PrState::Open, None)),
                [
                    MenuEntry::EditTask,
                    MenuEntry::SendToAgent(vec![]),
                    open_on_github(1),
                    MenuEntry::CopyTaskId(id(1)),
                    MenuEntry::Separator,
                    MenuEntry::ClosePr(Target {
                        id: id(1),
                        pr: "acme/widgets#1".parse().unwrap(),
                        repo: "widgets".into(),
                        reference: "widgets #1".into(),
                    }),
                ]
            );
        }

        #[test]
        fn should_offer_edit_send_open_and_copy_on_a_merged_or_closed_pr() {
            assert_eq!(
                menu(pr_task(2, "acme/widgets", PrState::Merged, None)),
                [
                    MenuEntry::EditTask,
                    MenuEntry::SendToAgent(vec![]),
                    open_on_github(2),
                    MenuEntry::CopyTaskId(id(2))
                ]
            );
            assert_eq!(
                menu(pr_task(3, "acme/widgets", PrState::Closed, None)),
                [
                    MenuEntry::EditTask,
                    MenuEntry::SendToAgent(vec![]),
                    open_on_github(3),
                    MenuEntry::CopyTaskId(id(3))
                ]
            );
        }

        #[test]
        fn should_offer_edit_send_and_copy_on_a_plain_task_or_an_unsynced_pr() {
            assert_eq!(
                menu(task(4, TaskKind::Plain, None)),
                [
                    MenuEntry::EditTask,
                    MenuEntry::SendToAgent(vec![]),
                    MenuEntry::CopyTaskId(id(4))
                ]
            );
            assert_eq!(
                menu(task(5, TaskKind::Pr, None)),
                [
                    MenuEntry::EditTask,
                    MenuEntry::SendToAgent(vec![]),
                    MenuEntry::CopyTaskId(id(5))
                ]
            );
        }

        #[test]
        fn should_leave_send_to_agent_out_of_a_done_tasks_menu() {
            let done = agentdeck_client::types::Task {
                status: agentdeck_core::TaskStatus::Done,
                ..task(6, TaskKind::Plain, None)
            };

            assert_eq!(
                menu(done),
                [MenuEntry::EditTask, MenuEntry::CopyTaskId(id(6))]
            );
        }
    }

    mod selection {
        use super::*;

        #[test]
        fn should_clear_the_selection_when_select_mode_ends() {
            let mut selection = picking(&[1, 2]);

            selection.set_active(false);

            assert!(!selection.active());
            assert!(selection.ids().is_empty());
        }

        #[test]
        fn should_be_indeterminate_while_only_some_of_a_group_are_picked() {
            let selection = picking(&[1]);
            let group = [id(1), id(2)];

            assert_eq!(selection.check(&group), Check::Some);
            assert_eq!(picking(&[1, 2]).check(&group), Check::All);
            assert_eq!(picking(&[3]).check(&group), Check::Off);
        }

        #[test]
        fn should_pick_or_unpick_a_whole_group() {
            let mut selection = picking(&[1]);

            selection.set_all(&[id(1), id(2)], true);
            assert_eq!(selection.ids(), [id(1), id(2)]);
            selection.set_all(&[id(1), id(2)], false);
            assert!(selection.ids().is_empty());
        }

        #[test]
        fn should_drop_a_row_that_hides() {
            let mut selection = picking(&[1, 2]);

            assert!(selection.retain(&[id(2), id(3)]));
            assert_eq!(selection.ids(), [id(2)]);
            assert!(!selection.retain(&[id(2)]));
        }

        #[test]
        fn should_end_select_mode_when_every_pr_closed() {
            let mut selection = picking(&[1, 2]);

            let notice = selection.closed(&[result(1, None), result(2, None)]);

            assert_eq!(notice.title, "Closed 2 PRs");
            assert_eq!(notice.description, None);
            assert!(!selection.active());
        }

        #[test]
        fn should_keep_only_the_failed_rows_picked_with_their_errors() {
            let mut selection = picking(&[1, 2, 3]);

            let notice = selection.closed(&[
                result(1, None),
                result(2, Some("Resource not accessible by personal access token")),
                result(3, None),
            ]);

            assert_eq!(notice.title, "Closed 2 of 3");
            assert_eq!(notice.kind, None);
            assert_eq!(
                notice.description.as_deref(),
                Some("Hover the marked PR to see why.")
            );
            assert!(selection.active());
            assert_eq!(selection.ids(), [id(2)]);
            assert_eq!(
                selection.error(id(2)).map(SharedString::as_ref),
                Some("Resource not accessible by personal access token")
            );
        }

        #[test]
        fn should_point_at_the_marked_prs_when_more_than_one_did_not_close() {
            let mut selection = picking(&[1, 2, 3]);

            let notice = selection.closed(&[
                result(1, None),
                result(2, Some("Must have admin rights")),
                result(3, Some("Must have admin rights")),
            ]);

            assert_eq!(notice.title, "Closed 1 of 3");
            assert_eq!(
                notice.description.as_deref(),
                Some("Hover the marked PRs to see why.")
            );
            assert_eq!(selection.ids(), [id(2), id(3)]);
        }

        #[test]
        fn should_toast_an_error_with_the_first_reason_when_none_closed() {
            let mut selection = picking(&[1, 2]);

            let notice = selection.closed(&[
                result(1, Some("Resource not accessible by personal access token")),
                result(2, Some("Must have admin rights")),
            ]);

            assert_eq!(notice.kind, Some(ToastType::Error));
            assert_eq!(notice.title, "Couldn't close 2 PRs");
            assert_eq!(
                notice.description.as_deref(),
                Some("Resource not accessible by personal access token")
            );
            assert!(selection.active());
            assert_eq!(selection.ids(), [id(1), id(2)]);
            assert_eq!(
                selection.error(id(2)).map(SharedString::as_ref),
                Some("Must have admin rights")
            );
        }

        #[test]
        fn should_name_one_pr_when_the_only_one_did_not_close() {
            let mut selection = picking(&[1]);

            let notice = selection.closed(&[result(1, Some("Must have admin rights"))]);

            assert_eq!(notice.title, "Couldn't close 1 PR");
            assert_eq!(
                notice.description.as_deref(),
                Some("Must have admin rights")
            );
        }

        #[test]
        fn should_forget_the_error_once_the_row_is_unpicked() {
            let mut selection = picking(&[1]);
            selection.closed(&[result(1, Some("no"))]);

            selection.set(id(1), false);

            assert_eq!(selection.error(id(1)), None);
        }
    }

    mod closing {
        use agentdeck_core::PrState;

        use super::*;
        use crate::overview::fixtures::pr_task;

        fn tasks(closed: &[u32]) -> Vec<agentdeck_client::types::Task> {
            (1..=3)
                .map(|n| {
                    let state = if closed.contains(&n) {
                        PrState::Closed
                    } else {
                        PrState::Open
                    };
                    pr_task(n, "acme/widgets", state, None)
                })
                .collect()
        }

        #[test]
        fn should_count_the_prs_the_tasks_show_closed() {
            let mut closing = Closing::new(vec![id(1), id(2)]);
            assert_eq!(closing.progress(), (0, 2));

            assert!(closing.saw(&tasks(&[1])));
            assert_eq!(closing.progress(), (1, 2));
            assert!(closing.saw(&tasks(&[1, 2])));
            assert_eq!(closing.progress(), (2, 2));
        }

        #[test]
        fn should_count_each_pr_once() {
            let mut closing = Closing::new(vec![id(1), id(2)]);
            closing.saw(&tasks(&[1]));

            assert!(!closing.saw(&tasks(&[1])));
            assert_eq!(closing.progress(), (1, 2));
        }

        #[test]
        fn should_ignore_a_pr_it_does_not_close() {
            let mut closing = Closing::new(vec![id(1), id(2)]);

            assert!(!closing.saw(&tasks(&[3])));
            assert_eq!(closing.progress(), (0, 2));
        }

        #[test]
        fn should_keep_a_count_once_its_task_leaves() {
            let mut closing = Closing::new(vec![id(1), id(2)]);
            closing.saw(&tasks(&[1]));

            closing.saw(&tasks(&[])[1..]);

            assert_eq!(closing.progress(), (1, 2));
        }

        #[test]
        fn should_read_closing_n_of_m_in_place_of_the_count() {
            let mut closing = Closing::new(vec![id(1), id(2)]);
            assert_eq!(bar_label(2, None), "2 selected");
            assert_eq!(bar_label(2, Some(&closing)), "Closing 0 of 2…");

            closing.saw(&tasks(&[2]));

            assert_eq!(bar_label(1, Some(&closing)), "Closing 1 of 2…");
        }
    }

    mod repo_counts {
        use super::*;

        fn target(task: u32, repo: &'static str) -> Target {
            Target {
                id: id(task),
                pr: format!("acme/{repo}#{task}").parse().unwrap(),
                repo: repo.into(),
                reference: format!("{repo} #{task}").into(),
            }
        }

        #[test]
        fn should_count_each_repos_prs_most_first() {
            let targets = [
                target(1, "ts-packages"),
                target(2, "agentdeck"),
                target(3, "agentdeck"),
                target(4, "herdr"),
            ];

            assert_eq!(
                repo_counts(&targets),
                [
                    ("agentdeck".into(), 2),
                    ("ts-packages".into(), 1),
                    ("herdr".into(), 1)
                ]
            );
        }
    }

    #[test]
    fn should_count_prs_in_words() {
        assert_eq!(prs(1), "1 PR");
        assert_eq!(prs(14), "14 PRs");
    }
}
