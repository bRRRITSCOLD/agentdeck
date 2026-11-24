//! Settings → GitHub (spec §8, §11), Penpot "Screens" → "Settings · GitHub · …": a card
//! saying where the daemon's GitHub token comes from, with Sign in with GitHub, Paste token or
//! Sign out beside it (all three for a saved token GitHub rejected), and the write-only OAuth client ID field (Penpot part
//! `agentdeck/ClientIdField`).
//!
//! It shows `GET /v1/github/auth` and reads it again on each `github_auth` event, which also
//! tells the sign-in dialog how a sign-in ended. The age cutoff, the section's last field,
//! stays the Settings view's.

pub mod paste_token;
pub mod sign_in;

use std::sync::Arc;

use agentdeck_client::ApiError;
use agentdeck_client::types::{GithubAuthInfo, TokenEnvVar, TokenSource};
use agentdeck_core::ws::ServerMessage;
use brrritscold_core_gpui::{
    ActiveTheme as _, Button, ButtonSize, ButtonVariant, Field, FieldDescription, FieldError,
    FieldLabel, Input, InputState,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Subscription, Window, div,
};
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::connection::{Connection, DaemonEvent};
use crate::notice::{self, Notice};
use paste_token::{PasteTokenDialog, TokenSaved};
use sign_in::SignInDialog;

/// Under the section's heading.
pub const KEYRING_NOTE: &str =
    "Your token and client ID are saved in the system keyring, never in config.toml.";

/// The client ID field's description.
pub const CLIENT_ID_DESCRIPTION: &str = "From the OAuth App you registered on GitHub, with Device Flow on. Sign in with GitHub needs it.";

/// The client ID input's placeholder while one is saved.
pub const CLIENT_ID_SAVED: &str = "Saved in the keyring";

/// The card's hint for a saved token GitHub rejected.
pub const REJECTED_HINT: &str = "It was revoked or expired. Sign in again or use another token.";

/// Whether the saved token is one GitHub rejected (revoked or expired): agentdeck's to replace
/// or delete.
fn saved_but_rejected(auth: &GithubAuthInfo) -> bool {
    auth.source == TokenSource::Keyring && auth.rejected
}

/// The card's first line: who the token signs in, that none does, or that GitHub rejected the
/// saved one.
pub fn login_line(auth: &GithubAuthInfo) -> String {
    match &auth.login {
        Some(login) => format!("Signed in as {login}"),
        None if saved_but_rejected(auth) => "GitHub rejected your saved token".to_owned(),
        None => "Not signed in to GitHub".to_owned(),
    }
}

/// The variable's name, as the daemon reports it.
fn var_name(var: TokenEnvVar) -> &'static str {
    match var {
        TokenEnvVar::GhToken => "GH_TOKEN",
        TokenEnvVar::GithubToken => "GITHUB_TOKEN",
    }
}

/// Where the token comes from, under the card's first line; `None` without a token, and for
/// a saved token GitHub rejected, whose first line already says so.
pub fn source_line(auth: &GithubAuthInfo) -> Option<String> {
    if saved_but_rejected(auth) {
        return None;
    }
    match auth.source {
        TokenSource::Keyring => Some("With your saved token.".to_owned()),
        TokenSource::Env => Some(format!(
            "With {} from agentdeckd's environment.",
            var_name(auth.env_var.unwrap_or(TokenEnvVar::GhToken))
        )),
        TokenSource::Gh => Some("With your gh login.".to_owned()),
        TokenSource::None => None,
    }
}

/// What to do instead of Sign out, for a token that isn't agentdeck's to delete, or what to
/// do about a saved token GitHub rejected.
pub fn hint(auth: &GithubAuthInfo) -> Option<String> {
    if saved_but_rejected(auth) {
        return Some(REJECTED_HINT.to_owned());
    }
    match auth.source {
        TokenSource::Env => Some(format!(
            "Unset {}",
            var_name(auth.env_var.unwrap_or(TokenEnvVar::GhToken))
        )),
        TokenSource::Gh => Some("Run gh auth logout".to_owned()),
        TokenSource::Keyring | TokenSource::None => None,
    }
}

/// "Your saved token overrides GH_TOKEN", while both give one.
pub fn override_line(auth: &GithubAuthInfo) -> Option<String> {
    match (auth.source, auth.env_var) {
        (TokenSource::Keyring, Some(var)) => {
            Some(format!("Your saved token overrides {}", var_name(var)))
        }
        _ => None,
    }
}

/// What the token's scopes leave out, for a signed-in token: a classic token without `repo`
/// or `public_repo` can't close PRs, and a fine-grained one (no scopes) only on repos its
/// owner owns.
pub fn scope_note(auth: &GithubAuthInfo) -> Option<&'static str> {
    auth.login.as_ref()?;
    match &auth.scopes {
        Some(scopes) if !scopes.iter().any(|s| s == "repo" || s == "public_repo") => {
            Some("PR sync works; closing PRs needs repo")
        }
        Some(_) => None,
        None => Some("Closing works only on repos you own"),
    }
}

/// The label over the actions beside the card. Sign out acts on the card's token, so it needs
/// none; beside a gh or env token, the actions save agentdeck's own, which then overrides it.
/// A saved token GitHub rejected gives no token, as `none`.
pub fn actions_label(auth: &GithubAuthInfo) -> Option<&'static str> {
    if saved_but_rejected(auth) {
        return Some("Sign in");
    }
    match auth.source {
        TokenSource::Keyring => None,
        TokenSource::Env | TokenSource::Gh => Some("Use your own token"),
        TokenSource::None => Some("Sign in"),
    }
}

/// The section, kept by the Settings view.
pub struct GithubSection {
    api: Arc<dyn Api>,
    /// As last read; `None` until the first read lands.
    auth: Option<GithubAuthInfo>,
    /// The client ID being typed, masked.
    client_id: Entity<InputState>,
    /// Why the client ID typed wasn't saved: the daemon's `no_keyring` or
    /// `invalid_client_id`.
    client_id_error: Option<SharedString>,
    paste_token: Entity<PasteTokenDialog>,
    sign_in: Entity<SignInDialog>,
    _subscriptions: Vec<Subscription>,
}

