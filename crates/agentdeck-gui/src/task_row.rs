//! Penpot's page parts for task lists: `TaskRow` (its `wide` layout), `GroupHeader` and
//! `Checks`, drawn from a [`task_list::Row`](crate::task_list::Row). The Tasks view lays
//! them out; their column widths are [`Columns`]'s.

use agentdeck_core::{ChecksState, ReviewState, TaskKind, TaskStatus};
use brrritscold_core_gpui::{
    ActiveTheme as _, Badge, BadgeVariant, Icon, IconName, Select, SelectContent, SelectItem,
    SelectTrigger, SelectTriggerSize,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Div, ElementId, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, px,
};

use crate::state_dot::{StateDot, StateDotState};
use crate::task_list::Row;

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
pub fn columns(cx: &App) -> Div {
    let s = cx.theme().semantic();
    let name = |width: Option<Pixels>, text: &'static str| {
        let cell = match width {
            Some(width) => cell(width),
            None => div().flex_1().min_w_0(),
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
        .child(name(None, "Task"))
        .child(name(Some(Columns::AGENT), "Agent"))
        .child(name(Some(Columns::PR), "PR"))
        .child(name(Some(Columns::DRAFT), ""))
        .child(name(Some(Columns::CHECKS), "Checks"))
        .child(name(Some(Columns::REVIEW), "Review"))
        .child(name(Some(Columns::WORKSPACE), "Workspace"))
}

/// Penpot `GroupHeader`: a chevron, the status and a `secondary` count badge. Clicking it
/// runs `on_toggle`.
pub fn group_header(
    status: TaskStatus,
    count: usize,
    collapsed: bool,
    on_toggle: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let s = cx.theme().semantic();
    div()
        .id(SharedString::from(format!("group-{status}")))
        .debug_selector(move || format!("group {status}"))
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
                .child(status_label(status)),
        )
        .child(
            Badge::new()
                .variant(BadgeVariant::Secondary)
                .label(count.to_string()),
        )
        .into_any_element()
}

/// Penpot `Checks`: an icon and a word in the checks colour (spec §11).
fn checks(state: ChecksState, cx: &App) -> Div {
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
        .child(label)
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
    let content = TaskStatus::ALL.iter().fold(SelectContent::new(), |content, s| {
        content.item(SelectItem::new(s.as_str(), status_label(*s)))
    });
    Select::new(id)
        .trigger(SelectTrigger::new().size(SelectTriggerSize::Sm))
        .content(content)
        .value(Some(status.as_str()))
        .on_value_change(move |value, window, cx| {
            if let Ok(status) = value.parse() {
                on_change(&status, window, cx);
            }
        })
}

/// Penpot `TaskRow` (`wide`): picker, kind icon, title, agent, PR, draft, checks, review and
/// workspace, `space.3` apart, padded `space.2`. `picker` is the row's status picker.
pub fn wide(row: &Row, picker: Select, cx: &App) -> AnyElement {
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
                .min_w_0()
                .text_size(font_size::SM)
                .text_color(s.foreground)
                .child(row.title.clone()),
        )
        .child(agent)
        .child(cell(Columns::PR).when_some(pr, |c, pr| {
            c.child(muted(pr.reference.clone(), font_size::SM))
        }))
        .child(cell(Columns::DRAFT).when(pr.is_some_and(|pr| pr.draft), |c| {
            c.child(Badge::new().variant(BadgeVariant::Outline).label("draft"))
        }))
        .child(
            cell(Columns::CHECKS)
                .when_some(pr.and_then(|pr| pr.checks), |c, state| c.child(checks(state, cx))),
        )
        .child(
            cell(Columns::REVIEW)
                .when_some(pr.and_then(|pr| pr.review), |c, state| c.child(review(state, cx))),
        )
        .child(match &row.workspace {
            Some(name) => cell(Columns::WORKSPACE)
                .child(Icon::new(IconName::Folder).size(px(14.)).text_color(s.foreground))
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
        .into_any_element()
}
