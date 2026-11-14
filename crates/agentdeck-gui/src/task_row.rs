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
    Styled as _, Window, canvas, div, px,
};

use crate::state_dot::{StateDot, StateDotState};
use crate::task_list::Row;

/// How far past the viewport a list draws, so a short scroll shows rows at once.
const OVERDRAW: Pixels = px(200.);

/// The state of a list of rows, empty until [`relist`]ed.
pub fn list_state() -> ListState {
    ListState::new(0, ListAlignment::Top, OVERDRAW)
}

/// Columns over `rows`, `list`'s rows, as wide as the area allows and never narrower than
/// `layout`: a narrower area scrolls them sideways together, by a sideways wheel only. The
/// list's scrollbar sits over the area's right edge, outside that scroll, so it stays in
/// view at any width; a hidden copy of the columns keeps it level with the list.
///
/// Each frame tells `on_width` the area's width, so the view can [`Table::fit`] its layout
/// to it for the next.
pub fn table(
    id: &'static str,
    selector: &'static str,
    layout: Layout,
    rows: impl IntoElement,
    list: &ListState,
    on_width: impl Fn(&Pixels, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
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
                .min_w(layout.min_width())
                .flex()
                .flex_col()
                .gap(space::S4)
                .child(columns(layout, cx))
                .child(rows),
        );
    // As wide as the area, not the table: the copy of the columns overflows it, unseen.
    let scrollbar = div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .flex()
        .flex_col()
        .gap(space::S4)
        .child(columns(layout, cx).min_w(layout.min_width()).invisible())
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
    /// The title's least width. Below it a column drops ([`Table::fit`]), and below the
    /// narrowest layout the table scrolls sideways, so a narrow window never squeezes the
    /// title to nothing. Penpot "Tasks · 1200" fits beside the sidebar with it.
    pub const TITLE_MIN: Pixels = px(200.);
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
    /// Its layouts, widest first: each drops one more column, in the order the table gives
    /// them up when it doesn't fit. Tasks drops Agent, then Checks; Review and Workspace
    /// stay. The Inbox, which has neither Agent nor Workspace, drops Checks, then Review.
    fn layouts(self) -> [Layout; 3] {
        let layout = |agent, checks, review| Layout {
            table: self,
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

    /// Every column the table has.
    pub fn widest(self) -> Layout {
        self.layouts()[0]
    }

    /// The widest layout that fits `width`; below the narrowest, the narrowest, which then
    /// scrolls sideways.
    pub fn fit(self, width: Pixels) -> Layout {
        let layouts = self.layouts();
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
        ];
        let optional = [
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

/// Penpot `Columns`: the column names over the rows, xs medium muted; `layout`'s columns
/// only. Tests find a named column as `column <name>`.
pub fn columns(layout: Layout, cx: &App) -> Div {
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
pub fn checks(state: ChecksState, icon_only: bool, cx: &App) -> Div {
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

/// What clicking a row's title does: open the task sheet on it.
pub type OnOpen = Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static>;

/// A row's title, sm: clicking it runs `on_open`. Tests find it as `task title <id>`.
fn title(row: &Row, on_open: OnOpen, cx: &App) -> gpui::Stateful<Div> {
    let id = row.id;
    div()
        .id(SharedString::from(format!("task-title-{id}")))
        .debug_selector(move || format!("task title {id}"))
        .min_w_0()
        .cursor_pointer()
        .text_size(font_size::SM)
        .text_color(cx.theme().semantic().foreground)
        .on_click(on_open)
        .child(row.title.clone())
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

/// Penpot `TaskRow` (`wide`): picker, kind icon, title, agent, PR, draft, checks, review and
/// workspace, `space.3` apart, padded `space.2`; `layout`'s columns only. `picker` is the
/// row's status picker; clicking the title runs `on_open`. The title cell ends with the
/// [`unseen_mark`] while the task's agent comments are unseen. Tests find a cell as
/// `<column> <id>`.
pub fn wide(row: &Row, picker: Select, layout: Layout, on_open: OnOpen, cx: &App) -> AnyElement {
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
    let id = row.id;
    let agent_cell = cell(Columns::AGENT).debug_selector(move || format!("agent {id}"));
    let agent = match &row.agent {
        Some(agent) => agent_cell
            .child(StateDot::new().state(StateDotState::of(agent.state, None)))
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
                .flex()
                .items_center()
                .gap(space::S1_5)
                .child(title(row, on_open, cx))
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
                        c.child(checks(state, false, cx))
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
        .into_any_element()
}

/// Penpot `TaskRow` (`panel`), for the right panel's narrow column: the kind icon, the title
/// over the PR reference and the agents' `StateDot` on top; the status picker, then the
/// checks icon and the review badge below. Padded `space.2`, the two lines `space.1-5` apart.
/// Clicking the title runs `on_open`. The [`unseen_mark`] sits just before the agents' mark
/// while the task's agent comments are unseen. Tests find the agents' mark as
/// `panel agent <id>`.
pub fn panel(row: &Row, picker: Select, on_open: OnOpen, cx: &App) -> AnyElement {
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
        // Penpot `Top`: icon, Text, unseen, agent, `space.2` apart.
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
                        .child(title(row, on_open, cx))
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
                            .child(StateDot::new().state(StateDotState::of(agent.state, None))),
                    )
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

    mod layout {
        use super::*;

        #[test]
        fn should_fit_every_column_the_title_at_its_least_and_the_gaps() {
            // Penpot's Tasks board: 1131 wide with a 267px title, so 1064 with a 200px one.
            assert_eq!(Table::Tasks.widest().min_width(), px(1064.));
            // Penpot's Inbox board: 1131 wide with a 527px title.
            assert_eq!(Table::Inbox.widest().min_width(), px(1131. - 527. + 200.));
        }

        #[test]
        fn should_show_every_column_in_a_wide_window() {
            let layout = Table::Tasks.fit(px(1064.));
            assert_eq!(layout, Table::Tasks.widest());
            assert!(layout.agent && layout.checks && layout.review && layout.workspace);
        }

        #[test]
        fn should_drop_the_agent_column_first_when_the_table_does_not_fit() {
            let layout = Table::Tasks.fit(px(1063.));
            assert!(!layout.agent);
            assert!(layout.checks);
            assert_eq!(layout.min_width(), px(956.));
        }

        #[test]
        fn should_drop_checks_after_agent() {
            let layout = Table::Tasks.fit(px(955.));
            assert!(!layout.agent && !layout.checks);
            assert_eq!(layout.min_width(), px(864.));
        }

        #[test]
        fn should_keep_review_and_workspace() {
            let layout = Table::Tasks.fit(px(400.));
            assert!(layout.review && layout.workspace);
            assert_eq!(layout.min_width(), px(864.));
        }

        #[test]
        fn should_fit_the_tasks_table_beside_the_sidebar_at_1200() {
            // 1200 less the sidebar (261) and the content's padding (48).
            assert!(Table::Tasks.fit(px(891.)).min_width() <= px(891.));
        }

        #[test]
        fn should_drop_checks_then_review_in_the_inbox() {
            let inbox = Table::Inbox;
            assert!(!inbox.widest().agent && !inbox.widest().workspace);
            let narrower = inbox.fit(px(803.));
            assert!(!narrower.checks && narrower.review);
            let narrowest = inbox.fit(px(711.));
            assert!(!narrowest.checks && !narrowest.review);
            assert_eq!(narrowest.min_width(), px(572.));
        }

        #[test]
        fn should_show_every_inbox_column_at_1200() {
            assert_eq!(Table::Inbox.fit(px(891.)), Table::Inbox.widest());
        }
    }
}
