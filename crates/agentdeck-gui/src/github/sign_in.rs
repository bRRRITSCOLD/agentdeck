//! The sign-in dialog (spec §8, §11), Penpot "Screens" → "Sign in · waiting", "· expired"
//! and "· denied", its body the part `agentdeck/SignInCode`.
//!
//! It starts a sign-in with `POST /v1/github/device` as it opens and shows the user code.
//! **Copy code & open GitHub** copies it, then opens the page to enter it on; meanwhile
//! "Waiting for approval…" counts the code's expiry down. How it ends comes from the GitHub
//! section's `github_auth` events, through [`SignInDialog::ended`]: signed in closes it and
//! toasts; expired, denied or failed dims the code, puts **Try again** where the copy button
//! was and the message under it. A refused start shows the daemon's message in place of the
//! code, with Try again. Cancel, Escape or the corner button cancels the sign-in.

use std::sync::Arc;
use std::time::Duration;

use agentdeck_client::types::DeviceSignIn;
use agentdeck_core::ws::DeviceOutcome;
use brrritscold_core_gpui::{
    ActiveTheme as _, Button, ButtonVariant, Dialog, DialogContent, DialogDescription,
    DialogFooter, DialogHandle, DialogHeader, DialogTitle, Icon, IconName,
};
use brrritscold_core_gpui_tokens::primitive::{
    border_radius, font_family, font_size, font_weight, letter_spacing, space,
};
use chrono::{DateTime, Utc};
use gpui::{
    ClipboardItem, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Window, div, relative,
};

use super::paste_token::signed_in;
use crate::api::Api;
use crate::notice;

/// The dialog's description.
pub const DESCRIPTION: &str = "Enter this code on GitHub to let agentdeck sync and close your PRs.";

/// What an ended sign-in says under Try again.
pub fn ended_message(outcome: DeviceOutcome, message: Option<&str>) -> SharedString {
    match outcome {
        DeviceOutcome::Expired => "This code expired.".into(),
        DeviceOutcome::Denied => "You denied access on GitHub.".into(),
        DeviceOutcome::Failed | DeviceOutcome::SignedIn => message
            .map(|m| SharedString::from(m.to_owned()))
            .unwrap_or_else(|| "GitHub didn't sign you in.".into()),
    }
}

/// "Expires in 14:32": what's left of the code at `now`, never below zero.
pub fn expires_in(expires_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let left = (expires_at - now).num_seconds().max(0);
    format!("Expires in {}:{:02}", left / 60, left % 60)
}

/// Where a sign-in is.
#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    /// Closed, or no sign-in asked for yet.
    Idle,
    /// Asking the daemon for a code.
    Starting,
    /// Showing the code until the user approves on GitHub or it expires.
    Waiting {
        /// The code to enter.
        code: SharedString,
        /// Where to enter it.
        uri: String,
        /// When it expires.
        expires_at: DateTime<Utc>,
    },
    /// The sign-in ended without signing in: the code, dimmed, and why.
    Ended {
        /// The code that was shown.
        code: SharedString,
        /// Why it ended.
        message: SharedString,
    },
    /// The daemon refused to start one: its message, in place of the code.
    Refused(SharedString),
}

/// The dialog, kept by the GitHub section.
pub struct SignInDialog {
    api: Arc<dyn Api>,
    handle: DialogHandle,
    stage: Stage,
}

impl SignInDialog {
    /// A closed dialog that signs in through `api`.
    pub fn new(api: Arc<dyn Api>) -> Self {
        SignInDialog {
            api,
            handle: DialogHandle::new(false),
            stage: Stage::Idle,
        }
    }

