//! Penpot `Centre / TopBar`: the selected workspace's name, above its tabs (spec §11). A
//! workspace has no folder, so the bar shows none.
//!
//! Its New agent button opens the new agent dialog, its `…` menu opens the Edit workspace
//! dialog, mutes or unmutes the workspace's notifications, archives it or opens the Delete
//! workspace dialog, all as actions the window handles. An archived workspace's bar has Unarchive in place of New agent, and its menu only
//! Edit and Delete.

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

/// The workspace view's top bar.
#[derive(IntoElement, Clone, Debug, PartialEq)]
pub struct TopBar {
    name: SharedString,
    archived: bool,
    muted: bool,
}

impl TopBar {
    /// The bar for the workspace named `name`.
    pub fn new(name: impl Into<SharedString>) -> Self {
        TopBar {
            name: name.into(),
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
}

impl RenderOnce for TopBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let s = cx.theme().semantic();
        // Penpot `TopBar`: 52px high, px `space.4`, items centred, `space.3` apart.
        div()
            .debug_selector(|| "top bar".into())
            .flex_none()
            .h(px(HEIGHT))
            .flex()
            .items_center()
            .gap(space::S3)
            .px(space::S4)
            // Penpot `Name`: sm, medium, `foreground`; it truncates.
            .child(
                div()
                    .debug_selector(|| "workspace name".into())
                    .min_w_0()
                    .truncate()
                    .text_size(font_size::SM)
                    .font_weight(font_weight::MEDIUM)
                    .text_color(s.foreground)
                    .child(self.name),
            )
            // Penpot `Spacer`, `NewAgent` (outline, sm) or, archived, `Unarchive` (outline,
            // sm), then `WorkspaceMenu`.
            .child(div().flex_1())
            .map(|bar| match self.archived {
                false => bar.child(outline("new agent", "New agent", NewAgent)),
                true => bar.child(outline("unarchive", "Unarchive", UnarchiveWorkspace)),
            })
            .child(menu(self.archived, self.muted))
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
    use gpui::{Context, Render, TestAppContext, VisualTestContext};

    use super::*;

    /// A view showing one bar.
    struct Shows(TopBar);

    impl Render for Shows {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(1000.)).child(self.0.clone())
        }
    }

    mod top_bar {
        use super::*;

        #[gpui::test]
        fn should_show_the_name_alone_centred_in_the_bar(cx: &mut TestAppContext) {
            cx.update(brrritscold_core_gpui::init);
            let window = cx.add_window(|_, _| Shows(TopBar::new("agentdeck")));
            let cx = VisualTestContext::from_window(*window, cx).into_mut();
            cx.update(|window, cx| window.draw(cx).clear(cx));

            let bar = cx.debug_bounds("top bar").expect("the bar");
            let name = cx.debug_bounds("workspace name").expect("the name");
            assert_eq!(bar.size.height, px(HEIGHT));
            // Within half a pixel: the text's line box rounds.
            assert!(
                (name.center().y - bar.center().y).abs() <= px(0.5),
                "name {name:?} in bar {bar:?}"
            );
        }
    }
}
