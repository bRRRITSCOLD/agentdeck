//! The Settings view (spec §11), Penpot "Screens" → "Settings": the settings the daemon keeps
//! in `config.toml` (spec §2), in sections of fields.
//!
//! - **GitHub:** the account and the OAuth client ID ([`GithubSection`]), then "Sync PRs
//!   updated in the last (days)", saved on Enter or when the field loses focus. Text that
//!   isn't a count of days shows the field's error and isn't saved.
//! - **Appearance:** Theme, a Select of System / Dark / Light. Picking one applies it at once
//!   and saves it.
//! - **Terminal:** "Open file paths with", `terminal.file_opener`, saved the same way. A
//!   command that isn't an opener ([`FileOpener::parse`]) shows the field's error instead.
//! - **General:** "Ask before quitting while agents run", `gui.confirm_quit`, a Switch. Flipping
//!   it shows the new value at once and saves it; a failed save puts the saved one back. The
//!   quit dialog's Don't ask again turns it off through [`SettingsView::set_confirm_quit`].
//!
//! The heading stays put; the sections scroll under it, with the scrollbar the lists show.
//!
//! The window makes the view as it connects, so the saved theme applies from the start. It
//! reads the settings again on every `changed settings`, so a change another client makes
//! shows here and its theme applies. A failed read or save toasts why; a theme that couldn't be
//! saved is put back.

use std::sync::Arc;

use agentdeck_client::ApiError;
use agentdeck_client::types::{
    AppearancePatch, GuiPatch, PrSyncPatch, Settings, SettingsPatch, TerminalPatch, ThemePreference,
};
use agentdeck_core::opener::{FileOpener, OpenerError};
use agentdeck_core::ws::{Entity as Changed, ServerMessage};
use brrritscold_core_gpui::{
    ActiveTheme as _, Field, FieldContent, FieldDescription, FieldError, FieldOrientation, Input,
    InputState, Label, Select, SelectContent, SelectItem, SelectTrigger, Switch, Theme,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, Window, div, point, px,
};
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::connection::{Connection, DaemonEvent};
use crate::github::GithubSection;
use crate::rereads::Rereads;
use crate::scrollbar;
use crate::workspace::failed;

/// A section's width. Raw in Penpot too: the fields are 480px wide.
const SECTION_WIDTH: gpui::Pixels = px(480.);

/// The age cutoff field's description. A narrower cutoff makes the open PRs past it Finished
/// at the next full sync, hourly, and a sync deletes each 7 days after that (spec §8 →
/// Finished).
const DAYS_DESCRIPTION: &str = "0 syncs every open PR. Older PRs leave within 7 days.";

/// What the field shows for text that isn't a count of days.
const NOT_DAYS: &str = "Enter a whole number of days, 0 or more.";

/// What the opener field shows for a blank command.
const NO_COMMAND: &str = "Enter a command, such as xdg-open {path}.";

/// What the opener field shows for a command with an unclosed quote.
const UNCLOSED: &str = "Close the quote in the command.";

/// What the opener field shows for a command that isn't given `{path}`.
const NO_PATH: &str = "Pass {path} to the command, as in code -g {path}:{line}.";

/// The opener field's description, as Penpot words it.
const OPENER_DESCRIPTION: &str = "Ctrl+click a file path in a pane to run this. {path} is the file and {line} its line, when the path has one (src/main.rs:42). For example: code -g {path}:{line}";

/// Why the opener field can't take `command`, in the field's words; `None` when it can.
fn opener_error(command: &str) -> Option<&'static str> {
    match FileOpener::parse(command) {
        Ok(_) => None,
        Err(OpenerError::Empty) => Some(NO_COMMAND),
        Err(OpenerError::Unsplittable(_)) => Some(UNCLOSED),
        Err(OpenerError::NoPath) => Some(NO_PATH),
    }
}

/// The confirm quit switch's description, as Penpot words it.
const CONFIRM_QUIT_DESCRIPTION: &str = "Agents keep running in the background after agentdeck quits. Off, Quit and the close button quit at once.";

/// The core-gpui preference for the daemon's `appearance.theme`.
pub fn preference(theme: ThemePreference) -> brrritscold_core_gpui::ThemePreference {
    match theme {
        ThemePreference::System => brrritscold_core_gpui::ThemePreference::System,
        ThemePreference::Dark => brrritscold_core_gpui::ThemePreference::Dark,
        ThemePreference::Light => brrritscold_core_gpui::ThemePreference::Light,
    }
}

/// The count of days `text` holds, ignoring the spaces around it.
fn parse_days(text: &str) -> Option<u32> {
    text.trim().parse().ok()
}

/// The Settings view.
pub struct SettingsView {
    api: Arc<dyn Api>,
    /// The settings as last read or saved; `None` until the first read lands.
    settings: Option<Settings>,
    /// The read in flight; replacing it drops the old one.
    read: Option<Task<()>>,
    /// Whether a read runs, and whether a change came in meanwhile.
    rereads: Rereads,
    /// "Sync PRs updated in the last (days)".
    days: Entity<InputState>,
    /// Why the days typed weren't saved, and the text that wasn't, until it is edited.
    days_error: Option<(SharedString, SharedString)>,
    /// "Open file paths with".
    opener: Entity<InputState>,
    /// Why the opener typed wasn't saved, and the text that wasn't, until it is edited.
    opener_error: Option<(SharedString, SharedString)>,
    /// The GitHub account and client ID, at the top of the GitHub section.
    github: Entity<GithubSection>,
    /// Scrolls the sections under the heading.
    scroll: ScrollHandle,
    _days: Subscription,
    _opener: Subscription,
    _events: Subscription,
}