    /// Where the sign-in is.
    #[cfg(test)]
    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    /// Whether it is open.
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.handle.is_open()
    }

    /// Opens it and starts a sign-in.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.handle.open(window, cx);
        self.start(window, cx);
    }

    /// Starts a sign-in: what opening and Try again do.
    pub fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stage = Stage::Starting;
        cx.notify();
        let starting = self.api.start_sign_in(cx);
        cx.spawn_in(window, async move |this, cx| {
            let started = starting.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.stage != Stage::Starting {
                    return;
                }
                this.stage = match started {
                    Ok(sign_in) => waiting(sign_in),
                    Err(err) => Stage::Refused(notice::message(&err)),
                };
                this.tick(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Redraws the countdown every second while the code is shown.
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.stage, Stage::Waiting { .. }) {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let waiting = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        matches!(this.stage, Stage::Waiting { .. })
                    })
                    .unwrap_or(false);
                if !waiting {
                    break;
                }
            }
        })
        .detach();
    }

    /// Copies the code, then opens the page to enter it on.
    pub fn copy_and_open(&mut self, cx: &mut Context<Self>) {
        if let Stage::Waiting { code, uri, .. } = &self.stage {
            cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
            cx.open_url(uri);
        }
    }

    /// How the sign-in running ended, from a `github_auth` event. Signed in closes the dialog
    /// and toasts who; anything else shows why, with Try again. Ignored unless a code is
    /// shown.
    pub fn ended(
        &mut self,
        outcome: DeviceOutcome,
        message: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Stage::Waiting { code, .. } = &self.stage else {
            return;
        };
        if outcome == DeviceOutcome::SignedIn {
            self.stage = Stage::Idle;
            self.handle.close(window, cx);
            let reading = self.api.github_auth(cx);
            cx.spawn_in(window, async move |_, cx| {
                if let Ok(auth) = reading.await {
                    let _ = cx.update(|window, cx| signed_in(&auth).show(window, cx));
                }
            })
            .detach();
        } else {
            self.stage = Stage::Ended {
                code: code.clone(),
                message: ended_message(outcome, message.as_deref()),
            };
        }
        cx.notify();
    }

    /// Cancels the sign-in and closes: what Cancel does.
    pub fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.handle.close(window, cx);
        self.stop(cx);
    }

    /// Tells the daemon to stop polling, if a sign-in may still run.
    fn stop(&mut self, cx: &mut Context<Self>) {
        let running = matches!(self.stage, Stage::Starting | Stage::Waiting { .. });
        self.stage = Stage::Idle;
        cx.notify();
        if running {
            let cancelling = self.api.cancel_sign_in(cx);
            cx.spawn(async move |_, _| {
                if let Err(err) = cancelling.await {
                    tracing::warn!("couldn't cancel the sign-in: {err:#}");
                }
            })
            .detach();
        }
    }

    /// Penpot `agentdeck/SignInCode`.
    fn body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let s = cx.theme().semantic();
        let code_box = |code: SharedString, dimmed: bool| {
            div()
                .debug_selector(|| "sign in code".into())
                .flex()
                .justify_center()
                .items_center()
                .py(space::S4)
                .rounded(border_radius::LG)
                .bg(s.muted)
                .child(
                    // `letter-spacing.widest`: GPUI has no letter spacing, so each letter is
                    // its own box, that far apart.
                    div()
                        .flex()
                        .gap(letter_spacing::WIDEST)
                        .font_family(font_family::MONO.family)
                        .text_size(font_size::F2XL)
                        .font_weight(font_weight::SEMIBOLD)
                        .line_height(relative(1.2))
                        .text_color(if dimmed {
                            s.muted_foreground
                        } else {
                            s.foreground
                        })
                        .children(code.chars().map(|c| div().child(c.to_string()))),
                )
        };
        let wide = |button: Button| button.w_full();
        let muted = |text: SharedString| {
            div()
                .debug_selector(|| "sign in message".into())
                .text_size(font_size::SM)
                .text_color(s.muted_foreground)
                .child(text)
        };
        let try_again = div()
            .debug_selector(|| "sign in try again".into())
            .child(wide(
                Button::new("sign-in-try-again")
                    .label("Try again")
                    .on_click(cx.listener(|this, _, window, cx| this.start(window, cx))),
            ));
        let body = div().flex().flex_col().gap(space::S4);
        match &self.stage {
            Stage::Idle | Stage::Starting => body
                .child(code_box(SharedString::default(), true))
                .child(wide(
                    Button::new("sign-in-copy")
                        .label("Copy code & open GitHub")
                        .disabled(true),
                )),
            Stage::Waiting {
                code, expires_at, ..
            } => body
                .child(code_box(code.clone(), false))
                .child(
                    div().debug_selector(|| "sign in copy".into()).child(wide(
                        Button::new("sign-in-copy")
                            .label("Copy code & open GitHub")
                            .on_click(cx.listener(|this, _, _, cx| this.copy_and_open(cx))),
                    )),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space::S2)
                        .text_size(font_size::SM)
                        .child(
                            Icon::new(IconName::LoaderCircle)
                                .size(space::S3_5)
                                .text_color(s.muted_foreground),
                        )
                        .child(
                            div()
                                .text_color(s.foreground)
                                .child("Waiting for approval…"),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .debug_selector(|| "sign in expires".into())
                                .text_color(s.muted_foreground)
                                .child(expires_in(*expires_at, Utc::now())),
                        ),
                ),
            Stage::Ended { code, message } => body
                .child(code_box(code.clone(), true))
                .child(try_again)
                .child(muted(message.clone())),
            Stage::Refused(message) => body.child(muted(message.clone())).child(try_again),
        }
        .into_any_element()
    }
}

