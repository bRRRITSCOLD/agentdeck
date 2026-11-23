//! Penpot's page parts for task lists, `TaskRow` (its `wide` layout) and `GroupHeader`, drawn
//! from a [`task_list::Row`](crate::task_list::Row), with core-gpui's `Checks` for a PR's
//! checks. The Tasks view and the Inbox lay them out, each as its [`Table`]; their column
//! widths are [`Columns`]'s.
//!
//! In Select mode (spec §11, Closing PRs) a row and a group header start with their `Select`
//! layer, a [`SelectCell`]; every row ends with its `Menu` layer. A row with nothing to select
//! keeps the `Select` layer's space, so the columns line up.
//!
//! Both lists draw only the items in view, through GPUI's [`list`](gpui::list): an inbox holds
//! hundreds of PRs, and drawing every row made each frame take hundreds of milliseconds.

use agentdeck_core::{ChecksState, PrState, ReviewState, TaskKind, TaskStatus};
use brrritscold_core_gpui::{
    ActiveTheme as _, Badge, BadgeVariant, Checks, ChecksState as UiChecksState, Icon, IconName,
    Select, SelectContent, SelectItem, SelectTrigger, SelectTriggerSize, Tooltip, TooltipContent,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Div, ElementId, Hsla, InteractiveElement as _, IntoElement, ListAlignment,
    ListState, MouseButton, ParentElement as _, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, canvas, div, px,
};

use crate::state_dot;
use crate::task_list::{PrCell, Row};

/// How far past the viewport a list draws, so a short scroll shows rows at once.
pub const OVERDRAW: Pixels = px(200.);

/// How far right of the list its scrollbar's track ends: Penpot puts the track in the
/// content's `space.6` padding, 2px in from the window's edge, clear of the rows' `Menu`.
const SCROLLBAR_OUTSET: Pixels = px(22.);

/// The state of a list of rows, empty until [`relist`]ed.
pub fn list_state() -> ListState {
    ListState::new(0, ListAlignment::Top, OVERDRAW)
}