impl SettingsView {
    /// A view over `api` that reads the settings now, applying the saved theme, and again on
    /// each of `connection`'s `changed settings`.
    pub fn new(
        api: Arc<dyn Api>,
        connection: &Entity<Connection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let days = cx.new(|cx| InputState::new(window, cx));
        let _days = cx.subscribe_in(&days, window, |this, _, event, window, cx| match event {
            InputEvent::PressEnter { .. } | InputEvent::Blur => this.save_days(window, cx),
            InputEvent::Change => this.edited_days(cx),
            InputEvent::Focus => {}
        });
        let opener = cx.new(|cx| InputState::new(window, cx));
        let _opener = cx.subscribe_in(&opener, window, |this, _, event, window, cx| match event {
            InputEvent::PressEnter { .. } | InputEvent::Blur => this.save_opener(window, cx),
            InputEvent::Change => this.edited_opener(cx),
            InputEvent::Focus => {}
        });
        let _events = cx.subscribe_in(connection, window, |this, _, event, window, cx| {
            if let DaemonEvent::Message(
                ServerMessage::Changed {
                    entity: Changed::Settings,
                    ..
                }
                | ServerMessage::Resync,
            ) = event
            {
                this.reread(window, cx);
            }
        });
        let github = cx.new(|cx| GithubSection::new(api.clone(), connection, window, cx));
        let mut view = SettingsView {
            api,
            settings: None,
            read: None,
            rereads: Rereads::default(),
            github,
            scroll: ScrollHandle::new(),
            days,
            days_error: None,
            opener,
            opener_error: None,
            _days,
            _opener,
            _events,
        };
        view.reread(window, cx);
        view
    }