/// The code shown for `sign_in`, until it expires.
fn waiting(sign_in: DeviceSignIn) -> Stage {
    let expires_at = DateTime::parse_from_rfc3339(&sign_in.expires_at)
        .map(|at| at.to_utc())
        .unwrap_or_else(|_| Utc::now());
    Stage::Waiting {
        code: sign_in.user_code.into(),
        uri: sign_in.verification_uri,
        expires_at,
    }
}

impl Render for SignInDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Escape and the corner button close it without asking: the sign-in stops too.
        if !self.handle.is_open() && self.stage != Stage::Idle {
            self.stop(cx);
        }
        let body = self.body(cx);
        Dialog::new("sign-in").handle(self.handle.clone()).content(
            DialogContent::new()
                .header(
                    DialogHeader::new()
                        .title(DialogTitle::new("Sign in to GitHub"))
                        .description(DialogDescription::new(DESCRIPTION)),
                )
                .child(body)
                .footer(
                    DialogFooter::new().child(
                        div().debug_selector(|| "sign in cancel".into()).child(
                            Button::new("sign-in-cancel")
                                .variant(ButtonVariant::Outline)
                                .label("Cancel")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.cancel(window, cx)),
                                ),
                        ),
                    ),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod ended_message {
        use super::*;

        #[test]
        fn should_say_why_the_sign_in_ended() {
            assert_eq!(
                ended_message(DeviceOutcome::Expired, None),
                "This code expired."
            );
            assert_eq!(
                ended_message(DeviceOutcome::Denied, None),
                "You denied access on GitHub."
            );
            assert_eq!(
                ended_message(DeviceOutcome::Failed, Some("The client_id is not valid.")),
                "The client_id is not valid."
            );
        }
    }

    mod expires_in {
        use super::*;

        fn at(secs: i64) -> DateTime<Utc> {
            DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap()
        }

        #[test]
        fn should_count_minutes_and_seconds_left() {
            assert_eq!(expires_in(at(872), at(0)), "Expires in 14:32");
            assert_eq!(expires_in(at(65), at(0)), "Expires in 1:05");
        }

        #[test]
        fn should_stop_at_zero() {
            assert_eq!(expires_in(at(0), at(10)), "Expires in 0:00");
        }
    }
}
