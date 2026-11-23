//! The Pull requests view's table (spec §11), Penpot "Screens · Main" → "Pull requests": a
//! row per pull request and a header per Repository group. A row is Penpot's `TaskRow`
//! (`wide`) without its status picker: the PR's state icon, its title, its reference, a draft
//! badge, its checks and review, and its `…` menu. Only the reference is a link, to the PR on
//! GitHub. A group header is Penpot's `GroupHeader` followed by who links the Repository:
//! "Not in a workspace" until Links come.
//!
//! A Finished row is drawn as any other: its state icon shows GitHub's state, merged, closed,
//! or open for one past the age cutoff (Penpot "Pull requests · show finished").
//!
//! The fixed column widths are the task tables' [`Columns`], so the PR table lines up with
//! them; it has no column to drop, as it fits the narrowest window whole.

use agentdeck_client::types::PullRequest;
use agentdeck_core::PrState;
use brrritscold_core_gpui::{
    ActiveTheme as _, Badge, BadgeVariant, Icon, IconName, Tooltip, TooltipContent,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, Div, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div, px,
};

use crate::task_row::{self, Columns, SelectCell, UNREACHABLE};

/// What a Repository group's header says in place of the Workspaces linking it, while none
/// does.
pub const NOT_IN_A_WORKSPACE: &str = "Not in a workspace";

/// The table's least width: every column, the title at its least, the `space.3` gaps and the
/// rows' `space.2` padding. Narrower than any window the sidebar leaves room for.
pub fn min_width() -> Pixels {
    let widths = [
        Columns::ICON,
        Columns::TITLE_MIN,
        Columns::PR,
        Columns::DRAFT,
        Columns::CHECKS,
        Columns::REVIEW,
        Columns::MENU,
    ];
    let sum = widths.iter().fold(px(0.), |sum, width| sum + *width);
    sum + space::S3 * (widths.len() as f32 - 1.) + space::S2 * 2.
}

/// Penpot `Columns` as the Pull requests view names them: the icon's slot, Title, PR, the
/// draft badge's slot, Checks and Review, then the `Menu` layer's space. Tests find a named
/// column as `pr column <name>`.
pub fn columns(cx: &App) -> Div {
    let s = cx.theme().semantic();
    let name = |width: Option<Pixels>, text: &'static str| {
        let cell = match width {
            Some(width) => task_row::cell(width),
            None => div().flex_1().min_w(Columns::TITLE_MIN),
        };
        cell.debug_selector(move || format!("pr column {text}"))
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
        .child(name(Some(Columns::ICON), ""))
        .child(name(None, "Title"))
        .child(name(Some(Columns::PR), "PR"))
        .child(name(Some(Columns::DRAFT), ""))
        .child(name(Some(Columns::CHECKS), "Checks"))
        .child(name(Some(Columns::REVIEW), "Review"))
        .child(task_row::cell(Columns::MENU))
}

/// A Repository group's header: Penpot's `GroupHeader` (a chevron, `label` and a count badge
/// of its PRs), which runs `on_toggle` when clicked, then [`NOT_IN_A_WORKSPACE`] in sm
/// `muted-foreground`, `space.2` after it. `key` names it: tests find it as `group <key>`,
/// and its workspaces as `group workspaces <key>`.
pub fn group_header(
    key: SharedString,
    label: SharedString,
    count: usize,
    collapsed: bool,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let s = cx.theme().semantic();
    let workspaces = key.clone();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(space::S2)
        .child(task_row::group_header(
            key,
            label,
            count,
            collapsed,
            SelectCell::Off,
            on_toggle,
            cx,
        ))
        .child(
            div()
                .debug_selector(move || format!("group workspaces {workspaces}"))
                .text_size(font_size::SM)
                .text_color(s.muted_foreground)
                .child(NOT_IN_A_WORKSPACE),
        )
        .into_any_element()
}

/// The PR's state icon and its colour, as task rows show a PR task's (spec §11, PR state):
/// open is `GitPullRequest` in `foreground`, or `GitPullRequestDraft` in `muted-foreground`
/// while a draft; merged is `GitMerge` in `merged`; closed is `GitPullRequestClosed` in
/// `destructive`.
fn state_icon(pr: &PullRequest, cx: &App) -> (IconName, Hsla) {
    let s = cx.theme().semantic();
    match (pr.state, pr.draft) {
        (PrState::Open, false) => (IconName::GitPullRequest, s.foreground),
        (PrState::Open, true) => (IconName::GitPullRequestDraft, s.muted_foreground),
        (PrState::Merged, _) => (IconName::GitMerge, s.merged),
        (PrState::Closed, _) => (IconName::GitPullRequestClosed, s.destructive),
    }
}