    /// Scrolls back to the top, where the GitHub section is.
    pub fn scroll_to_github(&mut self, cx: &mut Context<Self>) {
        self.scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    /// Scrolls to the bottom, where the General section is, as the user would to reach it.
    #[cfg(test)]
    pub fn scroll_to_bottom(&mut self, cx: &mut Context<Self>) {
        let max = self.scroll.max_offset();
        self.scroll.set_offset(point(px(0.), -max.y));
        cx.notify();
    }

    /// Why the days typed weren't saved, if they weren't.
    pub fn days_error(&self) -> Option<&SharedString> {
        self.days_error.as_ref().map(|(why, _)| why)
    }

    /// Why the opener typed wasn't saved, if it wasn't.
    pub fn opener_error(&self) -> Option<&SharedString> {
        self.opener_error.as_ref().map(|(why, _)| why)
    }

    /// Drops the days field's error once its text is no longer what was refused.
    fn edited_days(&mut self, cx: &mut Context<Self>) {
        let text = self.days.read(cx).value();
        if clear_if_edited(&mut self.days_error, &text) {
            cx.notify();
        }
    }

    /// Drops the opener field's error once its text is no longer what was refused.
    fn edited_opener(&mut self, cx: &mut Context<Self>) {
        let text = self.opener.read(cx).value();
        if clear_if_edited(&mut self.opener_error, &text) {
            cx.notify();
        }
    }

    /// Reads the settings, shows them and applies their theme, or reads once more after the
    /// read in flight, so an older read never lands over a newer one.
    pub fn reread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.rereads.ask() {
            return;
        }
        let reading = self.api.settings(cx);
        self.read = Some(cx.spawn_in(window, async move |this, cx| {
            let read = reading.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match read {
                    Ok(settings) => this.show(settings, window, cx),
                    Err(err) => failed("Couldn't read the settings", &err, window, cx),
                }
                if this.rereads.landed() {
                    this.reread(window, cx);
                }
            });
        }));
    }

    /// Shows `settings` and applies their theme. A field keeps what is being typed in it.
    fn show(&mut self, settings: Settings, window: &mut Window, cx: &mut Context<Self>) {
        Theme::set_preference(preference(settings.appearance.theme), cx);
        if show_unless_typing(
            &self.days,
            settings.pr_sync.max_age_days.to_string(),
            window,
            cx,
        ) {
            self.days_error = None;
        }
        let opener = settings.terminal.file_opener.clone();
        crate::terminal::open::set_file_opener(&opener, cx);
        if show_unless_typing(&self.opener, opener, window, cx) {
            self.opener_error = None;
        }
        self.settings = Some(settings);
        cx.notify();
    }

    /// Whether quitting asks first while agents run, as last read or saved; `true`, the
    /// default, until the first read lands.
    pub fn confirm_quit(&self) -> bool {
        self.settings.as_ref().is_none_or(|s| s.gui.confirm_quit)
    }

    /// Shows `gui.confirm_quit` as `on` at once and saves it; a failure puts the saved value
    /// back and toasts why. The task ends once the save has landed, so a quit can wait for it.
    pub fn set_confirm_quit(
        &mut self,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let before = self.settings.as_ref().map(|s| s.gui.confirm_quit);
        if before == Some(on) {
            return Task::ready(());
        }
        if let Some(settings) = &mut self.settings {
            settings.gui.confirm_quit = on;
        }
        cx.notify();
        let patch = SettingsPatch {
            gui: Some(GuiPatch {
                confirm_quit: Some(on),
            }),
            ..SettingsPatch::default()
        };
        let saving = self.api.update_settings(patch, cx);
        cx.spawn_in(window, async move |this, cx| {
            let saved = saving.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match saved {
                    Ok(settings) => this.settings = Some(settings),
                    Err(err) => {
                        if let (Some(settings), Some(before)) = (&mut this.settings, before) {
                            settings.gui.confirm_quit = before;
                        }
                        failed("Couldn't save Ask before quitting", &err, window, cx);
                    }
                }
                cx.notify();
            });
        })
    }

    /// Applies `theme` at once and saves it; a failure puts the saved one back and toasts why.
    pub fn set_theme(
        &mut self,
        theme: ThemePreference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = self.settings.as_ref().map(|s| s.appearance.theme);
        if before == Some(theme) {
            return;
        }
        Theme::set_preference(preference(theme), cx);
        if let Some(settings) = &mut self.settings {
            settings.appearance.theme = theme;
        }
        cx.notify();
        let patch = SettingsPatch {
            appearance: Some(AppearancePatch { theme: Some(theme) }),
            pr_sync: None,
            terminal: None,
            gui: None,
        };
        let saving = self.api.update_settings(patch, cx);
        cx.spawn_in(window, async move |this, cx| {
            let saved = saving.await;
            let _ = this.update_in(cx, |this, window, cx| match saved {
                Ok(settings) => this.settings = Some(settings),
                Err(err) => {
                    if let (Some(settings), Some(before)) = (&mut this.settings, before) {
                        settings.appearance.theme = before;
                        Theme::set_preference(preference(before), cx);
                    }
                    failed("Couldn't save the theme", &err, window, cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Saves the days typed: what Enter and leaving the field do. Text that isn't a count of
    /// days shows the field's error instead; the count already saved isn't sent again.
    pub fn save_days(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.days.read(cx).value();
        let Some(days) = parse_days(&text) else {
            self.days_error = Some((NOT_DAYS.into(), text));
            cx.notify();
            return;
        };
        self.days_error = None;
        cx.notify();
        let saved = self.settings.as_ref().map(|s| s.pr_sync.max_age_days);
        if saved == Some(days) {
            return;
        }
        let patch = SettingsPatch {
            appearance: None,
            pr_sync: Some(PrSyncPatch {
                max_age_days: Some(days),
            }),
            terminal: None,
            gui: None,
        };
        let saving = self.api.update_settings(patch, cx);
        cx.spawn_in(window, async move |this, cx| {
            let saved = saving.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match saved {
                    Ok(settings) => this.settings = Some(settings),
                    Err(err) => match err.downcast_ref::<ApiError>() {
                        Some(refusal) if refusal.code.as_deref() == Some("invalid_setting") => {
                            let text = this.days.read(cx).value();
                            this.days_error = Some((refusal.message.clone().into(), text));
                        }
                        _ => failed("Couldn't save the PR cutoff", &err, window, cx),
                    },
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Saves the opener typed: what Enter and leaving the field do. A command that isn't an
    /// opener shows the field's error instead; the one already saved isn't sent again.
    pub fn save_opener(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.opener.read(cx).value();
        if let Some(why) = opener_error(&text) {
            self.opener_error = Some((why.into(), text));
            cx.notify();
            return;
        }
        self.opener_error = None;
        cx.notify();
        let saved = self
            .settings
            .as_ref()
            .map(|s| s.terminal.file_opener.as_str());
        if saved == Some(text.as_ref()) {
            return;
        }
        let patch = SettingsPatch {
            appearance: None,
            pr_sync: None,
            terminal: Some(TerminalPatch {
                file_opener: Some(text.to_string()),
            }),
            gui: None,
        };
        let saving = self.api.update_settings(patch, cx);
        cx.spawn_in(window, async move |this, cx| {
            let saved = saving.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match saved {
                    Ok(settings) => this.settings = Some(settings),
                    Err(err) => match err.downcast_ref::<ApiError>() {
                        Some(refusal) if refusal.code.as_deref() == Some("invalid_setting") => {
                            let text = this.opener.read(cx).value();
                            this.opener_error = Some((refusal.message.clone().into(), text));
                        }
                        _ => failed("Couldn't save the file opener", &err, window, cx),
                    },
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Penpot "Settings" `Header`: the heading over where the settings are kept.
    fn header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let s = cx.theme().semantic();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(space::S1)
            .child(
                div()
                    .text_size(font_size::XL)
                    .font_weight(font_weight::MEDIUM)
                    .text_color(s.foreground)
                    .child("Settings"),
            )
            .child(
                div()
                    .debug_selector(|| "settings description".into())
                    .text_size(font_size::SM)
                    .text_color(s.muted_foreground)
                    .child("Saved in ~/.config/agentdeck/config.toml. Changes apply at once."),
            )
            .into_any_element()
    }

    /// A section: its heading over its fields, 16px apart.
    fn section(
        &self,
        name: &'static str,
        fields: impl IntoIterator<Item = gpui::AnyElement>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let s = cx.theme().semantic();
        div()
            .debug_selector(move || format!("settings section {name}"))
            .w(SECTION_WIDTH)
            .flex()
            .flex_col()
            .gap(space::S4)
            .child(
                div()
                    .text_size(font_size::BASE)
                    .font_weight(font_weight::SEMIBOLD)
                    .text_color(s.foreground)
                    .child(name),
            )
            .children(fields)
            .into_any_element()
    }

    /// The Theme field.
    fn theme_field(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.settings.as_ref().map(|s| s.appearance.theme);
        let select = Select::new("settings-theme")
            .trigger(SelectTrigger::new())
            .content(
                SelectContent::new()
                    .item(SelectItem::new(
                        ThemePreference::System.to_string(),
                        "System",
                    ))
                    .item(SelectItem::new(ThemePreference::Dark.to_string(), "Dark"))
                    .item(SelectItem::new(ThemePreference::Light.to_string(), "Light")),
            )
            .value(theme.map(|theme| SharedString::from(theme.to_string())))
            .on_value_change(cx.listener(|this, value: &SharedString, window, cx| {
                if let Ok(theme) = value.parse() {
                    this.set_theme(theme, window, cx);
                }
            }));
        Field::new()
            .child(Label::new().child("Theme"))
            .child(
                // Penpot fits the trigger to its value, so the field doesn't stretch it.
                div()
                    .debug_selector(|| "settings theme".into())
                    .self_start()
                    .child(select),
            )
            .child(FieldDescription::new(
                "System follows your desktop's light or dark setting.",
            ))
            .into_any_element()
    }

    /// The PR age cutoff field.
    fn days_field(&self) -> gpui::AnyElement {
        let error = self.days_error().cloned();
        Field::new()
            .child(Label::new().child("Sync PRs updated in the last (days)"))
            .child(
                div()
                    .debug_selector(|| "settings days".into())
                    .child(Input::new(&self.days).aria_invalid(error.is_some())),
            )
            .child(FieldDescription::new(DAYS_DESCRIPTION))
            .when_some(error, |field, why| {
                field.child(
                    div()
                        .debug_selector(|| "settings days error".into())
                        .child(FieldError::new(why)),
                )
            })
            .into_any_element()
    }

    /// Penpot `ConfirmQuit`: a horizontal Field, the Switch beside its label and description.
    fn confirm_quit_field(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity().downgrade();
        let field = Field::new()
            .orientation(FieldOrientation::Horizontal)
            .child(
                div()
                    .debug_selector(|| "settings confirm quit".into())
                    .child(
                        Switch::new("settings-confirm-quit")
                            .checked(self.confirm_quit())
                            .accessibility_label("Ask before quitting while agents run")
                            .on_change(move |on, window, cx| {
                                let _ = view.update(cx, |view, cx| {
                                    view.set_confirm_quit(on, window, cx).detach();
                                });
                            }),
                    ),
            )
            // `min_w_0`, so the description wraps in the section's width: a GPUI text's
            // min-content width is its whole line, where CSS's is its longest word.
            .content(
                FieldContent::new()
                    .min_w_0()
                    .child(Label::new().child("Ask before quitting while agents run"))
                    .child(FieldDescription::new(CONFIRM_QUIT_DESCRIPTION)),
            );
        div()
            .debug_selector(|| "settings confirm quit field".into())
            .child(field)
            .into_any_element()
    }

    /// The file opener field.
    fn opener_field(&self) -> gpui::AnyElement {
        let error = self.opener_error().cloned();
        Field::new()
            .child(Label::new().child("Open file paths with"))
            .child(
                div()
                    .debug_selector(|| "settings opener".into())
                    .child(Input::new(&self.opener).aria_invalid(error.is_some())),
            )
            .child(FieldDescription::new(OPENER_DESCRIPTION))
            .when_some(error, |field, why| {
                field.child(
                    div()
                        .debug_selector(|| "settings opener error".into())
                        .child(FieldError::new(why)),
                )
            })
            .into_any_element()
    }
}

/// Clears `error` once the field's `text` is no longer what was refused; whether it did.
fn clear_if_edited(error: &mut Option<(SharedString, SharedString)>, text: &SharedString) -> bool {
    if error.as_ref().is_some_and(|(_, refused)| refused != text) {
        *error = None;
        return true;
    }
    false
}

/// Puts `text` in `input` unless it is focused, so what is being typed stays; whether it did.
fn show_unless_typing(
    input: &Entity<InputState>,
    text: String,
    window: &mut Window,
    cx: &mut Context<SettingsView>,
) -> bool {
    if input.read(cx).focus_handle(cx).is_focused(window) {
        return false;
    }
    input.update(cx, |input, cx| input.set_value(text, window, cx));
    true
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let theme = self.theme_field(cx);
        let appearance = self.section("Appearance", [theme], cx);
        let terminal = self.section("Terminal", [self.opener_field()], cx);
        let confirm_quit = self.confirm_quit_field(cx);
        let general = self.section("General", [confirm_quit], cx);
        // Penpot `GitHub`: its heading and account (the GitHub section's), then the cutoff.
        let github = div()
            .debug_selector(|| "settings section GitHub".into())
            .w(SECTION_WIDTH)
            .flex()
            .flex_col()
            .gap(space::S4)
            .child(self.github.clone())
            .child(self.days_field());
        let header = self.header(cx);
        // Penpot `Content`: Header / Sections, padded `space.6`, as the Pull requests view's.
        // The heading stays put and the sections scroll, the scrollbar 2px in from the right
        // edge and 8px up from the bottom, as Penpot's `Scrollbar` sits.
        div()
            .debug_selector(|| "settings view".into())
            .size_full()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(s.background)
            .child(
                div()
                    .pt(space::S6)
                    .px(space::S6)
                    .pb(space::S4)
                    .child(header),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .mr(space::S0_5)
                    .mb(space::S2)
                    .child(
                        div()
                            .id("settings")
                            .debug_selector(|| "settings sections".into())
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .pl(space::S6)
                            .pb(space::S4)
                            // Sections 32px apart under 8px.
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_none()
                                    .gap(space::S8)
                                    .pt(space::S2)
                                    .child(github)
                                    .child(appearance)
                                    .child(terminal)
                                    .child(general),
                            ),
                    )
                    .child(scrollbar::vertical(&self.scroll, cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use agentdeck_client::types::{
        AppearanceSettings, GuiSettings, PrSyncSettings, TerminalSettings,
    };
    use agentdeck_client::ws::{self, Remote};
    use brrritscold_core_gpui::{Mode, Root};
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::{FakeApi, default_settings};
    use crate::connection::fake::FakeConnector;
    use crate::notice;

    fn with_theme(theme: ThemePreference) -> Settings {
        Settings {
            appearance: AppearanceSettings { theme },
            ..default_settings()
        }
    }

    fn with_days(days: u32) -> Settings {
        Settings {
            pr_sync: PrSyncSettings { max_age_days: days },
            ..default_settings()
        }
    }

    struct Harness {
        api: Arc<FakeApi>,
        view: Entity<SettingsView>,
        remote: Remote,
        _connection: Entity<Connection>,
    }

    impl Harness {
        /// A `Root` window on the view, over a daemon whose settings are `settings`.
        fn open(settings: Settings, cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            let api = Arc::new(FakeApi::default());
            api.set_settings(settings);
            let connector = Rc::new(FakeConnector::default());
            let remote = connector.then_connect_to(api.clone());
            let connection = cx.update(|cx| cx.new(|cx| Connection::new(connector, cx)));
            let mut view = None;
            let window = cx.add_window(|window, cx| {
                let api: Arc<dyn Api> = api.clone();
                let settings = cx.new(|cx| SettingsView::new(api, &connection, window, cx));
                view = Some(settings.clone());
                Root::new(settings, window, cx)
            });
            let cx = VisualTestContext::from_window(*window, cx).into_mut();
            // Focus events reach only an active window's views.
            cx.update(|window, _| window.activate_window());
            let harness = Harness {
                api,
                view: view.unwrap(),
                remote,
                _connection: connection,
            };
            harness.settle(cx);
            (harness, cx)
        }

        fn settle(&self, cx: &mut VisualTestContext) {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        fn preference(&self, cx: &mut VisualTestContext) -> brrritscold_core_gpui::ThemePreference {
            cx.read(|cx| cx.theme().preference())
        }

        fn days_shown(&self, cx: &mut VisualTestContext) -> String {
            cx.read(|cx| self.view.read(cx).days.read(cx).value().to_string())
        }

        /// Focuses the days field and types `text` over what it holds.
        fn type_days(&self, text: &str, cx: &mut VisualTestContext) {
            cx.update(|window, cx| {
                let days = self.view.read(cx).days.clone();
                days.update(cx, |input, cx| {
                    input.focus(window, cx);
                    input.set_value(text.to_owned(), window, cx);
                });
            });
            self.settle(cx);
        }

        fn patches(&self) -> Vec<SettingsPatch> {
            self.api.settings_patches.lock().unwrap().clone()
        }

        fn days_error(&self, cx: &mut VisualTestContext) -> Option<String> {
            self.view
                .read_with(cx, |view, _| view.days_error().map(|e| e.to_string()))
        }

        fn opener_shown(&self, cx: &mut VisualTestContext) -> String {
            cx.read(|cx| self.view.read(cx).opener.read(cx).value().to_string())
        }

        /// Focuses the opener field and types `text` over what it holds.
        fn type_opener(&self, text: &str, cx: &mut VisualTestContext) {
            cx.update(|window, cx| {
                let opener = self.view.read(cx).opener.clone();
                opener.update(cx, |input, cx| {
                    input.focus(window, cx);
                    input.set_value(text.to_owned(), window, cx);
                });
            });
            self.settle(cx);
        }

        fn opener_error(&self, cx: &mut VisualTestContext) -> Option<String> {
            self.view
                .read_with(cx, |view, _| view.opener_error().map(|e| e.to_string()))
        }
    }

    fn with_opener(command: &str) -> Settings {
        Settings {
            terminal: TerminalSettings {
                file_opener: command.into(),
            },
            ..default_settings()
        }
    }

    fn opener_patch(command: &str) -> SettingsPatch {
        SettingsPatch {
            appearance: None,
            pr_sync: None,
            terminal: Some(TerminalPatch {
                file_opener: Some(command.into()),
            }),
            gui: None,
        }
    }

    mod preference {
        use super::*;

        #[test]
        fn should_map_each_theme_to_core_gpuis() {
            use brrritscold_core_gpui::ThemePreference as Gpui;

            assert_eq!(preference(ThemePreference::System), Gpui::System);
            assert_eq!(preference(ThemePreference::Dark), Gpui::Dark);
            assert_eq!(preference(ThemePreference::Light), Gpui::Light);
        }
    }

    mod theme {
        use super::*;

        #[gpui::test]
        fn should_apply_the_saved_theme_once_read(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(with_theme(ThemePreference::Dark), cx);

            assert_eq!(
                harness.preference(cx),
                brrritscold_core_gpui::ThemePreference::Dark
            );
            assert_eq!(cx.read(|cx| cx.theme().mode()), Mode::Dark);
        }

        #[gpui::test]
        fn should_save_and_apply_the_theme_picked(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);

            cx.update(|window, cx| {
                harness.view.update(cx, |view, cx| {
                    view.set_theme(ThemePreference::Light, window, cx);
                });
            });
            harness.settle(cx);

            assert_eq!(
                harness.preference(cx),
                brrritscold_core_gpui::ThemePreference::Light
            );
            assert_eq!(
                harness.patches(),
                [SettingsPatch {
                    appearance: Some(AppearancePatch {
                        theme: Some(ThemePreference::Light),
                    }),
                    pr_sync: None,
                    terminal: None,
                    gui: None,
                }]
            );
            assert_eq!(
                harness.api.settings_now().appearance.theme,
                ThemePreference::Light
            );
        }

        /// Penpot "Screens · Settings" → "Settings": the trigger fits "System" and its chevron
        /// (about 96px), not the section's 480px.
        #[gpui::test]
        fn should_fit_the_select_to_its_value(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(with_theme(ThemePreference::System), cx);

            let select = cx.debug_bounds("settings theme").expect("the select");

            assert!(select.size.width < px(120.), "{:?}", select.size);
            assert!(select.size.width > px(60.), "{:?}", select.size);
        }

        #[gpui::test]
        fn should_put_the_theme_back_and_toast_when_it_cant_be_saved(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(with_theme(ThemePreference::Dark), cx);
            harness.api.fail_next("connection refused");

            cx.update(|window, cx| {
                harness.view.update(cx, |view, cx| {
                    view.set_theme(ThemePreference::Light, window, cx);
                });
            });
            harness.settle(cx);

            assert_eq!(
                harness.preference(cx),
                brrritscold_core_gpui::ThemePreference::Dark
            );
            let shown = cx.update(|_, cx| notice::shown(cx));
            assert_eq!(shown.last().unwrap().title, "Couldn't save the theme");
        }
    }

    mod days {
        use super::*;

        #[test]
        fn should_say_older_prs_leave_within_their_finished_window() {
            assert_eq!(
                DAYS_DESCRIPTION,
                "0 syncs every open PR. Older PRs leave within 7 days."
            );
        }

        #[gpui::test]
        fn should_show_the_saved_cutoff(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(with_days(30), cx);

            assert_eq!(harness.days_shown(cx), "30");
        }

        #[gpui::test]
        fn should_save_the_days_on_enter(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            harness.type_days(" 14 ", cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(
                harness.patches(),
                [SettingsPatch {
                    appearance: None,
                    pr_sync: Some(PrSyncPatch {
                        max_age_days: Some(14),
                    }),
                    terminal: None,
                    gui: None,
                }]
            );
            assert_eq!(harness.api.settings_now().pr_sync.max_age_days, 14);
        }

        #[gpui::test]
        fn should_save_the_days_when_the_field_loses_focus(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            harness.type_days("0", cx);

            cx.update(|window, cx| window.blur(cx));
            harness.settle(cx);

            assert_eq!(harness.api.settings_now().pr_sync.max_age_days, 0);
        }

        #[gpui::test]
        fn should_not_save_the_cutoff_already_saved(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(with_days(30), cx);
            harness.type_days("30", cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(harness.patches(), []);
        }

        #[gpui::test]
        fn should_show_the_fields_error_and_not_save_text_that_isnt_days(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            harness.type_days("-3", cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(harness.patches(), []);
            assert_eq!(harness.days_error(cx).as_deref(), Some(NOT_DAYS));
            assert!(cx.debug_bounds("settings days error").is_some());
        }

        #[gpui::test]
        fn should_clear_the_error_once_the_days_are_edited(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            harness.type_days("soon", cx);
            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            cx.simulate_input("x");
            harness.settle(cx);

            assert_eq!(harness.days_error(cx), None);
            assert!(cx.debug_bounds("settings days error").is_none());
        }

        #[gpui::test]
        fn should_show_the_daemons_refusal_under_the_field(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            harness.api.refuse_next(
                "invalid_setting",
                "pr_sync.max_age_days must be a whole number of days, 0 or more, not 1",
            );
            harness.type_days("1", cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert!(
                harness
                    .days_error(cx)
                    .is_some_and(|e| e.starts_with("pr_sync.max_age_days ")),
                "{:?}",
                harness.days_error(cx)
            );
        }

        #[gpui::test]
        fn should_toast_when_the_days_cant_be_saved(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            harness.api.fail_next("connection refused");
            harness.type_days("7", cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            let shown = cx.update(|_, cx| notice::shown(cx));
            assert_eq!(shown.last().unwrap().title, "Couldn't save the PR cutoff");
        }
    }

    mod opener {
        use super::*;

        #[gpui::test]
        fn should_show_the_saved_opener(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(with_opener("code -g {path}:{line}"), cx);

            assert_eq!(harness.opener_shown(cx), "code -g {path}:{line}");
            assert!(cx.debug_bounds("settings opener").is_some());
        }

        #[gpui::test]
        fn should_make_the_saved_opener_the_one_a_click_runs(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(with_opener("code -g {path}:{line}"), cx);

            let argv = cx.update(|_, cx| crate::terminal::open::argv("/w/a.rs", Some(2), None, cx));

            assert_eq!(argv, ["code", "-g", "/w/a.rs:2"]);
        }

        #[gpui::test]
        fn should_save_the_opener_on_enter(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            harness.type_opener("code -g {path}:{line}", cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(harness.patches(), [opener_patch("code -g {path}:{line}")]);
            assert_eq!(
                harness.api.settings_now().terminal.file_opener,
                "code -g {path}:{line}"
            );
        }

        #[gpui::test]
        fn should_save_the_opener_when_the_field_loses_focus(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            harness.type_opener("kitty nvim +{line} {path}", cx);

            cx.update(|window, cx| window.blur(cx));
            harness.settle(cx);

            assert_eq!(
                harness.api.settings_now().terminal.file_opener,
                "kitty nvim +{line} {path}"
            );
        }

        #[gpui::test]
        fn should_not_save_the_opener_already_saved(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            harness.type_opener(agentdeck_core::opener::DEFAULT, cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(harness.patches(), []);
        }

        #[gpui::test]
        fn should_show_the_fields_error_and_not_save_a_command_without_path(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            harness.type_opener("code -g", cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(harness.patches(), []);
            assert_eq!(harness.opener_error(cx).as_deref(), Some(NO_PATH));
            assert!(cx.debug_bounds("settings opener error").is_some());
        }

        #[gpui::test]
        fn should_clear_the_error_once_the_opener_is_edited(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            harness.type_opener("", cx);
            cx.simulate_keystrokes("enter");
            harness.settle(cx);
            assert_eq!(harness.opener_error(cx).as_deref(), Some(NO_COMMAND));

            cx.simulate_input("x");
            harness.settle(cx);

            assert_eq!(harness.opener_error(cx), None);
        }

        #[gpui::test]
        fn should_show_the_daemons_refusal_under_the_field(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            harness.api.refuse_next(
                "invalid_setting",
                "terminal.file_opener must name a command",
            );
            harness.type_opener("code {path}", cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(
                harness.opener_error(cx).as_deref(),
                Some("terminal.file_opener must name a command")
            );
        }

        #[gpui::test]
        fn should_toast_when_the_opener_cant_be_saved(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            harness.api.fail_next("connection refused");
            harness.type_opener("code {path}", cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            let shown = cx.update(|_, cx| notice::shown(cx));
            assert_eq!(shown.last().unwrap().title, "Couldn't save the file opener");
        }
    }

    mod confirm_quit {
        use gpui::Modifiers;

        use super::*;

        fn asking(confirm: bool) -> Settings {
            Settings {
                gui: GuiSettings {
                    confirm_quit: confirm,
                },
                ..default_settings()
            }
        }

        fn patch(confirm: bool) -> SettingsPatch {
            SettingsPatch {
                gui: Some(GuiPatch {
                    confirm_quit: Some(confirm),
                }),
                ..SettingsPatch::default()
            }
        }

        fn shown(harness: &Harness, cx: &mut VisualTestContext) -> bool {
            harness.view.read_with(cx, |view, _| view.confirm_quit())
        }

        /// Clicks the switch, scrolled into view under the other sections.
        fn click_switch(harness: &Harness, cx: &mut VisualTestContext) {
            harness.view.update(cx, |view, cx| {
                view.scroll.scroll_to_bottom();
                cx.notify();
            });
            harness.settle(cx);
            let switch = cx
                .debug_bounds("settings confirm quit")
                .expect("the switch shows");
            cx.simulate_click(switch.center(), Modifiers::default());
            harness.settle(cx);
        }

        #[gpui::test]
        fn should_show_the_saved_setting(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(asking(false), cx);

            assert!(!shown(&harness, cx));
        }

        #[gpui::test]
        fn should_turn_it_off_from_the_switch(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(asking(true), cx);

            click_switch(&harness, cx);

            assert!(!shown(&harness, cx));
            assert_eq!(harness.patches(), [patch(false)]);
        }

        #[gpui::test]
        fn should_turn_it_back_on_from_the_switch(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(asking(false), cx);

            click_switch(&harness, cx);

            assert!(shown(&harness, cx));
            assert!(harness.api.settings_now().gui.confirm_quit);
            assert_eq!(harness.patches(), [patch(true)]);
        }

        #[gpui::test]
        fn should_wrap_the_description_in_the_sections_width(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(asking(true), cx);

            let field = cx
                .debug_bounds("settings confirm quit field")
                .expect("the field shows");

            // More than a 20px label line, 4px and one 20px description line: it wraps.
            assert_eq!(field.size.width, SECTION_WIDTH);
            assert!(field.size.height > px(20. + 4. + 20.), "{field:?}");
        }

        #[gpui::test]
        fn should_put_the_saved_value_back_when_the_save_fails(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(asking(true), cx);
            harness.api.fail_next("connection refused");

            click_switch(&harness, cx);

            assert!(shown(&harness, cx));
            let shown = cx.update(|_, cx| notice::shown(cx));
            assert_eq!(
                shown.last().unwrap().title,
                "Couldn't save Ask before quitting"
            );
        }
    }

    mod changed {
        use super::*;

        #[gpui::test]
        fn should_read_again_and_apply_what_another_client_changed(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            let reads = harness.api.settings_reads();
            harness.api.set_settings(Settings {
                appearance: AppearanceSettings {
                    theme: ThemePreference::Dark,
                },
                pr_sync: PrSyncSettings { max_age_days: 7 },
                terminal: TerminalSettings {
                    file_opener: "code {path}".into(),
                },
                gui: GuiSettings {
                    confirm_quit: false,
                },
                overridden_by_env: Vec::new(),
            });

            harness
                .remote
                .events
                .try_send(ws::Event::Message(ServerMessage::Changed {
                    entity: Changed::Settings,
                    id: "settings".into(),
                }))
                .unwrap();
            harness.settle(cx);

            assert_eq!(harness.api.settings_reads(), reads + 1);
            assert_eq!(
                harness.preference(cx),
                brrritscold_core_gpui::ThemePreference::Dark
            );
            assert_eq!(harness.days_shown(cx), "7");
            assert_eq!(harness.opener_shown(cx), "code {path}");
        }

        #[gpui::test]
        fn should_show_the_newer_read_when_an_older_one_lands_last(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            let changed = || {
                ws::Event::Message(ServerMessage::Changed {
                    entity: Changed::Settings,
                    id: "settings".into(),
                })
            };
            harness.api.set_settings(with_days(7));
            let release = harness.api.hold_settings_read();
            harness.remote.events.try_send(changed()).unwrap();
            harness.settle(cx);
            harness.api.set_settings(with_days(14));
            harness.remote.events.try_send(changed()).unwrap();
            harness.settle(cx);

            release.send(()).unwrap();
            harness.settle(cx);

            assert_eq!(harness.days_shown(cx), "14");
        }

        #[gpui::test]
        fn should_ignore_other_changes(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            let reads = harness.api.settings_reads();

            harness
                .remote
                .events
                .try_send(ws::Event::Message(ServerMessage::Changed {
                    entity: Changed::PullRequest,
                    id: "x".into(),
                }))
                .unwrap();
            harness.settle(cx);

            assert_eq!(harness.api.settings_reads(), reads);
        }
    }

    mod layout {
        use super::*;

        #[gpui::test]
        fn should_lay_the_three_sections_out_480px_wide_and_32px_apart(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(default_settings(), cx);

            let appearance = cx
                .debug_bounds("settings section Appearance")
                .expect("Appearance");
            let github = cx.debug_bounds("settings section GitHub").expect("GitHub");
            let terminal = cx
                .debug_bounds("settings section Terminal")
                .expect("Terminal");

            assert_eq!(appearance.size.width, SECTION_WIDTH);
            assert_eq!(github.size.width, SECTION_WIDTH);
            assert_eq!(terminal.size.width, SECTION_WIDTH);
            assert_eq!(appearance.top() - github.bottom(), space::S8);
            assert_eq!(terminal.top() - appearance.bottom(), space::S8);
        }

        #[gpui::test]
        fn should_show_a_scrollbar_when_the_sections_are_taller_than_the_view(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            cx.simulate_resize(gpui::size(px(1440.), px(500.)));
            harness.settle(cx);

            let sections = cx.debug_bounds("settings sections").expect("the sections");
            assert!(crate::scrollbar::testing::thumb(sections, cx).is_some());
        }

        #[gpui::test]
        fn should_hide_the_scrollbar_when_everything_fits(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            cx.simulate_resize(gpui::size(px(1440.), px(2000.)));
            harness.settle(cx);

            let sections = cx.debug_bounds("settings sections").expect("the sections");
            assert_eq!(crate::scrollbar::testing::thumb(sections, cx), None);
        }

        #[gpui::test]
        fn should_keep_the_heading_in_place_while_the_sections_scroll(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            cx.simulate_resize(gpui::size(px(1440.), px(500.)));
            harness.settle(cx);
            let heading = cx
                .debug_bounds("settings description")
                .expect("the heading");
            let github = cx.debug_bounds("settings section GitHub").expect("GitHub");

            let sections = cx.debug_bounds("settings sections").expect("the sections");
            cx.simulate_event(gpui::ScrollEvent {
                position: sections.center(),
                delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-200.))),
                ..gpui::ScrollEvent::default()
            });
            harness.settle(cx);

            assert_eq!(cx.debug_bounds("settings description"), Some(heading));
            let scrolled = cx.debug_bounds("settings section GitHub").expect("GitHub");
            assert!(scrolled.top() < github.top());
        }

        #[gpui::test]
        fn should_draw_in_both_themes(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);

            for theme in [ThemePreference::Light, ThemePreference::Dark] {
                cx.update(|_, cx| Theme::set_preference(preference(theme), cx));
                harness.settle(cx);
            }
        }
    }
}