/// Columns over `rows`, `list`'s rows, as wide as the area allows and never narrower than
/// `layout`: a narrower area scrolls them sideways together, by a sideways wheel only. The
/// list's scrollbar sits just right of the area, in the content's padding and outside that
/// scroll, so it stays in view at any width; a hidden copy of the columns keeps it level
/// with the list.
///
/// Each frame tells `on_width` the area's width, so the view can [`Table::fit`] its layout
/// to it for the next. `select` leads the columns in Select mode.
#[allow(clippy::too_many_arguments)]
pub fn table(
    id: &'static str,
    selector: &'static str,
    layout: Layout,
    select: SelectCell,
    rows: impl IntoElement,
    list: &ListState,
    on_width: impl Fn(&Pixels, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let header = Header {
        columns: columns(layout, select, cx),
        ghost: columns(layout, SelectCell::Space, cx),
        min_width: layout.min_width(),
    };
    table_of(id, selector, header, rows, list, on_width, cx)
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

/// [`table`] for any columns: `header` over `rows`, `list`'s rows, as wide as the area allows
/// and never narrower than its `min_width`, with the list's scrollbar just right of the area.
/// Each frame tells `on_width` the area's width.
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

/// Where a table's scrollbar track sits for a list or table at `area`: just right of it.
#[cfg(test)]
pub fn scrollbar_area(area: gpui::Bounds<Pixels>) -> gpui::Bounds<Pixels> {
    gpui::Bounds {
        size: gpui::size(area.size.width + SCROLLBAR_OUTSET, area.size.height),
        ..area
    }
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
    /// Status picker.
    pub const STATUS: Pixels = px(100.);
    /// The kind icon's slot.
    pub const ICON: Pixels = px(16.);
    /// Linked agent.
    pub const AGENT: Pixels = px(96.);
    /// PR reference.
    pub const PR: Pixels = px(140.);
    /// The draft badge's slot.
    pub const DRAFT: Pixels = px(52.);
    /// Checks.
    pub const CHECKS: Pixels = px(80.);
    /// Review.
    pub const REVIEW: Pixels = px(128.);
    /// Workspace.
    pub const WORKSPACE: Pixels = px(140.);
    /// The `Menu` layer's slot: a ghost icon-xs button, or its space.
    pub const MENU: Pixels = px(24.);
    /// The `Select` layer's slot in Select mode: a checkbox, or its space.
    pub const SELECT: Pixels = px(16.);
    /// The title's least width. Below it a column drops ([`Table::fit`]), and below the
    /// narrowest layout the table scrolls sideways, so a narrow window never squeezes the
    /// title to nothing. Penpot "Tasks · 1200" fits beside the sidebar with it, in Select
    /// mode too (spec §11, Table width).
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

/// What a row shows beyond its task: its `Select` and `Menu` layers, and the error its PR's
/// close failed with, marked after its title and shown on hover.
pub struct RowParts {
    /// The `Select` layer.
    pub select: SelectCell,
    /// The `Menu` layer: Edit task, then on a PR row Open on GitHub, then Copy task id, and
    /// Close PR when its PR can close.
    pub menu: AnyElement,
    /// Why its PR didn't close, while it stays selected.
    pub error: Option<SharedString>,
}

/// Which list a table is: the Inbox's has no agent or workspace column and names the others
/// its own way, as Penpot's "Inbox" board does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Table {
    /// The Tasks view.
    Tasks,
    /// The Inbox.
    Inbox,
}

impl Table {
    /// Its layouts, widest first, in Select mode or not: each drops one more column, in the
    /// order the table gives them up when it doesn't fit. Tasks drops Agent, then Checks;
    /// Review and Workspace stay. The Inbox, which has neither Agent nor Workspace, drops
    /// Checks, then Review.
    fn layouts(self, select: bool) -> [Layout; 3] {
        let layout = |agent, checks, review| Layout {
            table: self,
            select,
            agent,
            checks,
            review,
            workspace: self == Table::Tasks,
        };
        match self {
            Table::Tasks => [
                layout(true, true, true),
                layout(false, true, true),
                layout(false, false, true),
            ],
            Table::Inbox => [
                layout(false, true, true),
                layout(false, false, true),
                layout(false, false, false),
            ],
        }
    }

    /// Every column the table has, out of Select mode.
    pub fn widest(self) -> Layout {
        self.layouts(false)[0]
    }

    /// The widest layout that fits `width`, in Select mode or not; below the narrowest, the
    /// narrowest, which then scrolls sideways.
    pub fn fit(self, width: Pixels, select: bool) -> Layout {
        let layouts = self.layouts(select);
        let narrowest = layouts[layouts.len() - 1];
        (layouts.into_iter())
            .find(|layout| layout.min_width() <= width)
            .unwrap_or(narrowest)
    }
}

/// The columns a table shows at its width: status, kind icon, title, PR and draft always;
/// the others while they fit ([`Table::fit`]). Header and row cells follow the same layout,
/// so they drop together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Which list it is, which names the columns.
    pub table: Table,
    /// Select mode: every row and the header start with a `Select` slot.
    pub select: bool,
    /// Linked agent (Tasks only).
    pub agent: bool,
    /// Checks.
    pub checks: bool,
    /// Review.
    pub review: bool,
    /// Workspace (Tasks only).
    pub workspace: bool,
}

impl Layout {
    /// The table's least width: every column it shows, the title at its least, the
    /// `space.3` gaps and the rows' `space.2` padding.
    pub fn min_width(self) -> Pixels {
        let fixed = [
            Columns::STATUS,
            Columns::ICON,
            Columns::TITLE_MIN,
            Columns::PR,
            Columns::DRAFT,
            Columns::MENU,
        ];
        let optional = [
            (self.select, Columns::SELECT),
            (self.agent, Columns::AGENT),
            (self.checks, Columns::CHECKS),
            (self.review, Columns::REVIEW),
            (self.workspace, Columns::WORKSPACE),
        ];
        let shown = (optional.iter()).filter_map(|(shown, width)| shown.then_some(width));
        let (sum, count) =
            (fixed.iter().chain(shown)).fold((px(0.), 0.), |(sum, n), w| (sum + *w, n + 1.));
        sum + space::S3 * (count - 1.) + space::S2 * 2.
    }
}

/// How a task status reads in a picker or a group header.
pub fn status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Todo => "Todo",
        TaskStatus::Doing => "Doing",
        TaskStatus::Review => "Review",
        TaskStatus::Done => "Done",
    }
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

/// `—`, for a cell with nothing to show.
pub fn none(cx: &App) -> Div {
    div()
        .text_size(font_size::XS)
        .text_color(cx.theme().semantic().muted_foreground)
        .child("—")
}

