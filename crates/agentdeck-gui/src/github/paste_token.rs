//! The Paste token dialog (spec §11), Penpot "Screens" → "Paste token" and "Paste token ·
//! invalid", its form the part `agentdeck/PasteTokenForm`.
//!
//! It saves the token with `PUT /v1/github/token`, which checks it with GitHub first, then
//! closes and toasts "Signed in as `<login>`", with a scope note when one applies. GitHub
//! refusing the token, or no keyring, shows under the field and keeps it open; any other
//! failure keeps it open and toasts why.

use std::sync::Arc;

use agentdeck_client::ApiError;
use agentdeck_client::types::GithubAuthInfo;
use brrritscold_core_gpui::{
    Button, ButtonVariant, Dialog, DialogContent, DialogDescription, DialogFooter, DialogHandle,
    DialogHeader, DialogTitle, Field, FieldError, FieldLabel, Input, InputState,
};
use brrritscold_core_gpui_tokens::primitive::space;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
};
use gpui_base::input::InputEvent;

use super::scope_note;
use crate::api::Api;
use crate::notice::{self, Notice};

/// The dialog's description.
pub const DESCRIPTION: &str = "A classic token with the repo scope, or a fine-grained one. agentdeck checks it with GitHub, then saves it in the system keyring.";

/// A token was saved: the account it signs in.
pub struct TokenSaved(pub GithubAuthInfo);

/// The toast a saved token shows: who it signs in, and what its scopes leave out.
pub fn signed_in(auth: &GithubAuthInfo) -> Notice {
    let notice = Notice::new(format!(
        "Signed in as {}",
        auth.login.as_deref().unwrap_or("GitHub")
    ));
    match scope_note(auth) {
        Some(note) => notice.description(note),
        None => notice,
    }
}

/// The dialog, kept by the GitHub section.
pub struct PasteTokenDialog {
    api: Arc<dyn Api>,
    handle: DialogHandle,
    token: Entity<InputState>,
    /// Why the token wasn't saved: GitHub refused it, or there's no keyring.
    error: Option<SharedString>,
    /// Whether a save is out; another waits for it.
    saving: bool,
    /// Whether the next render focuses the input, once the dialog has focused itself.
    focus_token: bool,
    _token: Subscription,
}

impl EventEmitter<TokenSaved> for PasteTokenDialog {}

impl PasteTokenDialog {
    /// A closed dialog that saves through `api`.
    pub fn new(api: Arc<dyn Api>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let token = cx.new(|cx| InputState::new(window, cx).masked(true));
        let _token = cx.subscribe_in(&token, window, |this, _, event, window, cx| match event {
            InputEvent::PressEnter { .. } => this.save(window, cx),
            InputEvent::Change => {
                this.error = None;
                cx.notify();
            }
            InputEvent::Blur | InputEvent::Focus => {}
        });
        PasteTokenDialog {
            api,
            handle: DialogHandle::new(false),
            token,
            error: None,
            saving: false,
            focus_token: false,
            _token,
        }
    }

    /// Opens it empty, the input focused.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        self.token
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.handle.open(window, cx);
        self.focus_token = true;
        cx.notify();
    }

    /// Whether it is open.
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.handle.is_open()
    }

    /// Why the token wasn't saved, if it wasn't.
    #[cfg(test)]
    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    /// Types `token` into the input.
    #[cfg(test)]
    pub fn type_token(&mut self, token: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.token.update(cx, |input, cx| {
            input.set_value(token.to_owned(), window, cx)
        });
    }

    /// Saves the token typed: what Save token and Enter do. A blank one isn't sent.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let token = self.token.read(cx).value().trim().to_owned();
        if token.is_empty() || self.saving {
            return;
        }
        self.saving = true;
        cx.notify();
        let saving = self.api.save_token(token, cx);
        cx.spawn_in(window, async move |this, cx| {
            let saved = saving.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match saved {
                    Ok(auth) => {
                        this.token
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        this.handle.close(window, cx);
                        signed_in(&auth).show(window, cx);
                        cx.emit(TokenSaved(auth));
                    }
                    Err(err) => match err.downcast_ref::<ApiError>() {
                        Some(refusal)
                            if matches!(
                                refusal.code.as_deref(),
                                Some("invalid_token" | "no_keyring")
                            ) =>
                        {
                            this.error = Some(refusal.message.clone().into());
                        }
                        _ => Notice::error("Couldn't save the token", notice::message(&err))
                            .show(window, cx),
                    },
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for PasteTokenDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_token) {
            // The dialog focuses itself in an effect it queues while drawing, after this
            // render; queueing from inside an effect lands after that one.
            let token = self.token.clone();
            window.defer(cx, move |window, cx| {
                window.defer(cx, move |window, cx| {
                    token.update(cx, |input, cx| input.focus(window, cx));
                });
            });
        }
        let blank = self.token.read(cx).value().trim().is_empty();
        let error = self.error.clone();
        // Penpot `agentdeck/PasteTokenForm`: the field, then its error, 16px apart.
        let form = div()
            .flex()
            .flex_col()
            .gap(space::S4)
            .child(
                Field::new().child(FieldLabel::new().child("Token")).child(
                    div()
                        .debug_selector(|| "paste token input".into())
                        .child(Input::new(&self.token).aria_invalid(error.is_some())),
                ),
            )
            .when_some(error, |form, why| {
                form.child(
                    div()
                        .debug_selector(|| "paste token error".into())
                        .child(FieldError::new(why)),
                )
            });
        Dialog::new("paste-token")
            .handle(self.handle.clone())
            .content(
                DialogContent::new()
                    .header(
                        DialogHeader::new()
                            .title(DialogTitle::new("Paste a GitHub token"))
                            .description(DialogDescription::new(DESCRIPTION)),
                    )
                    .child(form)
                    .footer(
                        DialogFooter::new()
                            .child(
                                Button::new("paste-token-cancel")
                                    .variant(ButtonVariant::Outline)
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.handle.close(window, cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div().debug_selector(|| "paste token save".into()).child(
                                    Button::new("paste-token-save")
                                        .label("Save token")
                                        .disabled(blank || self.saving)
                                        .on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.save(window, cx)
                                            }),
                                        ),
                                ),
                            ),
                    ),
            )
    }
}
