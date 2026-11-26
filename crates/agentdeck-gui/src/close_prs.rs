//! Closing PRs on GitHub from the Pull requests view (spec §11, Pull requests view → Select
//! mode, and Closing PRs): the view owns a [`ClosePrs`], which holds Select mode, the
//! selection and the confirm dialog, and closes PRs by reference with `POST /v1/prs/close`
//! (spec §8).
//!
//! The view draws the parts from it: the header's **Select** toggle ([`select_toggle`]), each
//! row's checkbox ([`select_cell`]), a group header's or the columns' select-all
//! ([`select_all`]) and the [`selection_bar`] along its bottom edge. The dialog renders with
//! the [`ClosePrs`] view itself, which the view places as a child.
//!
//! Penpot "Screens · Main" → "Pull requests · select", "Screens · Dialogs" → "Close PRs ·
//! confirm"; page-local parts `agentdeck/SelectionBar`, `agentdeck/CheckboxIndeterminate` and
//! `agentdeck/ClosePrsList`.

use std::collections::HashMap;
use std::fmt::Display;
use std::hash::Hash;
use std::sync::Arc;

use agentdeck_client::types::PrCloseResult;
use agentdeck_core::PrRef;
use brrritscold_core_gpui::{
    ActiveTheme as _, AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent,
    AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle, Button,
    ButtonSize, ButtonVariant, Checkbox, Icon, IconName, Separator, Toggle, ToggleSize,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px,
};
use gpui_base::DialogHandle;

use crate::api::Api;
use crate::notice::Notice;
use crate::scrollbar;
use crate::table::SelectCell;
use crate::workspace::failed;

/// How many repos the confirm dialog lists before its list scrolls.
const LIST_ROWS: usize = 6;

/// A repo line's height in the dialog's list. Raw in Penpot too.
const LIST_ROW_HEIGHT: f32 = 32.;

/// What a list picks its PRs by: a pull request on the Pull requests view.
pub trait Key: Copy + Eq + Hash + std::fmt::Debug + 'static {}

impl<K: Copy + Eq + Hash + std::fmt::Debug + 'static> Key for K {}

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

/// Select mode and the PRs picked in it, by the list's [`Key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection<K: Key> {
    active: bool,
    /// The rows picked, in the order picked.
    selected: Vec<K>,
    /// Why each PR that failed to close didn't, while it stays selected.
    errors: HashMap<K, SharedString>,
}

impl<K: Key> Default for Selection<K> {
    fn default() -> Self {
        Selection {
            active: false,
            selected: Vec::new(),
            errors: HashMap::new(),
        }
    }
}

impl<K: Key> Selection<K> {
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

    /// The keys picked, in the order picked.
    pub fn ids(&self) -> &[K] {
        &self.selected
    }

    /// Whether `key` is picked.
    pub fn contains(&self, key: K) -> bool {
        self.selected.contains(&key)
    }

    /// Picks `key`, or unpicks it, which forgets why it failed to close.
    pub fn set(&mut self, key: K, on: bool) {
        if on {
            if !self.contains(key) {
                self.selected.push(key);
            }
        } else {
            self.selected.retain(|t| *t != key);
            self.errors.remove(&key);
        }
    }

    /// Picks every one of `keys`, or unpicks them all.
    pub fn set_all(&mut self, keys: &[K], on: bool) {
        for key in keys {
            self.set(*key, on);
        }
    }

    /// How many of `keys` (a group's selectable rows) are picked.
    pub fn check(&self, keys: &[K]) -> Check {
        let picked = keys.iter().filter(|t| self.contains(**t)).count();
        match picked {
            0 => Check::Off,
            n if n == keys.len() => Check::All,
            _ => Check::Some,
        }
    }

    /// Keeps only the picked keys among `shown`: a row that hides leaves the selection.
    /// Returns whether anything left.
    pub fn retain(&mut self, shown: &[K]) -> bool {
        let before = self.selected.len();
        self.selected.retain(|t| shown.contains(t));
        self.errors.retain(|t, _| shown.contains(t));
        self.selected.len() != before
    }