/// Penpot `Columns`: the column names over the rows, xs medium muted; `layout`'s columns
/// only, starting with `select` in Select mode (the Inbox's select-all) and ending with the
/// `Menu` layer's space. Tests find a named column as `column <name>`.
pub fn columns(layout: Layout, select: SelectCell, cx: &App) -> Div {
    let s = cx.theme().semantic();
    let name = |width: Option<Pixels>, text: &'static str| {
        let cell = match width {
            Some(width) => cell(width),
            None => div().flex_1().min_w(Columns::TITLE_MIN),
        };
        cell.debug_selector(move || format!("column {text}"))
            .child(text)
    };
    div()
        .flex()
        .flex_none()
        .gap(space::S3)
        .px(space::S2)
        .text_size(font_size::XS)
        .font_weight(font_weight::MEDIUM)
        .text_color(s.muted_foreground)
        .children(select_slot(layout, select))
        .child(name(Some(Columns::STATUS), "Status"))
        .child(name(Some(Columns::ICON), ""))
        .map(|row| match layout.table {
            Table::Tasks => row.child(name(None, "Task")),
            Table::Inbox => row.child(name(None, "Title")),
        })
        .when(layout.agent, |row| {
            row.child(name(Some(Columns::AGENT), "Agent"))
        })
        .map(|row| match layout.table {
            Table::Tasks => row.child(name(Some(Columns::PR), "PR")),
            Table::Inbox => row.child(name(Some(Columns::PR), "Repository")),
        })
        .child(name(Some(Columns::DRAFT), ""))
        .when(layout.checks, |row| {
            row.child(name(Some(Columns::CHECKS), "Checks"))
        })
        .when(layout.review, |row| {
            row.child(name(Some(Columns::REVIEW), "Review"))
        })
        .when(layout.workspace, |row| {
            row.child(name(Some(Columns::WORKSPACE), "Workspace"))
        })
        .child(cell(Columns::MENU))
}

