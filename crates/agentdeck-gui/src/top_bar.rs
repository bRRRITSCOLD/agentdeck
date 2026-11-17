//! Penpot `Centre / TopBar`: the selected workspace's name and directory, above its tabs
//! (spec §11).
//!
//! Its New agent button opens the new agent dialog, its `…` menu opens the Edit workspace
//! dialog, mutes or unmutes the workspace's notifications, archives it or opens the Delete
//! workspace dialog, and its panel button toggles the right panel, all as actions the window
//! handles. An archived workspace's bar has Unarchive in place of New agent, and its menu only
//! Edit and Delete.

use std::path::Path;

use brrritscold_core_gpui::{
    ActiveTheme as _, Button, ButtonSize, ButtonVariant, DropdownMenu, DropdownMenuAlign,
    DropdownMenuContent, DropdownMenuItem, DropdownMenuItemVariant, DropdownMenuSeparator,
    IconName,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Action, App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, actions, div, px,
};

use crate::keymap::ToggleRightPanel;
use crate::new_agent::NewAgent;

actions!(
    agentdeck,
    [
        /// Opens the Edit workspace dialog on the workspace shown.
        EditWorkspace,
        /// Archives the workspace shown.
        ArchiveWorkspace,
        /// Brings the archived workspace shown back.
        UnarchiveWorkspace,
        /// Opens the Delete workspace dialog on the workspace shown.
        DeleteWorkspace,
        /// Mutes the notifications of the workspace shown.
        MuteWorkspace,
        /// Unmutes the notifications of the workspace shown.
        UnmuteWorkspace,
    ]
);

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
    archived: bool,
    muted: bool,
}

impl TopBar {
    /// The bar for the workspace named `name`, whose directory is `dir`.
    pub fn new(name: impl Into<SharedString>, dir: impl Into<SharedString>) -> Self {
        TopBar {
            name: name.into(),
            dir: dir.into(),
            archived: false,
            muted: false,
        }
    }

    /// The bar of an archived workspace, or of a live one.
    pub fn archived(mut self, archived: bool) -> Self {
        self.archived = archived;
        self
    }

    /// The bar of a workspace whose notifications are muted, or not.
    pub fn muted(mut self, muted: bool) -> Self {
        self.muted = muted;
        self
    }

    /// Whether it heads a muted workspace.
    #[cfg(test)]
    pub fn is_muted(&self) -> bool {
        self.muted
    }

    /// Whether it heads an archived workspace.
    pub fn is_archived(&self) -> bool {
        self.archived
    }

    /// The workspace's name.
    pub fn name(&self) -> &SharedString {
        &self.name
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
            // Penpot `Spacer`, `NewAgent` (outline, sm) or, archived, `Unarchive` (outline,
            // sm), `WorkspaceMenu`, then `TogglePanel`: ghost, icon-sm, `PanelRight`.
            .child(div().flex_1())
            .map(|bar| match self.archived {
                false => bar.child(outline("new agent", "New agent", NewAgent)),
                true => bar.child(outline("unarchive", "Unarchive", UnarchiveWorkspace)),
            })
            .child(menu(self.archived, self.muted))
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

/// An outline sm button reading `label` that dispatches `action`; tests find it as `name`.
fn outline(name: &'static str, label: &'static str, action: impl Action) -> impl IntoElement {
    div().debug_selector(move || name.into()).child(
        Button::new(name)
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Sm)
            .label(label)
            .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx)),
    )
}

/// Penpot `WorkspaceMenu`: a ghost icon-sm `Ellipsis` button opening the workspace's actions:
/// Edit workspace, Mute notifications (Unmute notifications once `muted`), Archive workspace,
/// a separator and the destructive Delete workspace; only Edit and Delete once the workspace
/// is `archived`.
fn menu(archived: bool, muted: bool) -> impl IntoElement {
    let item = |label: &'static str, action: Box<dyn Action>| {
        DropdownMenuItem::new(label)
            .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
    };
    let edit = item("Edit workspace", Box::new(EditWorkspace));
    let delete = item("Delete workspace", Box::new(DeleteWorkspace))
        .variant(DropdownMenuItemVariant::Destructive);
    let mute = match muted {
        false => item("Mute notifications", Box::new(MuteWorkspace)),
        true => item("Unmute notifications", Box::new(UnmuteWorkspace)),
    };
    let content = match archived {
        false => DropdownMenuContent::new()
            .item(edit)
            .item(mute)
            .item(item("Archive workspace", Box::new(ArchiveWorkspace)))
            .separator(DropdownMenuSeparator::new())
            .item(delete),
        true => DropdownMenuContent::new()
            .item(edit)
            .separator(DropdownMenuSeparator::new())
            .item(delete),
    };
    div().debug_selector(|| "workspace menu".into()).child(
        // Penpot "Screens · Main" → "Workspace menu": end-aligned, 4px below the trigger.
        DropdownMenu::new("workspace-menu")
            .align(DropdownMenuAlign::End)
            .trigger(
                Button::new("workspace-menu-trigger")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::IconSm)
                    .icon(IconName::Ellipsis),
            )
            .content(content),
    )
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
