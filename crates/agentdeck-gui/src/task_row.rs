//! Penpot's page parts for task lists: `TaskRow` (its `wide` layout), `GroupHeader` and
//! `Checks`, drawn from a [`task_list::Row`](crate::task_list::Row). The Tasks view and the
//! Inbox lay them out, each as its [`Table`]; their column widths are [`Columns`]'s.
//!
//! Both lists draw only the items in view, through GPUI's [`list`](gpui::list): an inbox holds
//! hundreds of PRs, and drawing every row made each frame take hundreds of milliseconds.

use agentdeck_core::{ChecksState, ReviewState, TaskKind, TaskStatus};
use brrritscold_core_gpui::{
    ActiveTheme as _, Badge, BadgeVariant, Icon, IconName, Select, SelectContent, SelectItem,
    SelectTrigger, SelectTriggerSize,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Div, ElementId, Hsla, InteractiveElement as _, IntoElement, ListAlignment,
    ListState, ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};

use crate::state_dot::{StateDot, StateDotState};
use crate::task_list::Row;

/// How far past the viewport a list draws, so a short scroll shows rows at once.
const OVERDRAW: Pixels = px(200.);

/// The state of a list of rows, empty until [`relist`]ed.
pub fn list_state() -> ListState {
    ListState::new(0, ListAlignment::Top, OVERDRAW)
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
    /// The title's least width. Below it the table scrolls sideways instead: Penpot's boards
    /// are 1440 wide, and a narrower window would squeeze the title to nothing.
    pub const TITLE_MIN: Pixels = px(240.);
}

/// Which list a table is: the Inbox's hides the agent and workspace columns and names the
/// others its own way, as Penpot's "Inbox" board does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Table {
    /// The Tasks view.
    Tasks,
    /// The Inbox.
    Inbox,
}

impl Table {
    /// Whether rows show the agent and workspace cells.
    fn full(self) -> bool {
        self == Table::Tasks
    }

