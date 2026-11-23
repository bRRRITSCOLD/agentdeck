//! The parts of a PR table (spec §11, Pull requests view): its scrolling frame ([`table_of`]),
//! the fixed column widths ([`Columns`]), a cell, Penpot's `GroupHeader`, core-gpui's `Checks`
//! for a PR's checks, its review, and the mark on a row whose PR didn't close.
//!
//! In Select mode a row and a group header start with their `Select` layer, a [`SelectCell`];
//! every row ends with its `Menu` layer. A row with nothing to select keeps the `Select`
//! layer's space, so the columns line up.
//!
//! A list draws only the items in view, through GPUI's [`list`](gpui::list): a long PR list
//! drawn whole made each frame take hundreds of milliseconds.

use agentdeck_core::{ChecksState, ReviewState};
use brrritscold_core_gpui::{
    ActiveTheme as _, Badge, BadgeVariant, Checks, ChecksState as UiChecksState, Icon, IconName,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::{
    AnyElement, App, Div, InteractiveElement as _, IntoElement, ListAlignment, ListState,
    MouseButton, ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, canvas, div, px,
};

/// How far past the viewport a list draws, so a short scroll shows rows at once.
pub const OVERDRAW: Pixels = px(200.);

/// How far right of the list its scrollbar's track ends: Penpot puts the track in the
/// content's `space.6` padding, 2px in from the window's edge, clear of the rows' `Menu`.
const SCROLLBAR_OUTSET: Pixels = px(22.);

/// The state of a list of rows, empty until [`relist`]ed.
pub fn list_state() -> ListState {
    ListState::new(0, ListAlignment::Top, OVERDRAW)
}

/// A table's column names, as [`table_of`] draws them over its rows.
pub struct Header {
    /// The column names.
    pub columns: Div,
    /// The same names as wide, with nothing to click: an unseen copy keeps the scrollbar
    /// level with the list.
    pub ghost: Div,
    /// The table's least width; a narrower area scrolls it sideways.
    pub min_width: Pixels,
}

/// A table: `header` over `rows`, `list`'s rows, as wide as the area allows and never narrower
/// than its `min_width`, which a narrower area scrolls sideways, by a sideways wheel only. The
/// list's scrollbar sits just right of the area, in the content's padding and outside that
/// scroll, so it stays in view at any width; the header's unseen `ghost` keeps it level with the
/// list. Each frame tells `on_width` the area's width.
pub fn table_of(
    id: &'static str,
    selector: &'static str,
    header: Header,
    rows: impl IntoElement,
    list: &ListState,
    on_width: impl Fn(&Pixels, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let Header {
        columns,
        ghost,
        min_width,
    } = header;
    let scrolled = div()
        .id(id)
        .flex_1()
        .min_h_0()
        .overflow_x_scroll()
        // Only a sideways wheel scrolls it; a vertical one is the list's.
        .restrict_scroll_to_axis()
        .flex()
        .flex_col()
        .child(
            div()
                .flex_1()
                .min_h_0()
                .min_w(min_width)
                .flex()
                .flex_col()
                .gap(space::S4)
                .child(columns)
                .child(rows),
        );
    // As wide as the area and its scrollbar, not the table: the copy of the columns
    // overflows it, unseen.
    let scrollbar = div()
        .absolute()
        .top_0()
        .bottom_0()
        .left_0()
        .right(-SCROLLBAR_OUTSET)
        .overflow_hidden()
        .flex()
        .flex_col()
        .gap(space::S4)
        .child(ghost.min_w(min_width).invisible())
        .child(
            div()
                .flex_1()
                .min_h_0()
                .child(crate::scrollbar::vertical(list, cx).viewport_from_layout()),
        );
    div()
        .debug_selector(move || selector.into())
        .relative()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(scrolled)
        .child(scrollbar)
        .child(
            canvas(
                move |bounds, window, cx| on_width(&bounds.size.width, window, cx),
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        )
}

/// Makes `list` hold `count` items, keeping its scroll position, and measures them again: a
/// row's height follows its title, and an index may now hold a group header instead.
pub fn relist(list: &ListState, count: usize) {
    let old = list.item_count();
    // An empty list drawn once keeps its top at item 0, which a splice at 0 would push past
    // the items it adds, to their end. With nothing shown, there's no place to keep.
    if old == 0 {
        list.reset(count);
        return;
    }
    if count > old {
        list.splice(old..old, count - old);
    } else if count < old {
        list.splice(count..old, 0);
    }
    list.remeasure();
}

/// An item of a list, `space.0-5` from the next: a list has no gap of its own.
pub fn spaced(item: AnyElement) -> AnyElement {
    div().pb(space::S0_5).child(item).into_any_element()
}

/// The fixed column widths of Penpot's `Columns` and `TaskRow` (`wide`): raw there too, as
/// no sizing token matches them. The title column takes what's left.
pub struct Columns;

impl Columns {
    /// The state icon's slot.
    pub const ICON: Pixels = px(16.);
    /// PR reference.
    pub const PR: Pixels = px(140.);
    /// The draft badge's slot.
    pub const DRAFT: Pixels = px(52.);
    /// Checks.
    pub const CHECKS: Pixels = px(80.);
    /// Review.
    pub const REVIEW: Pixels = px(128.);
    /// The `Menu` layer's slot: a ghost icon-xs button, or its space.
    pub const MENU: Pixels = px(24.);
    /// The `Select` layer's slot in Select mode: a checkbox, or its space.
    pub const SELECT: Pixels = px(16.);
    /// The title's least width. Below it the table scrolls sideways, so a narrow window never
    /// squeezes the title to nothing. The window's least width fits the table with it, in
    /// Select mode too (spec §11, Window).
    pub const TITLE_MIN: Pixels = px(160.);
}

/// A row's or a group header's `Select` layer.
pub enum SelectCell {
    /// Not in Select mode: no slot.
    Off,
    /// In Select mode, with nothing to select: the slot's space, so titles stay aligned.
    Space,
    /// In Select mode: this checkbox.
    Checkbox(AnyElement),
}

impl SelectCell {
    /// The slot, `Columns::SELECT` square; `None` when off. A click in it stays in it, so a
    /// group header's checkbox doesn't fold the group too.
    pub fn slot(self) -> Option<Div> {
        let slot = div()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .size(Columns::SELECT)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        match self {
            SelectCell::Off => None,
            SelectCell::Space => Some(slot),
            SelectCell::Checkbox(checkbox) => Some(slot.child(checkbox)),
        }
    }
}

/// What a row shows beyond its PR: its `Select` and `Menu` layers, and the error its PR's
/// close failed with, marked after its title and shown on hover.
pub struct RowParts {
    /// The `Select` layer.
    pub select: SelectCell,
    /// The `Menu` layer.
    pub menu: AnyElement,
    /// Why its PR didn't close, while it stays selected.
    pub error: Option<SharedString>,
}

/// A cell `width` wide, its content in a row `space.1-5` apart.
pub fn cell(width: Pixels) -> Div {
    div()
        .flex_none()
        .w(width)
        .min_w_0()
        .flex()
        .items_center()
        .gap(space::S1_5)
}

/// Penpot `GroupHeader`: its `Select` layer in Select mode, a chevron, `label` and a
/// `secondary` count badge. Clicking it, but for its checkbox, runs `on_toggle`. `key` names
/// it: its id is `group-<key>`, and tests find it as `group <key>`.
pub fn group_header(
    key: impl Into<SharedString>,
    label: impl Into<SharedString>,
    count: usize,
    collapsed: bool,
    select: SelectCell,
    on_toggle: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let s = cx.theme().semantic();
    let key = key.into();
    div()
        .id(SharedString::from(format!("group-{key}")))
        .debug_selector(move || format!("group {key}"))
        .flex()
        .flex_none()
        .items_center()
        .gap(space::S2)
        .px(space::S2)
        .py(space::S1_5)
        .cursor_pointer()
        .on_click(on_toggle)
        .children(select.slot())
        .child(
            Icon::new(if collapsed {
                IconName::ChevronRight
            } else {
                IconName::ChevronDown
            })
            .size(space::S4)
            .text_color(s.foreground),
        )
        .child(
            div()
                .text_size(font_size::SM)
                .font_weight(font_weight::MEDIUM)
                .text_color(s.foreground)
                .child(label.into()),
        )
        .child(
            Badge::new()
                .variant(BadgeVariant::Secondary)
                .label(count.to_string()),
        )
        .into_any_element()
}

/// core-gpui's `Checks` state for a PR's checks, and the word beside it (spec §11).
fn checks_mark(state: ChecksState) -> (UiChecksState, &'static str) {
    match state {
        ChecksState::Success => (UiChecksState::Success, "passing"),
        ChecksState::Pending => (UiChecksState::Pending, "running"),
        ChecksState::Failure => (UiChecksState::Failure, "failing"),
    }
}

/// Penpot `Checks`: the icon and its word, as a PR row shows its checks. Running checks are
/// drawn still, as a working agent's mark is
/// ([`state_dot::mark`](crate::state_dot::mark)).
pub fn checks(state: ChecksState) -> Checks {
    let (mark, word) = checks_mark(state);
    Checks::new(mark).label(word).spin(false)
}

/// The review cell: a badge in the review colour (spec §11), or muted text while one is
/// still required.
pub fn review(state: ReviewState, cx: &App) -> AnyElement {
    match state {
        ReviewState::Approved => Badge::new()
            .variant(BadgeVariant::Success)
            .label("approved")
            .into_any_element(),
        ReviewState::ChangesRequested => Badge::new()
            .variant(BadgeVariant::Warning)
            .label("changes requested")
            .into_any_element(),
        ReviewState::ReviewRequired => div()
            .text_size(font_size::XS)
            .text_color(cx.theme().semantic().muted_foreground)
            .child("review required")
            .into_any_element(),
    }
}

/// The tooltip on the title of a row whose PR the GitHub sign-in can't see (spec §11; Penpot
/// "Screens · Main" → "Pull requests · unreachable PR").
pub const UNREACHABLE: &str =
    "Your GitHub sign-in can't see this PR. It's shown as it was last synced.";

/// The mark on a row whose PR didn't close: a 14px lucide `CircleExclamationPoint` in
/// `destructive`, the failing checks' red. Tests find it as `selector`.
pub fn close_error_mark(selector: String, cx: &App) -> Div {
    div()
        .debug_selector(move || selector.clone())
        .flex_none()
        .flex()
        .child(
            Icon::new(IconName::CircleExclamationPoint)
                .size(px(14.))
                .text_color(cx.theme().semantic().destructive),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    mod checks {
        use gpui::TestAppContext;

        use super::*;
        use crate::state_dot::tests::{host, redraws_in_one_second};

        #[test]
        fn should_give_each_state_its_library_state_and_word() {
            assert_eq!(
                checks_mark(ChecksState::Success),
                (UiChecksState::Success, "passing")
            );
            assert_eq!(
                checks_mark(ChecksState::Pending),
                (UiChecksState::Pending, "running")
            );
            assert_eq!(
                checks_mark(ChecksState::Failure),
                (UiChecksState::Failure, "failing")
            );
        }

        #[gpui::test]
        fn should_hold_running_checks_still(cx: &mut TestAppContext) {
            let (row, cx) = host(|_| checks(ChecksState::Pending).into_any_element(), cx);
            assert_eq!(redraws_in_one_second(&row, cx), 0);
        }
    }
}
