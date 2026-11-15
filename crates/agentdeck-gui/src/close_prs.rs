//! Closing PRs on GitHub from a task list (spec §11, Closing PRs): the Inbox, the Tasks view
//! and the right panel each own a [`ClosePrs`], which holds Select mode, the selection and
//! the confirm dialog, and closes PRs with `POST /v1/prs/close` (spec §8).
//!
//! A list draws the parts from it: the header's **Select** toggle ([`select_toggle`]), each
//! row's checkbox and `…` menu ([`row_parts`]), a group header's or the columns' select-all
//! ([`select_all`]) and the [`selection_bar`] along its bottom edge. The dialog renders with
//! the [`ClosePrs`] view itself, which the list places as a child.
//!
//! Penpot "Screens" → "Inbox · select", "Inbox · row menu", "Inbox · close confirm", "Inbox ·
//! partial failure" and "Right panel · select"; page-local parts `agentdeck/SelectionBar`,
//! `agentdeck/CheckboxIndeterminate` and `agentdeck/ClosePrsList`.

use std::collections::HashMap;
use std::sync::Arc;

use agentdeck_client::types::PrCloseResult;
use agentdeck_core::TaskId;
use brrritscold_core_gpui::{
    ActiveTheme as _, AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent,
    AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle, Button,
    ButtonSize, ButtonVariant, Checkbox, DropdownMenu, DropdownMenuContent, DropdownMenuItem,
    DropdownMenuItemVariant, Icon, IconName, Separator, Toggle, ToggleSize,
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
use crate::scrollbar;
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

    /// Takes in the results of closing the picked PRs and says how it went. When every one
    /// closed, Select mode ends. Otherwise it stays on with the failed ones still picked,
    /// each with its error, and the rest unpicked.
    pub fn closed(&mut self, results: &[PrCloseResult]) -> Notice {
        let closed = results.iter().filter(|r| r.ok).count();
        if closed == results.len() {
            self.set_active(false);
            return Notice::new(format!("Closed {}", prs(closed)));
        }
        self.selected = (results.iter())
            .filter(|r| !r.ok)
            .map(|r| r.task_id)
            .collect();
        self.errors = (results.iter())
            .filter(|r| !r.ok)
            .map(|r| {
                let error = r
                    .error
                    .clone()
                    .unwrap_or_else(|| "GitHub didn't close it".into());
                (r.task_id, error.into())
            })
            .collect();
        Notice::new(format!("Closed {closed} of {}", results.len()))
    }
}

