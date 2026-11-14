//! The Settings view (spec §11), Penpot "Screens" → "Settings": the settings the daemon keeps
//! in `config.toml` (spec §2), in sections of fields.
//!
//! - **Appearance:** Theme, a Select of System / Dark / Light. Picking one applies it at once
//!   and saves it.
//! - **GitHub:** "Sync PRs updated in the last (days)", saved on Enter or when the field loses
//!   focus. Text that isn't a count of days shows the field's error and isn't saved.
//!
//! The window makes the view as it connects, so the saved theme applies from the start. It
//! reads the settings again on every `changed settings`, so a change another client makes
//! shows here and its theme applies. A failed read or save toasts why; a theme that couldn't be
//! saved is put back.

use std::sync::Arc;

use agentdeck_client::ApiError;
use agentdeck_client::types::{
    AppearancePatch, PrSyncPatch, Settings, SettingsPatch, ThemePreference,
};
use agentdeck_core::ws::{Entity as Changed, ServerMessage};
use brrritscold_core_gpui::{
    ActiveTheme as _, Field, FieldDescription, FieldError, Input, InputState, Label, Select,
    SelectContent, SelectItem, SelectTrigger, Theme,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, px,
};
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::connection::{Connection, DaemonEvent};
use crate::workspace::failed;

/// A section's width. Raw in Penpot too: the fields are 480px wide.
const SECTION_WIDTH: gpui::Pixels = px(480.);

/// What the field shows for text that isn't a count of days.
const NOT_DAYS: &str = "Enter a whole number of days, 0 or more.";

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
    /// "Sync PRs updated in the last (days)".
    days: Entity<InputState>,
    /// Why the days typed weren't saved, and the text that wasn't, until it is edited.
    days_error: Option<(SharedString, SharedString)>,
    _days: Subscription,
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
        let _events = cx.subscribe_in(connection, window, |this, _, event, window, cx| {
            if let DaemonEvent::Message(ServerMessage::Changed {
                entity: Changed::Settings,
                ..
            }) = event
            {
                this.reread(window, cx);
            }
        });
        let mut view = SettingsView {
            api,
            settings: None,
            days,
            days_error: None,
            _days,
            _events,
        };
        view.reread(window, cx);
        view
    }

    /// Why the days typed weren't saved, if they weren't.
    pub fn days_error(&self) -> Option<&SharedString> {
        self.days_error.as_ref().map(|(why, _)| why)
    }

    /// Drops the days field's error once its text is no longer what was refused.
    fn edited_days(&mut self, cx: &mut Context<Self>) {
        let text = self.days.read(cx).value();
        if self
            .days_error
            .as_ref()
            .is_some_and(|(_, refused)| *refused != text)
        {
            self.days_error = None;
            cx.notify();
        }
    }

    /// Reads the settings, shows them and applies their theme.
    pub fn reread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reading = self.api.settings(cx);
        cx.spawn_in(window, async move |this, cx| {
            let read = reading.await;
            let _ = this.update_in(cx, |this, window, cx| match read {
                Ok(settings) => this.show(settings, window, cx),
                Err(err) => failed("Couldn't read the settings", &err, window, cx),
            });
        })
        .detach();
    }

    /// Shows `settings` and applies their theme. The days field keeps what is being typed in
    /// it.
    fn show(&mut self, settings: Settings, window: &mut Window, cx: &mut Context<Self>) {
        Theme::set_preference(preference(settings.appearance.theme), cx);
        let days = self.days.clone();
        let typing = days.read(cx).focus_handle(cx).is_focused(window);
        if !typing {
            let text = settings.pr_sync.max_age_days.to_string();
            days.update(cx, |input, cx| input.set_value(text, window, cx));
            self.days_error = None;
        }
        self.settings = Some(settings);
        cx.notify();
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
            .trigger(SelectTrigger::new().w_full())
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
                div()
                    .debug_selector(|| "settings theme".into())
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
            .child(FieldDescription::new(
                "0 syncs every open PR. Older PRs already here stay until you mark them done.",
            ))
            .when_some(error, |field, why| {
                field.child(
                    div()
                        .debug_selector(|| "settings days error".into())
                        .child(FieldError::new(why)),
                )
            })
            .into_any_element()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let theme = self.theme_field(cx);
        let appearance = self.section("Appearance", [theme], cx);
        let github = self.section("GitHub", [self.days_field()], cx);
        let header = self.header(cx);
        // Penpot `Content`: Header / Sections, padded `space.6`, as the Inbox's.
        div()
            .id("settings")
            .debug_selector(|| "settings view".into())
            .size_full()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(space::S4)
            .p(space::S6)
            .bg(s.background)
            .child(header)
            // Sections 32px apart under 8px; Terminal (#92) comes after GitHub.
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(space::S8)
                    .pt(space::S2)
                    .child(appearance)
                    .child(github),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use agentdeck_client::types::{AppearanceSettings, PrSyncSettings};
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
                }]
            );
            assert_eq!(
                harness.api.settings_now().appearance.theme,
                ThemePreference::Light
            );
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
        }

        #[gpui::test]
        fn should_ignore_other_changes(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(default_settings(), cx);
            let reads = harness.api.settings_reads();

            harness
                .remote
                .events
                .try_send(ws::Event::Message(ServerMessage::Changed {
                    entity: Changed::Task,
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
        fn should_lay_the_sections_out_480px_wide_and_32px_apart(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(default_settings(), cx);

            let appearance = cx
                .debug_bounds("settings section Appearance")
                .expect("Appearance");
            let github = cx.debug_bounds("settings section GitHub").expect("GitHub");

            assert_eq!(appearance.size.width, SECTION_WIDTH);
            assert_eq!(github.size.width, SECTION_WIDTH);
            assert_eq!(github.top() - appearance.bottom(), space::S8);
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