/// `select`'s slot as `layout` has it: none out of Select mode, at least its space in it.
fn select_slot(layout: Layout, select: SelectCell) -> Option<Div> {
    match (layout.select, select) {
        (false, _) => None,
        (true, SelectCell::Off) => SelectCell::Space.slot(),
        (true, select) => select.slot(),
    }
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

/// Penpot `Checks`: the icon and its word, as the Tasks view, the Inbox and the task sheet
/// show a PR's checks. Running checks are drawn still, as a working agent's mark is
/// ([`state_dot::mark`]).
pub fn checks(state: ChecksState) -> Checks {
    let (mark, word) = checks_mark(state);
    Checks::new(mark).label(word).spin(false)
}

/// Penpot `Checks` as the right panel shows it: the icon alone.
fn checks_icon(state: ChecksState) -> Checks {
    Checks::new(checks_mark(state).0).spin(false)
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

/// A status picker's items: every status, by its label.
pub fn status_content() -> SelectContent {
    (TaskStatus::ALL.iter()).fold(SelectContent::new(), |content, s| {
        content.item(SelectItem::new(s.as_str(), status_label(*s)))
    })
}

/// The status picker: core-gpui's `Select` at `sm`, listing every status. Choosing one runs
/// `on_change` with it.
pub fn status_picker(
    id: impl Into<ElementId>,
    status: TaskStatus,
    on_change: impl Fn(&TaskStatus, &mut Window, &mut App) + 'static,
) -> Select {
    Select::new(id)
        // Penpot's `Status` trigger is 95px whatever the status, so the column lines up.
        .trigger(SelectTrigger::new().size(SelectTriggerSize::Sm).w(px(95.)))
        .content(status_content())
        .value(Some(status.as_str()))
        .on_value_change(move |value, window, cx| {
            if let Ok(status) = value.parse() {
                on_change(&status, window, cx);
            }
        })
}

/// The tooltip on the title of a row whose PR the GitHub sign-in can't see (spec §11; Penpot
/// "Screens · Main" → "Inbox · unreachable PR").
pub const UNREACHABLE: &str =
    "Your GitHub sign-in can't see this PR. It's shown as it was last synced.";

/// How opaque `row` draws, over all of it: half while the GitHub sign-in can't see its PR,
/// which shows as it was last synced (Penpot "Inbox · unreachable PR"); else whole.
pub fn opacity(row: &Row) -> f32 {
    if row.unreachable { 0.5 } else { 1. }
}

/// What clicking a row's title does: open the task sheet on it.
pub type OnOpen = Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static>;

/// A row's title, sm: clicking it runs `on_open`. With an `error`, why its PR didn't close,
/// the [`close_error`] mark follows it, and hovering either shows the error in a tooltip
/// (spec §11, Closing PRs). An unreachable PR's title shows [`UNREACHABLE`] on hover. Tests
/// find it as `task title <id>`.
fn title(row: &Row, on_open: OnOpen, error: Option<SharedString>, cx: &App) -> AnyElement {
    let id = row.id;
    let s = cx.theme().semantic();
    let title = div()
        .id(SharedString::from(format!("task-title-{id}")))
        .debug_selector(move || format!("task title {id}"))
        .min_w_0()
        .cursor_pointer()
        .text_size(font_size::SM)
        .text_color(s.foreground)
        .on_click(on_open)
        .child(row.title.clone());
    let Some(error) = error else {
        if !row.unreachable {
            return title.into_any_element();
        }
        // Penpot "Inbox · unreachable PR": the tooltip above the title.
        return Tooltip::new(SharedString::from(format!("unreachable-{id}")))
            .trigger(div().min_w_0().child(title))
            .content(TooltipContent::new(UNREACHABLE).flex_col())
            .into_any_element();
    };
    // Penpot "Inbox · partial failure": the mark `space.1-5` after the title, the tooltip
    // over both.
    let marked = div()
        .flex()
        .items_center()
        .gap(space::S1_5)
        .min_w_0()
        .child(title)
        .child(close_error(row, cx));
    div()
        .min_w_0()
        .debug_selector(move || format!("close error {id}"))
        .child(
            Tooltip::new(SharedString::from(format!("close-error-{id}")))
                .trigger(marked)
                // `TooltipContent` is a row, where GPUI's text can't shrink below its
                // one-line width, so a reason longer than its `max-w-xs` (320px) ran past
                // the edge unwrapped. As a column, the text takes the surface's width and
                // wraps inside it.
                .content(TooltipContent::new(error).flex_col()),
        )
        .into_any_element()
}

/// The mark on a row whose PR didn't close: a 14px lucide `CircleExclamationPoint` in
/// `destructive`, the failing checks' red. Tests find it as `close error mark <id>`.
fn close_error(row: &Row, cx: &App) -> Div {
    let id = row.id;
    close_error_mark(format!("close error mark {id}"), cx)
}

/// [`close_error`]'s mark for any row, which tests find as `selector`.
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

/// Penpot `TaskRow`'s kind icon and its colour (spec §11, PR state): a plain task's
/// `ListTodo` in `foreground`; a PR task's PR state as GitHub shows it. Open is
/// `GitPullRequest` in `foreground`, or `GitPullRequestDraft` in `muted-foreground` while a
/// draft; merged is `GitMerge` in `merged`; closed, draft or not, is `GitPullRequestClosed` in
/// `destructive`. A PR task the sync hasn't filled in yet shows as open.
fn kind_icon(kind: TaskKind, pr: Option<&PrCell>, cx: &App) -> (IconName, Hsla) {
    let s = cx.theme().semantic();
    match (kind, pr) {
        (TaskKind::Plain, _) => (IconName::ListTodo, s.foreground),
        (TaskKind::Pr, None) => (IconName::GitPullRequest, s.foreground),
        (TaskKind::Pr, Some(pr)) => match (pr.state, pr.draft) {
            (PrState::Open, false) => (IconName::GitPullRequest, s.foreground),
            (PrState::Open, true) => (IconName::GitPullRequestDraft, s.muted_foreground),
            (PrState::Merged, _) => (IconName::GitMerge, s.merged),
            (PrState::Closed, _) => (IconName::GitPullRequestClosed, s.destructive),
        },
    }
}

/// Penpot `TaskRow`'s `Unseen` layer, while an agent's comments on the task are unseen: a
/// 14px lucide `MessageSquareDot` in `foreground`. A speech bubble, so it can't be read as
/// a state mark in either mode. Tests find it as `unseen <id>`.
fn unseen_mark(row: &Row, cx: &App) -> Option<Div> {
    let id = row.id;
    row.unseen.then(|| {
        div()
            .debug_selector(move || format!("unseen {id}"))
            .flex_none()
            .flex()
            .child(
                Icon::new(IconName::MessageSquareDot)
                    .size(px(14.))
                    .text_color(cx.theme().semantic().foreground),
            )
    })
}

/// Penpot `TaskRow` (`wide`): its `Select` layer in Select mode, picker, kind icon, title,
/// agent, PR, draft, checks, review, workspace and its `Menu` layer, `space.3` apart, padded
/// `space.2`; `layout`'s columns only. `picker` is the row's status picker; clicking the
/// title runs `on_open`. The title cell ends with the [`unseen_mark`] while the task's agent
/// comments are unseen. Tests find a cell as `<column> <id>`.
pub fn wide(
    row: &Row,
    picker: Select,
    layout: Layout,
    parts: RowParts,
    on_open: OnOpen,
    cx: &App,
) -> AnyElement {
    let s = cx.theme().semantic();
    let muted = |text: SharedString, size: gpui::Rems| {
        div()
            .min_w_0()
            .truncate()
            .text_size(size)
            .text_color(s.muted_foreground)
            .child(text)
    };
    let (icon, icon_color) = kind_icon(row.kind, row.pr.as_ref(), cx);
    let id = row.id;
    let agent_cell = cell(Columns::AGENT).debug_selector(move || format!("agent {id}"));
    let agent = match &row.agent {
        Some(agent) => agent_cell
            .child(state_dot::mark(state_dot::of(agent.state, None)))
            .child(muted(agent.name.clone(), font_size::XS)),
        None => agent_cell.child(none(cx)),
    };
    let pr = row.pr.as_ref();
    div()
        .debug_selector(move || format!("task {id}"))
        .flex()
        .flex_none()
        .items_center()
        .gap(space::S3)
        .p(space::S2)
        .rounded(s.radius_md)
        .opacity(opacity(row))
        .children(select_slot(layout, parts.select))
        .child(cell(Columns::STATUS).child(picker))
        .child(
            cell(Columns::ICON)
                .justify_center()
                .child(Icon::new(icon).size(px(14.)).text_color(icon_color)),
        )
        .child(
            div()
                .flex_1()
                .min_w(Columns::TITLE_MIN)
                .flex()
                .items_center()
                .gap(space::S1_5)
                .child(title(row, on_open, parts.error, cx))
                .children(unseen_mark(row, cx)),
        )
        .when(layout.agent, |r| r.child(agent))
        .child(cell(Columns::PR).when_some(pr, |c, pr| {
            c.child(muted(pr.reference.clone(), font_size::SM))
        }))
        .child(
            cell(Columns::DRAFT).when(pr.is_some_and(|pr| pr.draft), |c| {
                c.child(Badge::new().variant(BadgeVariant::Outline).label("draft"))
            }),
        )
        .when(layout.checks, |r| {
            r.child(
                cell(Columns::CHECKS)
                    .debug_selector(move || format!("checks {id}"))
                    .when_some(pr.and_then(|pr| pr.checks), |c, state| {
                        c.child(checks(state))
                    }),
            )
        })
        .when(layout.review, |r| {
            r.child(
                cell(Columns::REVIEW)
                    .debug_selector(move || format!("review {id}"))
                    .when_some(pr.and_then(|pr| pr.review), |c, state| {
                        c.child(review(state, cx))
                    }),
            )
        })
        .when(layout.workspace, |r| {
            r.child(match &row.workspace {
                Some(name) => cell(Columns::WORKSPACE)
                    .debug_selector(move || format!("workspace {id}"))
                    .child(
                        Icon::new(IconName::Folder)
                            .size(px(14.))
                            .text_color(s.foreground),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(font_size::SM)
                            .text_color(s.foreground)
                            .child(name.clone()),
                    ),
                None => cell(Columns::WORKSPACE)
                    .debug_selector(move || format!("workspace {id}"))
                    .child(none(cx)),
            })
        })
        .child(cell(Columns::MENU).child(parts.menu))
        .into_any_element()
}

/// Penpot `TaskRow` (`panel`), for the right panel's narrow column: its `Select` layer in
/// Select mode, the kind icon, the title over the PR reference, the agents' `StateDot` and
/// its `Menu` layer on top; the status picker, then the checks icon and the review badge
/// below. Padded `space.2`, the two lines `space.1-5` apart. Clicking the title runs
/// `on_open`. The [`unseen_mark`] sits just before the agents' mark while the task's agent
/// comments are unseen. Tests find the agents' mark as `panel agent <id>`.
pub fn panel(row: &Row, picker: Select, parts: RowParts, on_open: OnOpen, cx: &App) -> AnyElement {
    let s = cx.theme().semantic();
    let (icon, icon_color) = kind_icon(row.kind, row.pr.as_ref(), cx);
    let pr = row.pr.as_ref();
    let id = row.id;
    div()
        .debug_selector(move || format!("panel task {id}"))
        .flex()
        .flex_col()
        .flex_none()
        .gap(space::S1_5)
        .p(space::S2)
        .rounded(s.radius_md)
        .opacity(opacity(row))
        // Penpot `Top`: select, icon, Text, unseen, agent, menu, `space.2` apart.
        .child(
            div()
                .flex()
                .items_start()
                .gap(space::S2)
                .children(parts.select.slot())
                .child(
                    div()
                        .flex_none()
                        .pt(px(1.5))
                        .child(Icon::new(icon).size(px(14.)).text_color(icon_color)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(space::S0_5)
                        .child(title(row, on_open, parts.error, cx))
                        .when_some(pr, |text, pr| {
                            text.child(
                                div()
                                    .truncate()
                                    .text_size(font_size::XS)
                                    .text_color(s.muted_foreground)
                                    .child(pr.reference.clone()),
                            )
                        }),
                )
                .when_some(unseen_mark(row, cx), |top, mark| {
                    top.child(div().flex_none().pt(px(1.5)).child(mark))
                })
                .when_some(row.agent.as_ref(), |top, agent| {
                    top.child(
                        div()
                            .debug_selector(move || format!("panel agent {id}"))
                            .flex_none()
                            .child(state_dot::mark(state_dot::of(agent.state, None))),
                    )
                })
                .child(parts.menu),
        )
        // Penpot `Meta`: picker, spacer, checks, review, `space.2` apart.
        .child(
            div()
                .flex()
                .items_center()
                .gap(space::S2)
                .child(picker)
                .child(div().flex_1())
                .when_some(pr.and_then(|pr| pr.checks), |meta, state| {
                    meta.child(checks_icon(state))
                })
                .when_some(pr.and_then(|pr| pr.review), |meta, state| {
                    meta.child(review(state, cx))
                }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    mod layout {
        use super::*;

        #[test]
        fn should_fit_every_column_the_title_at_its_least_the_menu_and_the_gaps() {
            // Penpot's Tasks board: 1131 wide with a 231px title, so 1060 with a 160px one.
            assert_eq!(Table::Tasks.widest().min_width(), px(1060.));
            // Penpot's Inbox board: 1131 wide with a 491px title.
            assert_eq!(Table::Inbox.widest().min_width(), px(1131. - 491. + 160.));
        }

        #[test]
        fn should_make_room_for_the_checkboxes_in_select_mode() {
            let select = Table::Tasks.fit(px(2000.), true);
            assert!(select.select);
            assert_eq!(select.min_width(), px(1060. + 16. + 12.));
        }

        #[test]
        fn should_show_every_column_in_a_wide_window() {
            let layout = Table::Tasks.fit(px(1060.), false);
            assert_eq!(layout, Table::Tasks.widest());
            assert!(layout.agent && layout.checks && layout.review && layout.workspace);
        }

        #[test]
        fn should_drop_the_agent_column_first_when_the_table_does_not_fit() {
            let layout = Table::Tasks.fit(px(1059.), false);
            assert!(!layout.agent);
            assert!(layout.checks);
            assert_eq!(layout.min_width(), px(952.));
        }

        #[test]
        fn should_drop_checks_after_agent() {
            let layout = Table::Tasks.fit(px(951.), false);
            assert!(!layout.agent && !layout.checks);
            assert_eq!(layout.min_width(), px(860.));
        }

        #[test]
        fn should_keep_review_and_workspace() {
            let layout = Table::Tasks.fit(px(400.), false);
            assert!(layout.review && layout.workspace);
            assert_eq!(layout.min_width(), px(860.));
        }

        #[test]
        fn should_fit_the_tasks_table_beside_the_sidebar_at_1200_in_select_mode_too() {
            // 1200 less the sidebar (261) and the content's padding (48).
            for select in [false, true] {
                assert!(Table::Tasks.fit(px(891.), select).min_width() <= px(891.));
            }
        }

        #[test]
        fn should_drop_checks_then_review_in_the_inbox() {
            let inbox = Table::Inbox;
            assert!(!inbox.widest().agent && !inbox.widest().workspace);
            let narrower = inbox.fit(px(799.), false);
            assert!(!narrower.checks && narrower.review);
            let narrowest = inbox.fit(px(707.), false);
            assert!(!narrowest.checks && !narrowest.review);
            assert_eq!(narrowest.min_width(), px(568.));
        }

        #[test]
        fn should_show_every_inbox_column_at_1200_in_select_mode_too() {
            for select in [false, true] {
                let layout = Table::Inbox.fit(px(891.), select);
                assert!(layout.checks && layout.review, "{layout:?}");
            }
        }
    }

    mod opacity {
        use crate::overview::Snapshot;
        use crate::overview::fixtures::{pr_task, unreachable_pr_task};
        use crate::task_list::{Query, Scope, rows};

        use super::*;

        #[test]
        fn should_dim_only_a_row_whose_pr_the_sign_in_cannot_see_to_half() {
            let snapshot = Snapshot {
                tasks: vec![
                    unreachable_pr_task(1, "acme/secret", None),
                    pr_task(2, "acme/widgets", PrState::Open, None),
                ],
                ..Snapshot::default()
            };
            let query = Query {
                scope: Scope::All,
                filter: "",
                show_done: false,
            };

            let opacities: Vec<f32> = rows(&snapshot, &query).iter().map(opacity).collect();

            assert_eq!(opacities, [0.5, 1.]);
        }
    }

    mod kind_icon {
        use brrritscold_core_gpui::{Theme, ThemePreference};
        use brrritscold_core_gpui_tokens::Semantic;
        use gpui::TestAppContext;

        use super::*;

        fn pr(state: PrState, draft: bool) -> PrCell {
            PrCell {
                repo: "widgets".into(),
                reference: "widgets #1".into(),
                full_reference: Some("acme/widgets#1".parse().unwrap()),
                state,
                draft,
                checks: None,
                review: None,
                url: "https://github.com/acme/widgets/pull/1".into(),
            }
        }

        #[gpui::test]
        fn should_show_the_prs_state_as_github_does_in_both_modes(cx: &mut TestAppContext) {
            cx.update(brrritscold_core_gpui::init);
            for (preference, s) in [
                (ThemePreference::Light, Semantic::LIGHT),
                (ThemePreference::Dark, Semantic::DARK),
            ] {
                cx.update(|cx| Theme::set_preference(preference, cx));
                let cases = [
                    (
                        "open",
                        Some(pr(PrState::Open, false)),
                        (IconName::GitPullRequest, s.foreground),
                    ),
                    (
                        "open draft",
                        Some(pr(PrState::Open, true)),
                        (IconName::GitPullRequestDraft, s.muted_foreground),
                    ),
                    (
                        "merged",
                        Some(pr(PrState::Merged, false)),
                        (IconName::GitMerge, s.merged),
                    ),
                    (
                        "closed",
                        Some(pr(PrState::Closed, false)),
                        (IconName::GitPullRequestClosed, s.destructive),
                    ),
                    // GitHub shows a closed draft as closed.
                    (
                        "closed draft",
                        Some(pr(PrState::Closed, true)),
                        (IconName::GitPullRequestClosed, s.destructive),
                    ),
                    // Not synced yet.
                    ("unknown", None, (IconName::GitPullRequest, s.foreground)),
                ];
                for (case, pr, want) in cases {
                    let got = cx.read(|cx| kind_icon(TaskKind::Pr, pr.as_ref(), cx));
                    assert_eq!(got, want, "{case} in {preference:?}");
                }
            }
        }

        #[gpui::test]
        fn should_keep_a_plain_tasks_icon(cx: &mut TestAppContext) {
            cx.update(brrritscold_core_gpui::init);
            cx.update(|cx| Theme::set_preference(ThemePreference::Light, cx));
            let got = cx.read(|cx| kind_icon(TaskKind::Plain, None, cx));
            assert_eq!(got, (IconName::ListTodo, Semantic::LIGHT.foreground));
        }
    }

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

        #[gpui::test]
        fn should_hold_the_panels_running_checks_still(cx: &mut TestAppContext) {
            let (row, cx) = host(|_| checks_icon(ChecksState::Pending).into_any_element(), cx);
            assert_eq!(redraws_in_one_second(&row, cx), 0);
        }
    }
}