/// A PR the confirm dialog asks about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Its task.
    pub id: TaskId,
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
            repo: pr.repo.clone(),
            reference: pr.reference.clone(),
        })
    }
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

    /// Turns Select mode on, or off, clearing the selection: what Select and Cancel do.
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.selection.set_active(active);
        cx.notify();
    }

    /// Picks `task`, or unpicks it.
    pub fn set(&mut self, task: TaskId, on: bool, cx: &mut Context<Self>) {
        self.selection.set(task, on);
        cx.notify();
    }

    /// Picks every one of `tasks`, or unpicks them.
    pub fn set_all(&mut self, tasks: &[TaskId], on: bool, cx: &mut Context<Self>) {
        self.selection.set_all(tasks, on);
        cx.notify();
    }

    /// Keeps only the picked tasks among `shown`, the selectable rows the list shows.
    pub fn retain(&mut self, shown: &[TaskId], cx: &mut Context<Self>) {
        if self.selection.retain(shown) {
            cx.notify();
        }
    }

    /// Opens the confirm dialog for the picked PRs, `targets`: what Close PRs… does.
    pub fn ask_selected(
        &mut self,
        targets: Vec<Target>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if targets.is_empty() {
            return;
        }
        self.ask(Asking::Selected(targets), window, cx);
    }

    /// Opens the confirm dialog for one PR: what a row's Close PR… does.
    pub fn ask_one(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(Asking::One(target), window, cx);
    }

    fn ask(&mut self, asking: Asking, window: &mut Window, cx: &mut Context<Self>) {
        self.asking = Some(asking);
        self.dialog.open(window, cx);
        cx.notify();
    }

    /// Whether the confirm dialog is open.
    #[cfg(test)]
    pub fn asking(&self) -> bool {
        self.dialog.is_open()
    }

    /// Closes the PRs the dialog asks about: what its Close button and Enter do. The dialog
    /// closes; the closed PRs' tasks go to done through the daemon's `changed`.
    pub fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(asking) = self.asking.take() else {
            return;
        };
        self.dialog.close(window, cx);
        let ids: Vec<TaskId> = asking.targets().iter().map(|t| t.id).collect();
        let closing = self.api.close_prs(ids, cx);
        cx.spawn_in(window, async move |this, cx| {
            let closed = closing.await;
            let _ = this.update_in(cx, |this, window, cx| {
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
            (Asking::Selected(_), Ok(results)) => self.selection.closed(&results).show(window, cx),
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
                .pl(space::S3)
                .pr(space::S6)
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
    let (id, active) = {
        let close = close.read(cx);
        (close.id, close.active())
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
                .on_change(move |on, _, cx| close.update(cx, |close, cx| close.set_active(on, cx))),
        )
        .into_any_element()
}

/// Penpot `agentdeck/CheckboxIndeterminate`: a checked box with a `Minus` in place of the
/// check, for a select-all over a partial selection. core-gpui's `Checkbox` draws no
/// indeterminate mark (core-react has none), so this is the app's own, styled as its checked
/// arm. Clicking it runs `on_click`.
fn checkbox_indeterminate(
    id: SharedString,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let s = cx.theme().semantic();
    // size-4 rounded-sm border shadow-xs, data-checked:border-primary data-checked:bg-primary
    // data-checked:text-primary-foreground
    div()
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
        .cursor_pointer()
        .on_click(move |_, window, cx| on_click(window, cx))
        .child(Icon::new(IconName::Minus).size(px(14.)))
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
    let id = state.id;
    let element_id = SharedString::from(format!("{id}-select-all-{key}"));
    let selector = format!("{id} select all {key}");
    let close = close.clone();
    let set = move |on: bool, cx: &mut App| {
        close.update(cx, |close, cx| close.set_all(&tasks, on, cx));
    };
    let checkbox = match check {
        Check::Some => checkbox_indeterminate(element_id, move |_, cx| set(true, cx), cx),
        Check::Off | Check::All => Checkbox::new(element_id)
            .checked(check == Check::All)
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

/// `row`'s `Select` and `Menu` layers and its close error: a checkbox in Select mode and a
/// `…` menu whose one item, Close PR…, asks to close its PR, both only when its PR can close.
/// Tests find them as `<id> select <task>` and `<id> menu <task>`.
pub fn row_parts(close: &Entity<ClosePrs>, row: &Row, cx: &App) -> RowParts {
    let state = close.read(cx);
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
    let menu = target.map(|target| {
        let close = close.clone();
        div()
            .debug_selector(move || format!("{id} menu {task}"))
            .child(
                DropdownMenu::new(SharedString::from(format!("{id}-menu-{task}")))
                    .trigger(
                        Button::new(SharedString::from(format!("{id}-menu-trigger-{task}")))
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::IconXs)
                            .icon(IconName::Ellipsis),
                    )
                    .content(
                        DropdownMenuContent::new().item(
                            DropdownMenuItem::new("Close PR…")
                                .variant(DropdownMenuItemVariant::Destructive)
                                .on_click(move |_, window, cx| {
                                    let target = target.clone();
                                    close.update(cx, |close, cx| close.ask_one(target, window, cx));
                                }),
                        ),
                    ),
            )
            .into_any_element()
    });
    RowParts {
        select,
        menu,
        error,
    }
}

/// Penpot `agentdeck/SelectionBar`, in Select mode: over a `border` line, "N selected" (sm
/// medium), an outline sm **Close PRs…** in `destructive`, disabled while nothing is picked,
/// which asks to close `targets` (the picked rows' PRs), and a ghost sm **Cancel**, which
/// ends Select mode. Tests find it as `<id> selection bar`.
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
                            .child(format!("{n} selected")),
                    )
                    .child(
                        div()
                            .debug_selector(move || format!("{id} close prs"))
                            .child(
                                Button::new(SharedString::from(format!("{id}-close-prs-button")))
                                    .variant(ButtonVariant::Outline)
                                    .size(ButtonSize::Sm)
                                    .label("Close PRs…")
                                    .text_color(s.destructive)
                                    .disabled(n == 0)
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
    use super::*;
    use crate::overview::fixtures::id;

    fn result(task: u32, error: Option<&str>) -> PrCloseResult {
        PrCloseResult {
            task_id: id(task),
            ok: error.is_none(),
            error: error.map(str::to_owned),
        }
    }

    fn picking(tasks: &[u32]) -> Selection {
        let mut selection = Selection::default();
        selection.set_active(true);
        for task in tasks {
            selection.set(id(*task), true);
        }
        selection
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
            assert_eq!(notice.description, None);
            assert!(selection.active());
            assert_eq!(selection.ids(), [id(2)]);
            assert_eq!(
                selection.error(id(2)).map(SharedString::as_ref),
                Some("Resource not accessible by personal access token")
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

    mod repo_counts {
        use super::*;

        fn target(task: u32, repo: &'static str) -> Target {
            Target {
                id: id(task),
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