impl GithubSection {
    /// A section over `api` that reads where the token comes from now, and again on each of
    /// `connection`'s `github_auth`.
    pub fn new(
        api: Arc<dyn Api>,
        connection: &Entity<Connection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let client_id = cx.new(|cx| InputState::new(window, cx).masked(true));
        let paste_token = cx.new(|cx| PasteTokenDialog::new(api.clone(), window, cx));
        let sign_in = cx.new(|_| SignInDialog::new(api.clone()));
        let _subscriptions = vec![
            cx.subscribe_in(
                &client_id,
                window,
                |this, _, event, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.save_client_id(window, cx),
                    InputEvent::Change => {
                        this.client_id_error = None;
                        cx.notify();
                    }
                    InputEvent::Blur | InputEvent::Focus => {}
                },
            ),
            cx.subscribe_in(
                connection,
                window,
                |this, _, event, window, cx| match event {
                    DaemonEvent::Message(ServerMessage::GithubAuth { device, message }) => {
                        if let Some(outcome) = device {
                            let message = message.clone();
                            this.sign_in.update(cx, |dialog, cx| {
                                dialog.ended(*outcome, message, window, cx);
                            });
                        }
                        this.reread(window, cx);
                    }
                    DaemonEvent::Message(ServerMessage::Resync) => this.reread(window, cx),
                    _ => {}
                },
            ),
            cx.subscribe_in(
                &paste_token,
                window,
                |this, _, saved: &TokenSaved, window, cx| {
                    this.show(saved.0.clone(), window, cx);
                },
            ),
        ];
        let mut section = GithubSection {
            api,
            auth: None,
            client_id,
            client_id_error: None,
            paste_token,
            sign_in,
            _subscriptions,
        };
        section.reread(window, cx);
        section
    }

    /// Where the token comes from, as last read.
    #[cfg(test)]
    pub fn auth(&self) -> Option<&GithubAuthInfo> {
        self.auth.as_ref()
    }

    /// Why the client ID typed wasn't saved, if it wasn't.
    #[cfg(test)]
    pub fn client_id_error(&self) -> Option<&SharedString> {
        self.client_id_error.as_ref()
    }

    /// The Paste token dialog.
    #[cfg(test)]
    pub fn paste_token(&self) -> &Entity<PasteTokenDialog> {
        &self.paste_token
    }

    /// The sign-in dialog.
    #[cfg(test)]
    pub fn sign_in(&self) -> &Entity<SignInDialog> {
        &self.sign_in
    }

    /// Reads where the token comes from, and shows it.
    pub fn reread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reading = self.api.github_auth(cx);
        cx.spawn_in(window, async move |this, cx| {
            let read = reading.await;
            let _ = this.update_in(cx, |this, window, cx| match read {
                Ok(auth) => this.show(auth, window, cx),
                Err(err) => {
                    Notice::error("Couldn't read the GitHub account", notice::message(&err))
                        .show(window, cx);
                }
            });
        })
        .detach();
    }

    fn show(&mut self, auth: GithubAuthInfo, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = if auth.client_id_set {
            CLIENT_ID_SAVED
        } else {
            ""
        };
        self.client_id.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx);
        });
        self.auth = Some(auth);
        cx.notify();
    }

    /// Opens the Paste token dialog.
    pub fn open_paste_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.paste_token
            .update(cx, |dialog, cx| dialog.open(window, cx));
    }

    /// Opens the sign-in dialog, which starts a sign-in.
    pub fn open_sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sign_in
            .update(cx, |dialog, cx| dialog.open(window, cx));
    }

    /// Deletes the saved token: Sign out.
    pub fn sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let signing_out = self.api.sign_out(cx);
        cx.spawn_in(window, async move |this, cx| {
            let signed_out = signing_out.await;
            let _ = this.update_in(cx, |this, window, cx| match signed_out {
                Ok(auth) => this.show(auth, window, cx),
                Err(err) => {
                    Notice::error("Couldn't sign out", notice::message(&err)).show(window, cx);
                }
            });
        })
        .detach();
    }

    /// Saves the client ID typed and empties the input: what Save and Enter do. A blank one
    /// isn't sent.
    pub fn save_client_id(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.client_id.read(cx).value().trim().to_owned();
        if typed.is_empty() {
            return;
        }
        let saving = self.api.save_client_id(typed, cx);
        cx.spawn_in(window, async move |this, cx| {
            let saved = saving.await;
            let _ = this.update_in(cx, |this, window, cx| match saved {
                Ok(auth) => {
                    this.client_id_error = None;
                    this.client_id
                        .update(cx, |input, cx| input.set_value("", window, cx));
                    this.show(auth, window, cx);
                }
                Err(err) => match err.downcast_ref::<ApiError>() {
                    Some(refusal)
                        if matches!(
                            refusal.code.as_deref(),
                            Some("no_keyring" | "invalid_client_id")
                        ) =>
                    {
                        this.client_id_error = Some(refusal.message.clone().into());
                        cx.notify();
                    }
                    _ => Notice::error("Couldn't save the client ID", notice::message(&err))
                        .show(window, cx),
                },
            });
        })
        .detach();
    }

    /// Deletes the saved client ID: what Clear does.
    pub fn clear_client_id(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let clearing = self.api.clear_client_id(cx);
        cx.spawn_in(window, async move |this, cx| {
            let cleared = clearing.await;
            let _ = this.update_in(cx, |this, window, cx| match cleared {
                Ok(auth) => this.show(auth, window, cx),
                Err(err) => Notice::error("Couldn't clear the client ID", notice::message(&err))
                    .show(window, cx),
            });
        })
        .detach();
    }

    /// Penpot `Heading`: the section's heading over the keyring note.
    fn heading(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        div()
            .flex()
            .flex_col()
            .gap(space::S1)
            .child(
                div()
                    .text_size(font_size::BASE)
                    .font_weight(font_weight::SEMIBOLD)
                    .text_color(s.foreground)
                    .child("GitHub"),
            )
            .child(
                div()
                    .text_size(font_size::SM)
                    .text_color(s.muted_foreground)
                    .child(KEYRING_NOTE),
            )
    }

    /// Penpot `Account`: the `Status` card, holding the token's lines, then the `Actions`
    /// column outside it on the right. The row stretches, so the card is never shorter than
    /// the column.
    fn account(&self, auth: &GithubAuthInfo, cx: &mut Context<Self>) -> gpui::AnyElement {
        let s = cx.theme().semantic();
        let muted = |text: String, selector: &'static str| {
            div()
                .debug_selector(move || selector.into())
                .text_size(font_size::SM)
                .text_color(s.muted_foreground)
                .child(text)
        };
        let status = div()
            .debug_selector(|| "github status".into())
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(space::S0_5)
            .p(space::S3)
            .rounded(s.radius_md)
            .border_1()
            .border_color(s.border)
            .child(
                div()
                    .debug_selector(|| "github login".into())
                    .text_size(font_size::SM)
                    .font_weight(font_weight::MEDIUM)
                    .text_color(s.foreground)
                    .child(login_line(auth)),
            )
            .when_some(source_line(auth), |lines, line| {
                lines.child(muted(line, "github source"))
            })
            .when_some(hint(auth), |lines, line| {
                lines.child(muted(line, "github hint"))
            })
            .when_some(override_line(auth), |lines, line| {
                lines.child(muted(line, "github override"))
            })
            .when_some(scope_note(auth), |lines, note| {
                lines.child(muted(note.to_owned(), "github scope note"))
            });
        // Stretched, so Sign in with GitHub and Paste token share the wider one's width.
        let actions = div()
            .debug_selector(|| "github actions".into())
            .flex_none()
            .flex()
            .flex_col()
            .gap(space::S2)
            .when_some(actions_label(auth), |actions, label| {
                actions.child(
                    div()
                        .debug_selector(|| "github actions label".into())
                        .text_size(font_size::SM)
                        .font_weight(font_weight::MEDIUM)
                        .text_color(s.muted_foreground)
                        .child(label),
                )
            });
        // Under Sign in and Paste token for a saved token GitHub rejected, as wide as they are.
        let sign_out = |cx: &mut Context<Self>| {
            div().debug_selector(|| "github sign out".into()).child(
                Button::new("github-sign-out")
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Sm)
                    .label("Sign out")
                    .w_full()
                    .on_click(cx.listener(|this, _, window, cx| this.sign_out(window, cx))),
            )
        };
        let actions = if auth.source == TokenSource::Keyring && !auth.rejected {
            actions.child(sign_out(cx))
        } else {
            actions
                .when(auth.client_id_set, |actions| {
                    actions.child(
                        div().debug_selector(|| "github sign in".into()).child(
                            Button::new("github-sign-in")
                                .size(ButtonSize::Sm)
                                .label("Sign in with GitHub")
                                .w_full()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_sign_in(window, cx);
                                })),
                        ),
                    )
                })
                .child(
                    div().debug_selector(|| "github paste token".into()).child(
                        Button::new("github-paste-token")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm)
                            .label("Paste token")
                            .w_full()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_paste_token(window, cx);
                            })),
                    ),
                )
                .when(saved_but_rejected(auth), |actions| {
                    actions.child(sign_out(cx))
                })
        };
        div()
            .debug_selector(|| "github account".into())
            .flex()
            .items_stretch()
            .gap(space::S4)
            .child(status)
            .child(actions)
            .into_any_element()
    }

    /// Penpot `agentdeck/ClientIdField`: the masked input, Save and Clear.
    fn client_id_field(&self, auth: &GithubAuthInfo, cx: &mut Context<Self>) -> gpui::AnyElement {
        let blank = self.client_id.read(cx).value().trim().is_empty();
        let error = self.client_id_error.clone();
        Field::new()
            .child(FieldLabel::new().child("OAuth client ID"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::S2)
                    .child(
                        div()
                            .debug_selector(|| "github client id".into())
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.client_id).aria_invalid(error.is_some())),
                    )
                    .child(
                        div()
                            .debug_selector(|| "github client id save".into())
                            .child(
                                Button::new("github-client-id-save")
                                    .variant(ButtonVariant::Outline)
                                    .size(ButtonSize::Sm)
                                    .label("Save")
                                    .disabled(blank)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save_client_id(window, cx);
                                    })),
                            ),
                    )
                    .when(auth.client_id_set, |row| {
                        row.child(
                            div()
                                .debug_selector(|| "github client id clear".into())
                                .child(
                                    Button::new("github-client-id-clear")
                                        .variant(ButtonVariant::Ghost)
                                        .size(ButtonSize::Sm)
                                        .label("Clear")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.clear_client_id(window, cx);
                                        })),
                                ),
                        )
                    }),
            )
            .child(FieldDescription::new(CLIENT_ID_DESCRIPTION))
            .when_some(error, |field, why| {
                field.child(
                    div()
                        .debug_selector(|| "github client id error".into())
                        .child(FieldError::new(why)),
                )
            })
            .into_any_element()
    }
}