    /// The table's least width: every column it shows, the title at its least, the
    /// `space.3` gaps and the rows' `space.2` padding.
    pub fn min_width(self) -> Pixels {
        let fixed = [
            Columns::STATUS,
            Columns::ICON,
            Columns::TITLE_MIN,
            Columns::PR,
            Columns::DRAFT,
            Columns::CHECKS,
            Columns::REVIEW,
        ];
        let extra = [Columns::AGENT, Columns::WORKSPACE];
        let shown = extra.iter().filter(|_| self.full());
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
fn cell(width: Pixels) -> Div {
    div()
        .flex_none()
        .w(width)
        .min_w_0()
        .flex()
        .items_center()
        .gap(space::S1_5)
}

/// `—`, for a cell with nothing to show.
fn none(cx: &App) -> Div {
    div()
        .text_size(font_size::XS)
        .text_color(cx.theme().semantic().muted_foreground)
        .child("—")
}

/// Penpot `Columns`: the column names over the rows, xs medium muted.
pub fn columns(table: Table, cx: &App) -> Div {
    let s = cx.theme().semantic();
    let name = |width: Option<Pixels>, text: &'static str| {
        let cell = match width {
            Some(width) => cell(width),
            None => div().flex_1().min_w(Columns::TITLE_MIN),
        };
        cell.child(text)
    };
    div()
        .flex()
        .flex_none()
        .gap(space::S3)
        .px(space::S2)
        .text_size(font_size::XS)
        .font_weight(font_weight::MEDIUM)
        .text_color(s.muted_foreground)
        .child(name(Some(Columns::STATUS), "Status"))
        .child(name(Some(Columns::ICON), ""))
        .map(|row| match table {
            Table::Tasks => row
                .child(name(None, "Task"))
                .child(name(Some(Columns::AGENT), "Agent"))
                .child(name(Some(Columns::PR), "PR")),
            Table::Inbox => row
                .child(name(None, "Title"))
                .child(name(Some(Columns::PR), "Repository")),
        })
        .child(name(Some(Columns::DRAFT), ""))
        .child(name(Some(Columns::CHECKS), "Checks"))
        .child(name(Some(Columns::REVIEW), "Review"))
        .when(table.full(), |row| {
            row.child(name(Some(Columns::WORKSPACE), "Workspace"))
        })
}

/// Penpot `GroupHeader`: a chevron, `label` and a `secondary` count badge. Clicking it runs
/// `on_toggle`. `key` names it: its id is `group-<key>`, and tests find it as `group <key>`.
pub fn group_header(
    key: impl Into<SharedString>,
    label: impl Into<SharedString>,
    count: usize,
    collapsed: bool,
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

/// Penpot `Checks`: an icon and, unless `icon_only` (the panel's), a word, in the checks
/// colour (spec §11).
fn checks(state: ChecksState, icon_only: bool, cx: &App) -> Div {
    let s = cx.theme().semantic();
    let (icon, label, color): (IconName, &str, Hsla) = match state {
        ChecksState::Success => (IconName::CircleCheck, "passing", s.success),
        ChecksState::Pending => (IconName::LoaderCircle, "running", s.info),
        ChecksState::Failure => (IconName::OctagonX, "failing", s.destructive),
    };
    div()
        .flex()
        .items_center()
        .gap(space::S1)
        .text_size(font_size::XS)
        .text_color(color)
        .child(Icon::new(icon).size(px(14.)).text_color(color))
        .when(!icon_only, |c| c.child(label))
}

/// The review cell: a badge in the review colour (spec §11), or muted text while one is
/// still required.
fn review(state: ReviewState, cx: &App) -> AnyElement {
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

/// The status picker: core-gpui's `Select` at `sm`, listing every status. Choosing one runs
/// `on_change` with it.
pub fn status_picker(
    id: impl Into<ElementId>,
    status: TaskStatus,
    on_change: impl Fn(&TaskStatus, &mut Window, &mut App) + 'static,
) -> Select {
    let content = TaskStatus::ALL
        .iter()
        .fold(SelectContent::new(), |content, s| {
            content.item(SelectItem::new(s.as_str(), status_label(*s)))
        });
    Select::new(id)
        // Penpot's `Status` trigger is 95px whatever the status, so the column lines up.
        .trigger(SelectTrigger::new().size(SelectTriggerSize::Sm).w(px(95.)))
        .content(content)
        .value(Some(status.as_str()))
        .on_value_change(move |value, window, cx| {
            if let Ok(status) = value.parse() {
                on_change(&status, window, cx);
            }
        })
}

/// Penpot `TaskRow` (`wide`): picker, kind icon, title, agent, PR, draft, checks, review and
/// workspace, `space.3` apart, padded `space.2`; `table`'s columns only. `picker` is the
/// row's status picker.
pub fn wide(row: &Row, picker: Select, table: Table, cx: &App) -> AnyElement {
    let s = cx.theme().semantic();
    let muted = |text: SharedString, size: gpui::Rems| {
        div()
            .min_w_0()
            .truncate()
            .text_size(size)
            .text_color(s.muted_foreground)
            .child(text)
    };
    let kind_icon = match row.kind {
        TaskKind::Plain => IconName::ListTodo,
        TaskKind::Pr => IconName::GitPullRequest,
    };
    let agent = match &row.agent {
        Some(agent) => cell(Columns::AGENT)
            .child(StateDot::new().state(StateDotState::of(agent.state, None)))
            .child(muted(agent.name.clone(), font_size::XS)),
        None => cell(Columns::AGENT).child(none(cx)),
    };
    let pr = row.pr.as_ref();
    let id = row.id;
    div()
        .debug_selector(move || format!("task {id}"))
        .flex()
        .flex_none()
        .items_center()
        .gap(space::S3)
        .p(space::S2)
        .rounded(s.radius_md)
        .child(cell(Columns::STATUS).child(picker))
        .child(
            cell(Columns::ICON)
                .justify_center()
                .child(Icon::new(kind_icon).size(px(14.)).text_color(s.foreground)),
        )
        .child(
            div()
                .flex_1()
                .min_w(Columns::TITLE_MIN)
                .text_size(font_size::SM)
                .text_color(s.foreground)
                .child(row.title.clone()),
        )
        .when(table.full(), |r| r.child(agent))
        .child(cell(Columns::PR).when_some(pr, |c, pr| {
            c.child(muted(pr.reference.clone(), font_size::SM))
        }))
        .child(
            cell(Columns::DRAFT).when(pr.is_some_and(|pr| pr.draft), |c| {
                c.child(Badge::new().variant(BadgeVariant::Outline).label("draft"))
            }),
        )
        .child(
            cell(Columns::CHECKS).when_some(pr.and_then(|pr| pr.checks), |c, state| {
                c.child(checks(state, false, cx))
            }),
        )
        .child(
            cell(Columns::REVIEW).when_some(pr.and_then(|pr| pr.review), |c, state| {
                c.child(review(state, cx))
            }),
        )
        .when(table.full(), |r| {
            r.child(match &row.workspace {
                Some(name) => cell(Columns::WORKSPACE)
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
                None => cell(Columns::WORKSPACE).child(none(cx)),
            })
        })
        .into_any_element()
}

/// Penpot `TaskRow` (`panel`), for the right panel's narrow column: the kind icon, the title
/// over the PR reference and the agents' `StateDot` on top; the status picker, then the
/// checks icon and the review badge below. Padded `space.2`, the two lines `space.1-5` apart.
pub fn panel(row: &Row, picker: Select, cx: &App) -> AnyElement {
    let s = cx.theme().semantic();
    let kind_icon = match row.kind {
        TaskKind::Plain => IconName::ListTodo,
        TaskKind::Pr => IconName::GitPullRequest,
    };
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
        // Penpot `Top`: icon, Text, agent, `space.2` apart.
        .child(
            div()
                .flex()
                .items_start()
                .gap(space::S2)
                .child(
                    div()
                        .flex_none()
                        .pt(px(1.5))
                        .child(Icon::new(kind_icon).size(px(14.)).text_color(s.foreground)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(space::S0_5)
                        .child(
                            div()
                                .text_size(font_size::SM)
                                .text_color(s.foreground)
                                .child(row.title.clone()),
                        )
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
                .when_some(row.agent.as_ref(), |top, agent| {
                    top.child(StateDot::new().state(StateDotState::of(agent.state, None)))
                }),
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
                    meta.child(checks(state, true, cx))
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

    mod table {
        use super::*;

        #[test]
        fn should_fit_every_column_the_title_at_its_least_and_the_gaps() {
            // Penpot's Tasks board: 1131 wide with a 267px title, so 1104 with a 240px one.
            assert_eq!(Table::Tasks.min_width(), px(1104.));
            // Penpot's Inbox board: 1131 wide with a 527px title.
            assert_eq!(Table::Inbox.min_width(), px(1131. - 527. + 240.));
        }
    }
}
