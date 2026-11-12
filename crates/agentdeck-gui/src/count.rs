//! The Penpot part `agentdeck / Count`: a 12px icon and a number, as the sidebar shows unread
//! notes (`StickyNote`) and open PRs (`GitPullRequest`).

use brrritscold_core_gpui::{ActiveTheme as _, Icon, IconName};
use brrritscold_core_gpui_tokens::primitive::{font_size, space};
use gpui::{App, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div};

/// An icon and a count.
#[derive(IntoElement)]
pub struct Count {
    icon: IconName,
    value: usize,
}

impl Count {
    /// `value` beside `icon`.
    pub fn new(icon: IconName, value: usize) -> Self {
        Count { icon, value }
    }
}

impl RenderOnce for Count {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let s = cx.theme().semantic();
        div()
            .flex()
            .items_center()
            .gap(space::S1)
            .text_size(font_size::XS)
            .text_color(s.muted_foreground)
            // The icon keeps the library icon's `foreground`, as the part does.
            .child(
                Icon::new(self.icon)
                    .size(space::S3)
                    .text_color(s.foreground),
            )
            .child(self.value.to_string())
    }
}