impl Render for GithubSection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let auth = self.auth.clone();
        // The dialogs sit outside the column, so its gaps don't count them.
        div()
            .debug_selector(|| "github".into())
            .child(
                div()
                    .debug_selector(|| "github section".into())
                    .flex()
                    .flex_col()
                    .gap(space::S4)
                    .child(self.heading(cx))
                    .when_some(auth, |section, auth| {
                        let account = self.account(&auth, cx);
                        let field = self.client_id_field(&auth, cx);
                        section.child(account).child(field)
                    }),
            )
            .child(self.paste_token.clone())
            .child(self.sign_in.clone())
    }
}

#[cfg(test)]
pub mod fixtures {
    use super::*;

    /// Signed in as octocat with a saved classic token with `repo`, a client ID saved.
    pub fn saved_token() -> GithubAuthInfo {
        GithubAuthInfo {
            source: TokenSource::Keyring,
            env_var: None,
            login: Some("octocat".into()),
            scopes: Some(vec!["repo".into()]),
            rejected: false,
            client_id_set: true,
        }
    }

    /// A saved token GitHub rejected (revoked or expired), a client ID saved.
    pub fn rejected_token() -> GithubAuthInfo {
        GithubAuthInfo {
            login: None,
            scopes: None,
            rejected: true,
            ..saved_token()
        }
    }