    /// Why `key`'s PR didn't close, while it stays picked.
    pub fn error(&self, key: K) -> Option<&SharedString> {
        self.errors.get(&key)
    }

    /// Takes in the results of closing the picked PRs, each with its key, and says how it
    /// went. When every one closed, Select mode ends. Otherwise it stays on with the failed
    /// ones still picked, each with its error, and the rest unpicked. When some closed, the
    /// toast says how many and to hover the marked rows; when none closed, it is an error with
    /// the first one's error, as a close from a row's menu says why.
    pub fn closed(&mut self, results: &[(K, PrCloseResult)]) -> Notice {
        let closed = results.iter().filter(|(_, r)| r.ok).count();
        if closed == results.len() {
            self.set_active(false);
            return Notice::new(format!("Closed {}", prs(closed)));
        }
        let failed: Vec<(K, SharedString)> = (results.iter())
            .filter(|(_, r)| !r.ok)
            .map(|(key, r)| {
                let error = r
                    .error
                    .clone()
                    .unwrap_or_else(|| "GitHub didn't close it".into());
                (*key, error.into())
            })
            .collect();
        self.selected = failed.iter().map(|(key, _)| *key).collect();
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
/// of them the list shows closed so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closing<K> {
    targets: Vec<K>,
    closed: Vec<K>,
}

impl<K: Key> Closing<K> {
    /// Closing `targets`, none closed yet.
    pub fn new(targets: Vec<K>) -> Self {
        Closing {
            targets,
            closed: Vec::new(),
        }
    }

    /// How many have closed, of how many.
    pub fn progress(&self) -> (usize, usize) {
        (self.closed.len(), self.targets.len())
    }

    /// Counts the targets among `closed`, the rows the list now shows closed, each once. A row
    /// that leaves the list keeps its count. Returns whether the count moved.
    pub fn saw_closed(&mut self, closed: impl IntoIterator<Item = K>) -> bool {
        let before = self.closed.len();
        for key in closed {
            if self.targets.contains(&key) && !self.closed.contains(&key) {
                self.closed.push(key);
            }
        }
        self.closed.len() != before
    }
}

/// The selection bar's words: "N selected", or "Closing N of M…" while closing.
pub fn bar_label<K: Key>(selected: usize, closing: Option<&Closing<K>>) -> String {
    match closing.map(Closing::progress) {
        Some((done, of)) => format!("Closing {done} of {of}…"),
        None => format!("{selected} selected"),
    }
}

/// A PR the confirm dialog asks about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target<K> {
    /// Its row's key.
    pub id: Option<K>,
    /// Its reference, `owner/name#123`, which closing it sends.
    pub pr: PrRef,
    /// Its repo's name without the owner.
    pub repo: SharedString,
    /// Its repo and number: `ts-packages #151`.
    pub reference: SharedString,
}

