//! Penpot `Centre / TopBar`: the selected workspace's name and directory, above its tabs
//! (spec §11).
//!
//! Its New agent button opens the new agent dialog and its panel button toggles the right
//! panel, both as actions the window handles.

use std::path::Path;

use brrritscold_core_gpui::{ActiveTheme as _, Button, ButtonSize, ButtonVariant, IconName};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div, px,
};

use crate::keymap::ToggleRightPanel;
use crate::new_agent::NewAgent;

/// The bar's height. Raw in Penpot too: no sizing token is 52px.
const HEIGHT: f32 = 52.;

/// `dir` as the bar shows it: under `home`, with `~` for `home`.
pub fn shown_dir(dir: &str, home: Option<&Path>) -> String {
    let under_home = home.and_then(|home| Path::new(dir).strip_prefix(home).ok());
    match under_home {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => dir.to_owned(),
    }
}

/// The workspace view's top bar.
#[derive(IntoElement, Clone, Debug, PartialEq)]
pub struct TopBar {
    name: SharedString,
    dir: SharedString,
}

impl TopBar {
    /// The bar for the workspace named `name`, whose directory is `dir`.
    pub fn new(name: impl Into<SharedString>, dir: impl Into<SharedString>) -> Self {
        TopBar {
            name: name.into(),
            dir: dir.into(),
        }
    }

    /// The workspace's directory, as given.
    pub fn dir(&self) -> &SharedString {
        &self.dir
    }
}

impl RenderOnce for TopBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let s = cx.theme().semantic();
        let dir = shown_dir(&self.dir, std::env::home_dir().as_deref());
        // Penpot `TopBar`: 52px high, px `space.4`, items centred, `space.3` apart.
        div()
            .flex_none()
            .h(px(HEIGHT))
            .flex()
            .items_center()
            .gap(space::S3)
            .px(space::S4)
            // Penpot `Heading`: Name over Dir, `space.0-5` apart; each truncates.
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(space::S0_5)
                    // Penpot `Name`: sm, medium, `foreground`.
                    .child(
                        div()
                            .truncate()
                            .text_size(font_size::SM)
                            .font_weight(font_weight::MEDIUM)
                            .text_color(s.foreground)
                            .child(self.name),
                    )
                    // Penpot `Dir`: xs, normal, `muted-foreground`.
                    .child(
                        div()
                            .truncate()
                            .text_size(font_size::XS)
                            .font_weight(font_weight::NORMAL)
                            .text_color(s.muted_foreground)
                            .child(dir),
                    ),
            )
            // Penpot `Spacer`, `NewAgent` (outline, sm), then `TogglePanel`: ghost, icon-sm,
            // `PanelRight`.
            .child(div().flex_1())
            .child(
                div().debug_selector(|| "new agent".into()).child(
                    Button::new("new-agent")
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Sm)
                        .label("New agent")
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(NewAgent), cx);
                        }),
                ),
            )
            .child(
                div().debug_selector(|| "toggle panel".into()).child(
                    Button::new("toggle-panel")
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::IconSm)
                        .icon(IconName::PanelRight)
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(ToggleRightPanel), cx);
                        }),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod shown_dir {
        use super::*;

        fn home() -> Option<&'static Path> {
            Some(Path::new("/home/ada"))
        }

        #[test]
        fn should_put_a_tilde_for_the_home_dir() {
            assert_eq!(
                shown_dir("/home/ada/.local/share/agentdeck/workspaces/x", home()),
                "~/.local/share/agentdeck/workspaces/x"
            );
            assert_eq!(shown_dir("/home/ada", home()), "~");
        }

        #[test]
        fn should_keep_a_dir_outside_home_as_it_is() {
            assert_eq!(shown_dir("/srv/x", home()), "/srv/x");
        }

        #[test]
        fn should_not_abbreviate_a_sibling_that_shares_homes_prefix() {
            assert_eq!(shown_dir("/home/adam/x", home()), "/home/adam/x");
        }

        #[test]
        fn should_keep_the_dir_when_home_is_unknown() {
            assert_eq!(shown_dir("/home/ada/x", None), "/home/ada/x");
        }
    }
}