    /// No token from any source, and no client ID.
    pub fn signed_out() -> GithubAuthInfo {
        crate::api::fake::signed_out()
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use agentdeck_client::ws::{self, Remote};
    use agentdeck_core::ws::DeviceOutcome;
    use brrritscold_core_gpui::Root;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    use super::fixtures::{rejected_token, saved_token, signed_out};
    use super::sign_in::Stage;
    use super::*;
    use crate::api::fake::FakeApi;
    use crate::connection::fake::FakeConnector;

    struct Harness {
        api: Arc<FakeApi>,
        view: Entity<GithubSection>,
        remote: Remote,
        _connection: Entity<Connection>,
    }

    impl Harness {
        /// A `Root` window on the section, over a daemon whose GitHub auth is `auth`.
        fn open(
            auth: GithubAuthInfo,
            cx: &mut TestAppContext,
        ) -> (Harness, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            cx.update(crate::keymap::init);
            let api = Arc::new(FakeApi::default());
            api.set_github(auth);
            let connector = Rc::new(FakeConnector::default());
            let remote = connector.then_connect_to(api.clone());
            let connection = cx.update(|cx| cx.new(|cx| Connection::new(connector, cx)));
            let mut view = None;
            let window = cx.add_window(|window, cx| {
                let api: Arc<dyn Api> = api.clone();
                let section = cx.new(|cx| GithubSection::new(api, &connection, window, cx));
                view = Some(section.clone());
                Root::new(section, window, cx)
            });
            let cx = VisualTestContext::from_window(*window, cx).into_mut();
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

        fn click(&self, selector: &'static str, cx: &mut VisualTestContext) {
            let bounds = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} is drawn"));
            cx.simulate_click(bounds.center(), Modifiers::default());
            self.settle(cx);
        }

        fn shows(&self, selector: &'static str, cx: &mut VisualTestContext) -> bool {
            cx.debug_bounds(selector).is_some()
        }

        fn github_auth(
            &self,
            device: Option<DeviceOutcome>,
            message: Option<&str>,
            cx: &mut VisualTestContext,
        ) {
            self.remote
                .events
                .try_send(ws::Event::Message(ServerMessage::GithubAuth {
                    device,
                    message: message.map(str::to_owned),
                }))
                .unwrap();
            self.settle(cx);
        }

        fn type_client_id(&self, text: &str, cx: &mut VisualTestContext) {
            cx.update(|window, cx| {
                let input = self.view.read(cx).client_id.clone();
                input.update(cx, |input, cx| {
                    input.focus(window, cx);
                    input.set_value(text.to_owned(), window, cx);
                });
            });
            self.settle(cx);
        }

        fn paste_token(&self, cx: &mut VisualTestContext) -> Entity<PasteTokenDialog> {
            self.view
                .read_with(cx, |view, _| view.paste_token().clone())
        }

        fn sign_in_dialog(&self, cx: &mut VisualTestContext) -> Entity<SignInDialog> {
            self.view.read_with(cx, |view, _| view.sign_in().clone())
        }

        fn stage(&self, cx: &mut VisualTestContext) -> Stage {
            let dialog = self.sign_in_dialog(cx);
            dialog.read_with(cx, |dialog, _| dialog.stage().clone())
        }
    }

    fn env_token() -> GithubAuthInfo {
        GithubAuthInfo {
            source: TokenSource::Env,
            env_var: Some(TokenEnvVar::GhToken),
            scopes: None,
            ..saved_token()
        }
    }

    /// Whether `inner` lies within `outer`, edges included.
    fn inside(inner: gpui::Bounds<gpui::Pixels>, outer: gpui::Bounds<gpui::Pixels>) -> bool {
        inner.left() >= outer.left()
            && inner.top() >= outer.top()
            && inner.right() <= outer.right()
            && inner.bottom() <= outer.bottom()
    }

    fn refusal(code: &str, message: &str) -> anyhow::Error {
        ApiError {
            message: message.to_owned(),
            code: Some(code.to_owned()),
        }
        .into()
    }

    mod account {
        use super::*;

        #[gpui::test]
        fn should_offer_only_sign_out_for_a_saved_token(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(saved_token(), cx);

            assert!(harness.shows("github sign out", cx));
            assert!(!harness.shows("github sign in", cx));
            assert!(!harness.shows("github paste token", cx));
            assert!(!harness.shows("github hint", cx));
            assert!(!harness.shows("github actions label", cx));
        }

        #[gpui::test]
        fn should_offer_sign_in_and_paste_token_over_sign_out_for_a_rejected_saved_token(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(rejected_token(), cx);

            assert!(harness.shows("github actions label", cx));
            assert!(harness.shows("github sign in", cx));
            assert!(harness.shows("github paste token", cx));
            assert!(harness.shows("github sign out", cx));
            assert!(harness.shows("github hint", cx));
            assert!(!harness.shows("github source", cx));
            let sign_in = cx.debug_bounds("github sign in").expect("sign in");
            let paste = cx.debug_bounds("github paste token").expect("paste token");
            let sign_out = cx.debug_bounds("github sign out").expect("sign out");
            assert!(sign_in.top() < paste.top() && paste.top() < sign_out.top());
            assert_eq!(sign_out.size.width, sign_in.size.width);
            assert_eq!(paste.size.width, sign_in.size.width);
        }

        #[gpui::test]
        fn should_offer_sign_in_again_once_github_rejects_the_saved_token(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(saved_token(), cx);
            harness.api.set_github(rejected_token());

            harness.github_auth(None, None, cx);

            assert!(harness.shows("github sign in", cx));
            assert!(harness.shows("github paste token", cx));
            assert!(harness.shows("github sign out", cx));
        }

        #[gpui::test]
        fn should_offer_sign_in_and_paste_token_with_a_hint_for_an_env_token(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(env_token(), cx);

            assert!(harness.shows("github sign in", cx));
            assert!(harness.shows("github paste token", cx));
            assert!(harness.shows("github hint", cx));
            assert!(harness.shows("github scope note", cx));
            assert!(harness.shows("github actions label", cx));
            assert!(!harness.shows("github sign out", cx));
        }

        #[gpui::test]
        fn should_hold_every_line_in_the_card_and_the_actions_beside_it(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(env_token(), cx);

            let card = cx.debug_bounds("github status").expect("the card");
            for line in [
                "github login",
                "github source",
                "github hint",
                "github scope note",
            ] {
                let bounds = cx.debug_bounds(line).expect(line);
                assert!(inside(bounds, card), "{line} is in the card");
            }
            let actions = cx.debug_bounds("github actions").expect("the actions");
            assert!(actions.left() > card.right());
            assert_eq!(actions.top(), card.top());
            for action in [
                "github actions label",
                "github sign in",
                "github paste token",
            ] {
                let bounds = cx.debug_bounds(action).expect(action);
                assert!(inside(bounds, actions), "{action} is in the actions");
            }
        }

        #[gpui::test]
        fn should_stretch_the_card_to_the_actions_height(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(signed_out(), cx);

            let card = cx.debug_bounds("github status").expect("the card");
            let actions = cx.debug_bounds("github actions").expect("the actions");
            assert_eq!(card.size.height, actions.size.height);
        }

        #[gpui::test]
        fn should_keep_the_cards_own_height_when_its_text_is_taller(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(saved_token(), cx);

            let card = cx.debug_bounds("github status").expect("the card");
            let sign_out = cx.debug_bounds("github sign out").expect("sign out");
            let source = cx.debug_bounds("github source").expect("the source line");
            assert!(card.size.height > sign_out.size.height);
            // Its last line, then the padding and the border.
            assert_eq!(card.bottom(), source.bottom() + space::S3 + gpui::px(1.));
        }

        #[gpui::test]
        fn should_size_sign_in_and_paste_token_alike(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(env_token(), cx);

            let sign_in = cx.debug_bounds("github sign in").expect("sign in");
            let paste = cx.debug_bounds("github paste token").expect("paste token");
            assert_eq!(sign_in.size.width, paste.size.width);
        }

        #[gpui::test]
        fn should_label_the_actions_while_not_signed_in(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(signed_out(), cx);

            assert!(harness.shows("github actions label", cx));
            assert!(harness.shows("github status", cx));
        }

        #[gpui::test]
        fn should_hide_sign_in_until_a_client_id_is_set(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(signed_out(), cx);

            assert!(!harness.shows("github sign in", cx));
            assert!(harness.shows("github paste token", cx));
            assert!(!harness.shows("github client id clear", cx));
        }

        #[gpui::test]
        fn should_take_no_room_for_its_closed_dialogs(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(saved_token(), cx);

            let whole = cx.debug_bounds("github").expect("the section");
            let column = cx.debug_bounds("github section").expect("its column");
            assert_eq!(whole.size.height, column.size.height);
        }

        #[gpui::test]
        fn should_read_again_on_github_auth(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(signed_out(), cx);
            harness.api.set_github(saved_token());

            harness.github_auth(None, None, cx);

            assert_eq!(
                harness.view.read_with(cx, |view, _| view.auth().cloned()),
                Some(saved_token())
            );
            assert!(harness.shows("github sign out", cx));
        }

        #[gpui::test]
        fn should_sign_out(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(saved_token(), cx);

            harness.click("github sign out", cx);

            let auth = harness.view.read_with(cx, |view, _| view.auth().cloned());
            assert_eq!(auth.map(|a| a.source), Some(TokenSource::None));
            assert!(harness.shows("github paste token", cx));
        }
    }

    mod client_id {
        use super::*;

        #[gpui::test]
        fn should_save_it_on_enter_and_empty_the_input(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(signed_out(), cx);
            harness.type_client_id(" Iv1.abc ", cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(*harness.api.saved_client_ids.lock().unwrap(), ["Iv1.abc"]);
            let value = cx.read(|cx| harness.view.read(cx).client_id.read(cx).value().to_string());
            assert_eq!(value, "");
            assert!(harness.shows("github client id clear", cx));
            assert!(harness.shows("github sign in", cx));
        }

        #[gpui::test]
        fn should_not_save_a_blank_one(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(signed_out(), cx);
            harness.type_client_id("  ", cx);

            harness.click("github client id save", cx);

            assert!(harness.api.saved_client_ids.lock().unwrap().is_empty());
        }

        #[gpui::test]
        fn should_show_no_keyring_as_the_fields_error(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(signed_out(), cx);
            harness.type_client_id("Iv1.abc", cx);
            harness
                .api
                .refuse_next("no_keyring", "No system keyring is running.");

            harness.click("github client id save", cx);

            assert_eq!(
                harness
                    .view
                    .read_with(cx, |view, _| view.client_id_error().cloned()),
                Some("No system keyring is running.".into())
            );
            assert!(harness.shows("github client id error", cx));
        }

        #[gpui::test]
        fn should_show_a_sign_in_code_refused_as_the_fields_error(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(signed_out(), cx);
            harness.type_client_id("WDJB-MJHT", cx);
            harness.api.refuse_next(
                "invalid_client_id",
                "That looks like the sign-in code, not a client ID.",
            );

            harness.click("github client id save", cx);

            assert_eq!(
                harness
                    .view
                    .read_with(cx, |view, _| view.client_id_error().cloned()),
                Some("That looks like the sign-in code, not a client ID.".into())
            );
            assert!(harness.shows("github client id error", cx));
            assert!(cx.read(notice::shown).is_empty());
        }

        #[gpui::test]
        fn should_clear_it(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(saved_token(), cx);

            harness.click("github client id clear", cx);

            assert!(!harness.shows("github client id clear", cx));
            assert!(!harness.api.github_now().client_id_set);
        }
    }

    mod paste_token {
        use super::*;

        fn open_and_type(harness: &Harness, token: &str, cx: &mut VisualTestContext) {
            harness.click("github paste token", cx);
            let dialog = harness.paste_token(cx);
            cx.update(|window, cx| {
                dialog.update(cx, |dialog, cx| dialog.type_token(token, window, cx));
            });
            harness.settle(cx);
        }

        #[gpui::test]
        fn should_save_close_and_toast_who_with_the_scope_note(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(signed_out(), cx);
            *harness.api.token_login.lock().unwrap() =
                ("octocat".into(), Some(vec!["read:org".into()]));
            open_and_type(&harness, "ghp_x", cx);

            harness.click("paste token save", cx);

            let dialog = harness.paste_token(cx);
            assert!(!dialog.read_with(cx, |dialog, _| dialog.is_open()));
            assert_eq!(*harness.api.saved_tokens.lock().unwrap(), ["ghp_x"]);
            let toast = cx.read(notice::shown).pop().expect("a toast");
            assert_eq!(toast.title, "Signed in as octocat");
            assert_eq!(
                toast.description.as_deref(),
                Some("PR sync works; closing PRs needs repo")
            );
            assert!(harness.shows("github sign out", cx));
        }

        #[gpui::test]
        fn should_save_and_close_on_enter(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(signed_out(), cx);
            harness.click("github paste token", cx);
            cx.simulate_input("ghp_x");
            harness.settle(cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            let dialog = harness.paste_token(cx);
            assert!(!dialog.read_with(cx, |dialog, _| dialog.is_open()));
            assert_eq!(*harness.api.saved_tokens.lock().unwrap(), ["ghp_x"]);
        }

        #[gpui::test]
        fn should_close_without_saving_on_escape(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(signed_out(), cx);
            harness.click("github paste token", cx);
            cx.simulate_input("ghp_x");
            harness.settle(cx);

            cx.simulate_keystrokes("escape");
            harness.settle(cx);

            let dialog = harness.paste_token(cx);
            assert!(!dialog.read_with(cx, |dialog, _| dialog.is_open()));
            assert!(harness.api.saved_tokens.lock().unwrap().is_empty());
        }

        #[gpui::test]
        fn should_stay_open_on_enter_with_no_token(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(signed_out(), cx);
            open_and_type(&harness, "  ", cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            let dialog = harness.paste_token(cx);
            assert!(dialog.read_with(cx, |dialog, _| dialog.is_open()));
            assert!(harness.api.saved_tokens.lock().unwrap().is_empty());
        }

        #[gpui::test]
        fn should_stay_open_with_the_refusal_on_enter(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(signed_out(), cx);
            open_and_type(&harness, "ghp_bad", cx);
            harness
                .api
                .refuse_next("invalid_token", "GitHub didn't accept this token.");

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            let dialog = harness.paste_token(cx);
            let (open, error) =
                dialog.read_with(cx, |dialog, _| (dialog.is_open(), dialog.error().cloned()));
            assert!(open);
            assert_eq!(error, Some("GitHub didn't accept this token.".into()));
        }

        #[gpui::test]
        fn should_replace_a_rejected_saved_token(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(rejected_token(), cx);
            open_and_type(&harness, "ghp_x", cx);

            harness.click("paste token save", cx);

            let auth = harness.view.read_with(cx, |view, _| view.auth().cloned());
            assert_eq!(auth.map(|auth| auth.rejected), Some(false));
            assert!(harness.shows("github sign out", cx));
            assert!(!harness.shows("github paste token", cx));
            assert!(!harness.shows("github hint", cx));
        }

        #[gpui::test]
        fn should_show_a_refused_token_under_the_field_and_stay_open(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(signed_out(), cx);
            open_and_type(&harness, "ghp_bad", cx);
            harness
                .api
                .refuse_next("invalid_token", "GitHub didn't accept this token.");

            harness.click("paste token save", cx);

            let dialog = harness.paste_token(cx);
            let (open, error) =
                dialog.read_with(cx, |dialog, _| (dialog.is_open(), dialog.error().cloned()));
            assert!(open);
            assert_eq!(error, Some("GitHub didn't accept this token.".into()));
            assert!(harness.shows("paste token error", cx));
        }

        #[gpui::test]
        fn should_toast_any_other_failure_and_stay_open(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(signed_out(), cx);
            open_and_type(&harness, "ghp_x", cx);
            harness
                .api
                .refuse_next("github_error", "GitHub answered 503");

            harness.click("paste token save", cx);

            let dialog = harness.paste_token(cx);
            assert!(dialog.read_with(cx, |dialog, _| dialog.is_open()));
            let toast = cx.read(notice::shown).pop().expect("a toast");
            assert_eq!(toast.title, "Couldn't save the token");
            assert_eq!(toast.description.as_deref(), Some("GitHub answered 503"));
        }
    }

    mod sign_in {
        use super::*;

        fn signing_in(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            let (harness, cx) = Harness::open(
                GithubAuthInfo {
                    client_id_set: true,
                    ..signed_out()
                },
                cx,
            );
            harness.click("github sign in", cx);
            (harness, cx)
        }

        #[gpui::test]
        fn should_start_a_sign_in_and_show_the_code(cx: &mut TestAppContext) {
            let (harness, cx) = signing_in(cx);

            assert_eq!(*harness.api.sign_ins.lock().unwrap(), 1);
            assert!(
                matches!(harness.stage(cx), Stage::Waiting { code, .. } if code == "WDJB-MJHT")
            );
            assert!(harness.shows("sign in copy", cx));
            assert!(harness.shows("sign in waiting", cx));
            assert!(harness.shows("sign in expires", cx));
        }

        #[gpui::test]
        fn should_fit_the_waiting_line_and_the_expiry_on_one_line(cx: &mut TestAppContext) {
            let (_harness, cx) = signing_in(cx);

            let waiting = cx
                .debug_bounds("sign in waiting")
                .expect("the waiting line");
            let expires = cx.debug_bounds("sign in expires").expect("the expiry");
            assert!(waiting.right() <= expires.left(), "{waiting:?} {expires:?}");
            assert_eq!(waiting.size.height, expires.size.height);
        }

        #[gpui::test]
        fn should_copy_the_code_then_open_github_through_the_opener(cx: &mut TestAppContext) {
            let (harness, cx) = signing_in(cx);
            let opener = cx.update(|_, cx| crate::terminal::open::fake::install(cx));

            harness.click("sign in copy", cx);

            let copied = cx.read_from_clipboard().and_then(|item| item.text());
            assert_eq!(copied.as_deref(), Some("WDJB-MJHT"));
            assert_eq!(
                *opener.started.lock().unwrap(),
                [["xdg-open", "https://github.com/login/device"]]
            );
            assert_eq!(cx.opened_url(), None, "not through GPUI's open_url");
        }

        #[gpui::test]
        fn should_copy_the_code_then_open_github_on_enter(cx: &mut TestAppContext) {
            let (harness, cx) = signing_in(cx);
            let opener = cx.update(|_, cx| crate::terminal::open::fake::install(cx));

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            let copied = cx.read_from_clipboard().and_then(|item| item.text());
            assert_eq!(copied.as_deref(), Some("WDJB-MJHT"));
            assert_eq!(
                *opener.started.lock().unwrap(),
                [["xdg-open", "https://github.com/login/device"]]
            );
            let dialog = harness.sign_in_dialog(cx);
            assert!(dialog.read_with(cx, |dialog, _| dialog.is_open()));
        }

        #[gpui::test]
        fn should_try_again_on_enter_once_the_code_expired(cx: &mut TestAppContext) {
            let (harness, cx) = signing_in(cx);
            harness.github_auth(Some(DeviceOutcome::Expired), None, cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(*harness.api.sign_ins.lock().unwrap(), 2);
            assert!(matches!(harness.stage(cx), Stage::Waiting { .. }));
        }

        #[gpui::test]
        fn should_toast_when_github_does_not_open(cx: &mut TestAppContext) {
            let (harness, cx) = signing_in(cx);
            let opener = cx.update(|_, cx| crate::terminal::open::fake::install(cx));
            *opener.failure.lock().unwrap() = Some("No such file or directory".into());

            harness.click("sign in copy", cx);

            let toast = cx.read(notice::shown).pop().expect("a toast");
            assert_eq!(toast.title, "Couldn't open GitHub");
            assert_eq!(
                toast.description.as_deref(),
                Some("xdg-open: No such file or directory")
            );
        }

        #[gpui::test]
        fn should_close_and_toast_once_signed_in(cx: &mut TestAppContext) {
            let (harness, cx) = signing_in(cx);
            harness.api.set_github(saved_token());

            harness.github_auth(Some(DeviceOutcome::SignedIn), None, cx);

            let dialog = harness.sign_in_dialog(cx);
            assert!(!dialog.read_with(cx, |dialog, _| dialog.is_open()));
            let toast = cx.read(notice::shown).pop().expect("a toast");
            assert_eq!(toast.title, "Signed in as octocat");
            assert_eq!(*harness.api.cancelled_sign_ins.lock().unwrap(), 0);
        }

        #[gpui::test]
        fn should_offer_try_again_once_the_code_expired(cx: &mut TestAppContext) {
            let (harness, cx) = signing_in(cx);

            harness.github_auth(Some(DeviceOutcome::Expired), None, cx);

            assert_eq!(
                harness.stage(cx),
                Stage::Ended {
                    code: "WDJB-MJHT".into(),
                    message: "This code expired.".into()
                }
            );
            assert!(harness.shows("sign in try again", cx));
            assert!(!harness.shows("sign in copy", cx));

            harness.click("sign in try again", cx);

            assert_eq!(*harness.api.sign_ins.lock().unwrap(), 2);
            assert!(matches!(harness.stage(cx), Stage::Waiting { .. }));
        }

        #[gpui::test]
        fn should_show_githubs_message_when_it_failed(cx: &mut TestAppContext) {
            let (harness, cx) = signing_in(cx);

            harness.github_auth(
                Some(DeviceOutcome::Failed),
                Some("The client_id is not valid."),
                cx,
            );

            assert!(matches!(
                harness.stage(cx),
                Stage::Ended { message, .. } if message == "The client_id is not valid."
            ));
        }

        #[gpui::test]
        fn should_show_a_refused_start_in_place_of_the_code(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(
                GithubAuthInfo {
                    client_id_set: true,
                    ..signed_out()
                },
                cx,
            );
            harness
                .api
                .refuse_next("github_error", "Device Flow must be explicitly enabled");

            harness.click("github sign in", cx);

            assert_eq!(
                harness.stage(cx),
                Stage::Refused("Device Flow must be explicitly enabled".into())
            );
            assert!(!harness.shows("sign in code", cx));
            assert!(harness.shows("sign in try again", cx));
            let _ = refusal;
        }

        #[gpui::test]
        fn should_cancel_the_sign_in_on_cancel(cx: &mut TestAppContext) {
            let (harness, cx) = signing_in(cx);

            harness.click("sign in cancel", cx);

            let dialog = harness.sign_in_dialog(cx);
            assert!(!dialog.read_with(cx, |dialog, _| dialog.is_open()));
            assert_eq!(*harness.api.cancelled_sign_ins.lock().unwrap(), 1);
        }

        #[gpui::test]
        fn should_cancel_the_sign_in_on_escape(cx: &mut TestAppContext) {
            let (harness, cx) = signing_in(cx);

            cx.simulate_keystrokes("escape");
            harness.settle(cx);
            harness.settle(cx);

            let dialog = harness.sign_in_dialog(cx);
            assert!(!dialog.read_with(cx, |dialog, _| dialog.is_open()));
            assert_eq!(*harness.api.cancelled_sign_ins.lock().unwrap(), 1);
        }
    }

    mod lines {
        use super::*;

        #[test]
        fn should_say_who_a_token_signs_in_and_where_it_comes_from() {
            let saved = saved_token();
            let env = GithubAuthInfo {
                source: TokenSource::Env,
                env_var: Some(TokenEnvVar::GithubToken),
                ..saved_token()
            };
            let gh = GithubAuthInfo {
                source: TokenSource::Gh,
                ..saved_token()
            };

            assert_eq!(login_line(&saved), "Signed in as octocat");
            assert_eq!(
                source_line(&saved).as_deref(),
                Some("With your saved token.")
            );
            assert_eq!(
                source_line(&env).as_deref(),
                Some("With GITHUB_TOKEN from agentdeckd's environment.")
            );
            assert_eq!(source_line(&gh).as_deref(), Some("With your gh login."));
        }

        #[test]
        fn should_say_not_signed_in_without_a_login() {
            assert_eq!(login_line(&signed_out()), "Not signed in to GitHub");
            assert_eq!(source_line(&signed_out()), None);
        }

        #[test]
        fn should_say_github_rejected_a_saved_token_and_what_to_do() {
            let rejected = rejected_token();

            assert_eq!(login_line(&rejected), "GitHub rejected your saved token");
            assert_eq!(source_line(&rejected), None);
            assert_eq!(
                hint(&rejected).as_deref(),
                Some("It was revoked or expired. Sign in again or use another token.")
            );
            assert_eq!(actions_label(&rejected), Some("Sign in"));
            assert_eq!(scope_note(&rejected), None);
        }

        #[test]
        fn should_still_say_a_rejected_saved_token_overrides_the_variable() {
            let both = GithubAuthInfo {
                env_var: Some(TokenEnvVar::GhToken),
                ..rejected_token()
            };

            assert_eq!(
                override_line(&both).as_deref(),
                Some("Your saved token overrides GH_TOKEN")
            );
        }

        #[test]
        fn should_say_not_signed_in_over_the_source_of_a_rejected_env_token() {
            let env = GithubAuthInfo {
                source: TokenSource::Env,
                env_var: Some(TokenEnvVar::GhToken),
                ..rejected_token()
            };

            assert_eq!(login_line(&env), "Not signed in to GitHub");
            assert_eq!(
                source_line(&env).as_deref(),
                Some("With GH_TOKEN from agentdeckd's environment.")
            );
            assert_eq!(hint(&env).as_deref(), Some("Unset GH_TOKEN"));
            assert_eq!(actions_label(&env), Some("Use your own token"));
        }

        #[test]
        fn should_hint_what_to_do_instead_of_sign_out() {
            let env = GithubAuthInfo {
                source: TokenSource::Env,
                env_var: Some(TokenEnvVar::GhToken),
                ..saved_token()
            };
            let gh = GithubAuthInfo {
                source: TokenSource::Gh,
                ..saved_token()
            };

            assert_eq!(hint(&env).as_deref(), Some("Unset GH_TOKEN"));
            assert_eq!(hint(&gh).as_deref(), Some("Run gh auth logout"));
            assert_eq!(hint(&saved_token()), None);
        }

        #[test]
        fn should_label_the_actions_by_source() {
            let label = |source| {
                actions_label(&GithubAuthInfo {
                    source,
                    ..saved_token()
                })
            };

            assert_eq!(label(TokenSource::Gh), Some("Use your own token"));
            assert_eq!(label(TokenSource::Env), Some("Use your own token"));
            assert_eq!(label(TokenSource::None), Some("Sign in"));
            assert_eq!(label(TokenSource::Keyring), None);
        }

        #[test]
        fn should_say_a_saved_token_overrides_the_variable() {
            let both = GithubAuthInfo {
                env_var: Some(TokenEnvVar::GhToken),
                ..saved_token()
            };

            assert_eq!(
                override_line(&both).as_deref(),
                Some("Your saved token overrides GH_TOKEN")
            );
            assert_eq!(override_line(&saved_token()), None);
        }
    }

    mod scope_note {
        use super::*;

        #[test]
        fn should_say_closing_needs_repo_without_repo_or_public_repo() {
            let auth = GithubAuthInfo {
                scopes: Some(vec!["read:org".into()]),
                ..saved_token()
            };

            assert_eq!(
                scope_note(&auth),
                Some("PR sync works; closing PRs needs repo")
            );
        }

        #[test]
        fn should_say_nothing_with_repo_or_public_repo() {
            let public = GithubAuthInfo {
                scopes: Some(vec!["public_repo".into()]),
                ..saved_token()
            };

            assert_eq!(scope_note(&saved_token()), None);
            assert_eq!(scope_note(&public), None);
        }

        #[test]
        fn should_say_closing_works_only_on_own_repos_for_a_token_without_scopes() {
            let fine_grained = GithubAuthInfo {
                scopes: None,
                ..saved_token()
            };

            assert_eq!(
                scope_note(&fine_grained),
                Some("Closing works only on repos you own")
            );
        }

        #[test]
        fn should_say_nothing_while_not_signed_in() {
            assert_eq!(scope_note(&signed_out()), None);
        }
    }
}