/// Each repo among `targets` and how many of them it holds, most first, then in the order
/// they come.
pub fn repo_counts<K>(targets: &[Target<K>]) -> Vec<(SharedString, usize)> {
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
enum Asking<K> {
    /// The PRs picked in Select mode.
    Selected(Vec<Target<K>>),
    /// One PR, from its row's menu.
    One(Target<K>),
}

impl<K> Asking<K> {
    fn targets(&self) -> &[Target<K>] {
        match self {
            Asking::Selected(targets) => targets,
            Asking::One(target) => std::slice::from_ref(target),
        }
    }
}

/// One list's Select mode, selection and confirm dialog, its rows picked by `K`. `id` keys its
/// elements, so two lists on screen never share one.
pub struct ClosePrs<K: Key> {
    api: Arc<dyn Api>,
    id: &'static str,
    selection: Selection<K>,
    dialog: DialogHandle,
    asking: Option<Asking<K>>,
    /// The close of the picked PRs in flight, which locks Select mode and the selection.
    closing: Option<Closing<K>>,
    /// Scrolls the dialog's list of repos.
    scroll: ScrollHandle,
}

impl<K: Key> ClosePrs<K> {
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
    pub fn selection(&self) -> &Selection<K> {
        &self.selection
    }

    /// Whether Select mode is on.
    pub fn active(&self) -> bool {
        self.selection.active()
    }

    /// Why `key`'s PR didn't close, while it stays picked.
    pub fn error(&self, key: K) -> Option<&SharedString> {
        self.selection.error(key)
    }

    /// The selection bar's words: "N selected", or "Closing N of M…" while closing.
    #[cfg(test)]
    pub fn bar_label(&self) -> String {
        bar_label(self.selection.ids().len(), self.closing.as_ref())
    }

    /// Whether a close of the picked PRs is in flight. Select mode and the selection stay as
    /// they are until it answers.
    pub fn is_closing(&self) -> bool {
        self.closing.is_some()
    }

    /// Counts the PRs being closed among `closed`, the rows the list now shows closed.
    pub fn saw_closed(&mut self, closed: impl IntoIterator<Item = K>, cx: &mut Context<Self>) {
        if let Some(closing) = &mut self.closing
            && closing.saw_closed(closed)
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

    /// Whether `key` is picked.
    pub fn picked(&self, key: K) -> bool {
        self.selection.contains(key)
    }

    /// Picks `key`, or unpicks it when picked: a click on its row. Not while closing.
    pub fn toggle(&mut self, key: K, cx: &mut Context<Self>) {
        let on = !self.selection.contains(key);
        self.set(key, on, cx);
    }

    /// Picks `key`, or unpicks it. Not while closing.
    pub fn set(&mut self, key: K, on: bool, cx: &mut Context<Self>) {
        if self.is_closing() {
            return;
        }
        self.selection.set(key, on);
        cx.notify();
    }

    /// Picks every one of `keys`, or unpicks them. Not while closing.
    pub fn set_all(&mut self, keys: &[K], on: bool, cx: &mut Context<Self>) {
        if self.is_closing() {
            return;
        }
        self.selection.set_all(keys, on);
        cx.notify();
    }

    /// Keeps only the picked keys among `shown`, the selectable rows the list shows.
    pub fn retain(&mut self, shown: &[K], cx: &mut Context<Self>) {
        if self.selection.retain(shown) {
            cx.notify();
        }
    }

    /// Opens the confirm dialog for the picked PRs, `targets`: what Close PRs does. Not while
    /// they are closing.
    pub fn ask_selected(
        &mut self,
        targets: Vec<Target<K>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if targets.is_empty() || self.is_closing() {
            return;
        }
        self.ask(Asking::Selected(targets), window, cx);
    }

    /// Opens the confirm dialog for one PR: what a row's Close PR does.
    pub fn ask_one(&mut self, target: Target<K>, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(Asking::One(target), window, cx);
    }

    fn ask(&mut self, asking: Asking<K>, window: &mut Window, cx: &mut Context<Self>) {
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
    /// do. The dialog closes; the closed PRs come back closed through the daemon's `changed`.
    /// For the picked PRs, the selection bar counts them closing until the daemon answers.
    pub fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(asking) = self.asking.take() else {
            return;
        };
        self.dialog.close(window, cx);
        if let Asking::Selected(targets) = &asking {
            self.closing = Some(Closing::new(targets.iter().filter_map(|t| t.id).collect()));
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
        asking: &Asking<K>,
        closed: anyhow::Result<Vec<PrCloseResult>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match (asking, closed) {
            (Asking::Selected(targets), Ok(results)) => {
                // One result per reference, in the order asked.
                let results: Vec<(K, PrCloseResult)> = (targets.iter().zip(results))
                    .filter_map(|(target, result)| Some((target.id?, result)))
                    .collect();
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
                    if let Some(key) = target.id {
                        self.selection.set(key, false);
                    }
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
    fn repo_list(&self, targets: &[Target<K>], cx: &App) -> AnyElement {
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

impl<K: Key> Render for ClosePrs<K> {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let targets = (self.asking.as_ref()).map_or(&[][..], |asking| asking.targets());
        let n = targets.len();
        let id = self.id;
        // core-gpui runs the action's `on_click` on every confirm: the Close button, Enter on
        // it, or Enter on the dialog, each closing the PRs once.
        div().child(
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
                                                    .variant(ButtonVariant::Destructive)
                                                    .on_click(cx.listener(|this, _, window, cx| {
                                                        this.confirm(window, cx)
                                                    })),
                                                ),
                                        ),
                                ),
                        ),
                ),
        )
    }
}

/// The header's **Select**: a ghost (`default`) sm `Toggle`, pressed in Select mode, which
/// turns it on or off; disabled unless `enabled` (the Pull requests view with no PR to show)
/// and while closing. Tests find it as `<id> select`.
pub fn select_toggle<K: Key>(close: &Entity<ClosePrs<K>>, enabled: bool, cx: &App) -> AnyElement {
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
                .disabled(closing || !enabled)
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

/// A group header's or the columns' select-all over `keys`, the selectable rows under
/// it: off, all, or indeterminate while only some are picked. It picks them all, or, when all
/// are picked, unpicks them. With no selectable row, only its space. Tests find it as
/// `<id> select all <key>`.
pub fn select_all<K: Key>(
    close: &Entity<ClosePrs<K>>,
    key: &str,
    keys: Vec<K>,
    cx: &App,
) -> SelectCell {
    let state = close.read(cx);
    if !state.active() {
        return SelectCell::Off;
    }
    if keys.is_empty() {
        return SelectCell::Space;
    }
    let check = state.selection.check(&keys);
    let closing = state.is_closing();
    let id = state.id;
    let element_id = SharedString::from(format!("{id}-select-all-{key}"));
    let selector = format!("{id} select all {key}");
    let close = close.clone();
    let set = move |on: bool, cx: &mut App| {
        close.update(cx, |close, cx| close.set_all(&keys, on, cx));
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

/// A row's `Select` layer in Select mode: its checkbox, picking `key`, when `selectable`, else
/// only its space; nothing out of Select mode. `label` names it: tests find it as
/// `<id> select <label>`.
pub fn select_cell<K: Key>(
    close: &Entity<ClosePrs<K>>,
    key: K,
    selectable: bool,
    label: impl Display,
    cx: &App,
) -> SelectCell {
    let state = close.read(cx);
    if !state.active() {
        return SelectCell::Off;
    }
    if !selectable {
        return SelectCell::Space;
    }
    let id = state.id;
    let selector = format!("{id} select {label}");
    let close = close.clone();
    SelectCell::Checkbox(
        div()
            .debug_selector(move || selector.clone())
            .child(
                Checkbox::new(SharedString::from(format!("{id}-select-{label}")))
                    .checked(state.selection.contains(key))
                    .disabled(state.is_closing())
                    .accessibility_label("Select")
                    .on_change(move |on, _, cx| {
                        close.update(cx, |close, cx| close.set(key, on, cx));
                    }),
            )
            .into_any_element(),
    )
}

/// Penpot `agentdeck/SelectionBar`, in Select mode: over a `border` line, "N selected" (sm
/// medium); with `copy` (the picked PRs' details, on the Pull requests view) an outline sm
/// **Copy PR details**, disabled while nothing is picked, which puts them on the clipboard;
/// an outline sm **Close PRs** in `destructive`, disabled while nothing is picked, which asks
/// to close `targets` (the picked rows' PRs); and a ghost sm **Cancel**, which ends Select
/// mode. While the picked PRs close it reads "Closing N of M…" with Close PRs and Cancel
/// disabled (Penpot "Screens · Main" → "Pull requests · closing"). Tests find it as `<id> selection bar`, its
/// parts as `<id> selection count`, `<id> copy prs`, `<id> close prs` and `<id> cancel select`.
pub fn selection_bar<K: Key>(
    close: &Entity<ClosePrs<K>>,
    targets: Vec<Target<K>>,
    copy: Option<String>,
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
                            .debug_selector(move || format!("{id} selection count"))
                            .text_size(font_size::SM)
                            .font_weight(font_weight::MEDIUM)
                            .text_color(s.foreground)
                            .child(label),
                    )
                    .when_some(copy, |bar, details| {
                        bar.child(
                            div()
                                .debug_selector(move || format!("{id} copy prs"))
                                .child(
                                    Button::new(SharedString::from(format!("{id}-copy-prs")))
                                        .variant(ButtonVariant::Outline)
                                        .size(ButtonSize::Sm)
                                        .label("Copy PR details")
                                        .disabled(n == 0)
                                        .on_click(move |_, window, cx| {
                                            crate::notice::copy(
                                                details.clone(),
                                                "Copied PR details",
                                                window,
                                                cx,
                                            );
                                        }),
                                ),
                        )
                    })
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

#[cfg(test)]
mod tests {
    use brrritscold_core_gpui::ToastType;

    use super::*;

    /// Row `n`'s result: its PR, `acme/widgets#<n>`, closed or refused with `error`.
    fn result(n: u32, error: Option<&str>) -> (u32, PrCloseResult) {
        let result = PrCloseResult {
            pr: format!("acme/widgets#{n}").parse().unwrap(),
            ok: error.is_none(),
            error: error.map(str::to_owned),
        };
        (n, result)
    }

    fn picking(rows: &[u32]) -> Selection<u32> {
        let mut selection = Selection::default();
        selection.set_active(true);
        for n in rows {
            selection.set(*n, true);
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
            let group = [1, 2];

            assert_eq!(selection.check(&group), Check::Some);
            assert_eq!(picking(&[1, 2]).check(&group), Check::All);
            assert_eq!(picking(&[3]).check(&group), Check::Off);
        }

        #[test]
        fn should_pick_or_unpick_a_whole_group() {
            let mut selection = picking(&[1]);

            selection.set_all(&[1, 2], true);
            assert_eq!(selection.ids(), [1, 2]);
            selection.set_all(&[1, 2], false);
            assert!(selection.ids().is_empty());
        }

        #[test]
        fn should_drop_a_row_that_hides() {
            let mut selection = picking(&[1, 2]);

            assert!(selection.retain(&[2, 3]));
            assert_eq!(selection.ids(), [2]);
            assert!(!selection.retain(&[2]));
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
            assert_eq!(selection.ids(), [2]);
            assert_eq!(
                selection.error(2).map(SharedString::as_ref),
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
            assert_eq!(selection.ids(), [2, 3]);
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
            assert_eq!(selection.ids(), [1, 2]);
            assert_eq!(
                selection.error(2).map(SharedString::as_ref),
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

            selection.set(1, false);

            assert_eq!(selection.error(1), None);
        }
    }

    mod closing {
        use super::*;

        #[test]
        fn should_count_the_prs_the_list_shows_closed() {
            let mut closing = Closing::new(vec![1, 2]);
            assert_eq!(closing.progress(), (0, 2));

            assert!(closing.saw_closed([1]));
            assert_eq!(closing.progress(), (1, 2));
            assert!(closing.saw_closed([1, 2]));
            assert_eq!(closing.progress(), (2, 2));
        }

        #[test]
        fn should_count_each_pr_once() {
            let mut closing = Closing::new(vec![1, 2]);
            closing.saw_closed([1]);

            assert!(!closing.saw_closed([1]));
            assert_eq!(closing.progress(), (1, 2));
        }

        #[test]
        fn should_ignore_a_pr_it_does_not_close() {
            let mut closing = Closing::new(vec![1, 2]);

            assert!(!closing.saw_closed([3]));
            assert_eq!(closing.progress(), (0, 2));
        }

        #[test]
        fn should_keep_a_count_once_its_row_leaves() {
            let mut closing = Closing::new(vec![1, 2]);
            closing.saw_closed([1]);

            closing.saw_closed([]);

            assert_eq!(closing.progress(), (1, 2));
        }

        #[test]
        fn should_read_closing_n_of_m_in_place_of_the_count() {
            let mut closing = Closing::new(vec![1, 2]);
            assert_eq!(bar_label::<u32>(2, None), "2 selected");
            assert_eq!(bar_label(2, Some(&closing)), "Closing 0 of 2…");

            closing.saw_closed([2]);

            assert_eq!(bar_label(1, Some(&closing)), "Closing 1 of 2…");
        }
    }

    mod repo_counts {
        use super::*;

        fn target(n: u32, repo: &'static str) -> Target<u32> {
            Target {
                id: Some(n),
                pr: format!("acme/{repo}#{n}").parse().unwrap(),
                repo: repo.into(),
                reference: format!("{repo} #{n}").into(),
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
