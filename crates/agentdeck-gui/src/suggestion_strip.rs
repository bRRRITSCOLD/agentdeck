//! The Suggestions strip on a Workspace's Pull requests (spec §11), Penpot "Screens · Main" →
//! "Workspace · Pull requests · suggestion", "· suggestions" and "Workspace · no repositories ·
//! suggestions": one row per Suggestion, stacked in one strip drawn as core-gpui's default
//! `Alert` box. An app-local composition (Penpot `agentdeck/SuggestionRow`), since an `Alert`
//! holds one title and one action.
//!
//! A row reads "Panes here work in **owner/name**. Link it to this workspace?", then **Link** ·
//! **Not now**. What each does is the view's: this only draws them.

use std::rc::Rc;

use agentdeck_client::types::Repository;
use agentdeck_core::RepositoryId;
use brrritscold_core_gpui::{ActiveTheme as _, Button, ButtonSize, ButtonVariant, Icon, IconName};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, px,
};

/// What a row says before the Repository's name.
pub const LEAD: &str = "Panes here work in";

/// What a row says after the Repository's name.
pub const TAIL: &str = ". Link it to this workspace?";

/// What Link does with a row's Repository.
pub type OnLink = Rc<dyn Fn(Repository, &mut Window, &mut App)>;

/// What Not now does with a row's Repository.
pub type OnNotNow = Rc<dyn Fn(RepositoryId, &mut Window, &mut App)>;

/// The strip of `suggestions`, one row each, in order; nothing when there are none. Tests find
/// it as `suggestions`, each row as `suggestion <name>`, its text as `suggestion text <name>`
/// and its buttons as `link suggestion <name>` and `not now <name>`.
pub fn strip(
    suggestions: &[Repository],
    on_link: OnLink,
    on_not_now: OnNotNow,
    cx: &App,
) -> Option<AnyElement> {
    if suggestions.is_empty() {
        return None;
    }
    let s = cx.theme().semantic();
    // Penpot `Suggestions`: the default Alert's box (rounded-lg border bg-card), its rows
    // `space.2` from the top and bottom, as each row's buttons pad themselves.
    let rows = suggestions
        .iter()
        .map(|repository| row(repository, on_link.clone(), on_not_now.clone(), cx));
    Some(
        div()
            .debug_selector(|| "suggestions".into())
            .flex_none()
            .flex()
            .flex_col()
            .px(space::S4)
            .py(space::S2)
            .rounded(s.radius_lg)
            .border_1()
            .border_color(s.border)
            .bg(s.card)
            .text_color(s.card_foreground)
            .children(rows)
            .into_any_element(),
    )
}

/// Penpot `agentdeck/SuggestionRow`: a 16px `GitBranch`, the sentence with the name in
/// medium, then Link · Not now at the end, `space.2-5` apart, as an Alert's icon and text.
fn row(repository: &Repository, on_link: OnLink, on_not_now: OnNotNow, cx: &App) -> AnyElement {
    let s = cx.theme().semantic();
    let name = repository.name.to_string();
    let selector = |what: &str| {
        let selector = format!("{what}{name}");
        move || selector.clone()
    };
    let (held, id) = (repository.clone(), repository.id);
    div()
        .debug_selector(selector("suggestion "))
        .flex()
        .items_center()
        .gap(space::S2_5)
        .h(space::S8)
        .text_size(font_size::SM)
        .child(
            Icon::new(IconName::GitBranch)
                .size(px(16.))
                .text_color(s.foreground),
        )
        .child(
            div()
                .debug_selector(selector("suggestion text "))
                .min_w_0()
                .flex()
                .whitespace_nowrap()
                .child(format!("{LEAD}\u{a0}"))
                .child(
                    div()
                        .font_weight(font_weight::MEDIUM)
                        .child(SharedString::from(name.clone())),
                )
                .child(TAIL),
        )
        .child(div().flex_1())
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .child(
                    div().debug_selector(selector("link suggestion ")).child(
                        Button::new(SharedString::from(format!("link-suggestion-{id}")))
                            .variant(ButtonVariant::Link)
                            .size(ButtonSize::Sm)
                            .label("Link")
                            .on_click(move |_, window, cx| on_link(held.clone(), window, cx)),
                    ),
                )
                .child(div().text_color(s.muted_foreground).child("·"))
                .child(
                    div().debug_selector(selector("not now ")).child(
                        Button::new(SharedString::from(format!("not-now-{id}")))
                            .variant(ButtonVariant::Link)
                            .size(ButtonSize::Sm)
                            .label("Not now")
                            .on_click(move |_, window, cx| on_not_now(id, window, cx)),
                    ),
                ),
        )
        .into_any_element()
}