/// The row's title, sm `foreground`, wrapping. It is no link: a click on it does nothing. A PR
/// the GitHub sign-in can't see shows [`UNREACHABLE`] above it on hover (Penpot "Pull requests
/// · unreachable PR"). Tests find it as `pr title <reference>`.
fn title(pr: &PullRequest, key: &SharedString, cx: &App) -> AnyElement {
    let selector = key.clone();
    let title = div()
        .id(SharedString::from(format!("pr-title-{key}")))
        .debug_selector(move || format!("pr title {selector}"))
        .min_w_0()
        .text_size(font_size::SM)
        .text_color(cx.theme().semantic().foreground)
        .child(pr.title.clone());
    if !pr.unreachable {
        return title.into_any_element();
    }
    Tooltip::new(SharedString::from(format!("pr-unreachable-{key}")))
        .trigger(div().min_w_0().child(title))
        .content(TooltipContent::new(UNREACHABLE).flex_col())
        .into_any_element()
}

/// The row's reference, `#<number>` in sm `muted-foreground`: the one link on the row,
/// underlined on hover, which runs `on_open`. Tests find it as `pr link <reference>`.
fn reference(
    pr: &PullRequest,
    key: &SharedString,
    on_open: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let selector = key.clone();
    div()
        .id(SharedString::from(format!("pr-link-{key}")))
        .debug_selector(move || format!("pr link {selector}"))
        .min_w_0()
        .truncate()
        .cursor_pointer()
        .text_size(font_size::SM)
        .text_color(cx.theme().semantic().muted_foreground)
        .hover(|style| style.underline())
        .on_click(on_open)
        .child(format!("#{}", pr.number))
        .into_any_element()
}

/// Penpot `TaskRow` (`wide`) as the Pull requests view draws a PR: its state icon, title,
/// reference, draft badge, checks, review and `menu`, `space.3` apart, padded `space.2`, at
/// half opacity while the GitHub sign-in can't see it. A click on the reference runs
/// `on_open`; nothing else on the row reacts to a click but `menu`. Tests find the row as
/// `pr <reference>` and a cell as `<column> <reference>`.
pub fn row(
    pr: &PullRequest,
    menu: AnyElement,
    on_open: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let s = cx.theme().semantic();
    let key = SharedString::from(pr.reference.to_string());
    let (icon, icon_color) = state_icon(pr, cx);
    let selector = key.clone();
    let checks = key.clone();
    let review = key.clone();
    div()
        .debug_selector(move || format!("pr {selector}"))
        .flex()
        .flex_none()
        .items_center()
        .gap(space::S3)
        .p(space::S2)
        .rounded(s.radius_md)
        .when(pr.unreachable, |row| row.opacity(0.5))
        .child(
            task_row::cell(Columns::ICON)
                .justify_center()
                .child(Icon::new(icon).size(px(14.)).text_color(icon_color)),
        )
        .child(
            div()
                .flex_1()
                .min_w(Columns::TITLE_MIN)
                .flex()
                .items_center()
                .child(title(pr, &key, cx)),
        )
        .child(task_row::cell(Columns::PR).child(reference(pr, &key, on_open, cx)))
        .child(task_row::cell(Columns::DRAFT).when(pr.draft, |c| {
            c.child(Badge::new().variant(BadgeVariant::Outline).label("draft"))
        }))
        .child(
            task_row::cell(Columns::CHECKS)
                .debug_selector(move || format!("checks {checks}"))
                .when_some(pr.checks, |c, state| c.child(task_row::checks(state))),
        )
        .child(
            task_row::cell(Columns::REVIEW)
                .debug_selector(move || format!("review {review}"))
                .when_some(pr.review, |c, state| c.child(task_row::review(state, cx))),
        )
        .child(task_row::cell(Columns::MENU).child(menu))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;
    use crate::overview::fixtures::pull_request;

    #[gpui::test]
    fn should_show_githubs_state_on_finished_rows(cx: &mut TestAppContext) {
        cx.update(brrritscold_core_gpui::init);
        let finished = |state| PullRequest {
            state,
            finished: true,
            ..pull_request(1, "acme/widgets")
        };

        cx.update(|cx| {
            let s = cx.theme().semantic();
            assert_eq!(
                state_icon(&finished(PrState::Merged), cx),
                (IconName::GitMerge, s.merged)
            );
            assert_eq!(
                state_icon(&finished(PrState::Closed), cx),
                (IconName::GitPullRequestClosed, s.destructive)
            );
            assert_eq!(
                state_icon(&finished(PrState::Open), cx),
                (IconName::GitPullRequest, s.foreground)
            );
        });
    }

    #[test]
    fn should_fit_beside_the_sidebar_at_the_narrowest_window() {
        let (window, _) = crate::window_state::MIN_SIZE;
        // The sidebar and its separator, and the content's `space.6` padding either side.
        let room = px(window) - crate::sidebar::WIDTH - px(1.) - space::S6 * 2.;

        assert!(min_width() <= room, "{:?} in {room:?}", min_width());
    }
}
