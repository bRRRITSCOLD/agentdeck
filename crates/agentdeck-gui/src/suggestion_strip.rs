//! The Suggestions strip on a Workspace's Pull requests (spec §11), Penpot "Screens · Main" →
//! "Workspace · Pull requests · suggestion", "· suggestions" and "Workspace · no repositories ·
//! suggestions": one row per Suggestion, stacked in one strip drawn as core-gpui's default
//! `Alert` box. An app-local composition (Penpot `agentdeck/SuggestionRow`), since an `Alert`
//! holds one title and one action; [`suggestion_row`] draws it, for a pane's hosted agent
//! exited strip too.
//!
//! A row reads "Panes here work in **owner/name**. Link it to this workspace?", then **Link** ·
//! **Not now**. What each does is the view's: this only draws them.

use std::rc::Rc;

use agentdeck_client::types::Repository;
use agentdeck_core::RepositoryId;
use brrritscold_core_gpui::{ActiveTheme as _, Button, ButtonSize, ButtonVariant, Icon, IconName};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
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

/// One Suggestion's row: a `GitBranch`, "Panes here work in **owner/name**. Link it to this
/// workspace?", then Link · Not now.
fn row(repository: &Repository, on_link: OnLink, on_not_now: OnNotNow, cx: &App) -> AnyElement {
    let name = repository.name.to_string();
    let (held, id) = (repository.clone(), repository.id);
    suggestion_row(
        format!("suggestion {name}"),
        IconName::GitBranch,
        Sentence {
            selector: format!("suggestion text {name}"),
            lead: LEAD,
            name: name.clone().into(),
            tail: TAIL,
        },
        [
            RowAction {
                id: format!("link-suggestion-{id}").into(),
                label: "Link",
                selector: format!("link suggestion {name}"),
                on_click: Rc::new(move |window, cx| on_link(held.clone(), window, cx)),
            },
            RowAction {
                id: format!("not-now-{id}").into(),
                label: "Not now",
                selector: format!("not now {name}"),
                on_click: Rc::new(move |window, cx| on_not_now(id, window, cx)),
            },
        ],
        cx,
    )
}

/// A [`suggestion_row`]'s sentence: `lead`, then `name` in medium, then `tail`.
pub struct Sentence {
    /// What tests find the sentence by.
    pub selector: String,
    /// What comes before the name, a space after it; none when empty.
    pub lead: &'static str,
    /// The name, in `font-weight.medium`.
    pub name: SharedString,
    /// What comes after the name, its own leading space included.
    pub tail: &'static str,
}

/// What a click on a [`RowAction`] does.
pub type OnAction = Rc<dyn Fn(&mut Window, &mut App)>;

/// One of a [`suggestion_row`]'s two link buttons.
pub struct RowAction {
    /// The button's element id.
    pub id: SharedString,
    /// Its label.
    pub label: &'static str,
    /// What tests find it by.
    pub selector: String,
    /// What a click does.
    pub on_click: OnAction,
}

/// Penpot `agentdeck/SuggestionRow`: a 16px `icon` in `foreground`, the sentence (sm), then
/// the two actions at the end as link sm buttons with a `muted-foreground` "·" between them,
/// `space.2-5` apart, as an Alert's icon and text; `h-8`. Tests find it by `selector`.
pub fn suggestion_row(
    selector: String,
    icon: IconName,
    sentence: Sentence,
    actions: [RowAction; 2],
    cx: &App,
) -> AnyElement {
    let s = cx.theme().semantic();
    let found = |selector: String| move || selector;
    let button = |action: RowAction| {
        let on_click = action.on_click;
        div().debug_selector(found(action.selector)).child(
            Button::new(action.id)
                .variant(ButtonVariant::Link)
                .size(ButtonSize::Sm)
                .label(action.label)
                .on_click(move |_, window, cx| on_click(window, cx)),
        )
    };
    let [first, second] = actions;
    let Sentence {
        selector: text,
        lead,
        name,
        tail,
    } = sentence;
    div()
        .debug_selector(found(selector))
        .flex()
        .items_center()
        .gap(space::S2_5)
        .h(space::S8)
        .text_size(font_size::SM)
        .child(Icon::new(icon).size(px(16.)).text_color(s.foreground))
        .child(
            div()
                .debug_selector(found(text))
                .min_w_0()
                .flex()
                .whitespace_nowrap()
                .when(!lead.is_empty(), |text| text.child(format!("{lead}\u{a0}")))
                .child(div().font_weight(font_weight::MEDIUM).child(name))
                .child(tail),
        )
        .child(div().flex_1())
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .child(button(first))
                .child(div().text_color(s.muted_foreground).child("·"))
                .child(button(second)),
        )
        .into_any_element()
}
