//! The toasts the app shows, built as a [`Notice`] so a test can read what was shown and press
//! its action: core-gpui's `Toast` keeps its parts to itself.

use std::rc::Rc;

use agentdeck_client::ApiError;
use brrritscold_core_gpui::{
    Toast, ToastAction, ToastDescription, ToastExt as _, ToastTitle, ToastType,
};
use gpui::{App, ClipboardItem, SharedString, Window};

/// A toast: its kind, title, description and action.
#[derive(Clone)]
pub struct Notice {
    /// Its kind, which picks its icon.
    pub kind: Option<ToastType>,
    /// The `Title` part.
    pub title: SharedString,
    /// The `Description` part.
    pub description: Option<SharedString>,
    /// The `Action` part.
    pub action: Option<NoticeAction>,
}

/// What a toast's action does.
type Run = Rc<dyn Fn(&mut Window, &mut App)>;

/// A toast's action button.
#[derive(Clone)]
pub struct NoticeAction {
    /// The button's label.
    pub label: SharedString,
    run: Run,
}

impl NoticeAction {
    /// A button reading `label` that runs `run`.
    pub fn new(
        label: impl Into<SharedString>,
        run: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        NoticeAction {
            label: label.into(),
            run: Rc::new(run),
        }
    }

    /// Does what pressing it does.
    pub fn run(&self, window: &mut Window, cx: &mut App) {
        (self.run)(window, cx);
    }
}

impl Notice {
    /// A plain toast titled `title`.
    pub fn new(title: impl Into<SharedString>) -> Self {
        Notice {
            kind: None,
            title: title.into(),
            description: None,
            action: None,
        }
    }

    /// An error toast titled `title`, saying `description`.
    pub fn error(title: impl Into<SharedString>, description: impl Into<SharedString>) -> Self {
        Notice {
            kind: Some(ToastType::Error),
            description: Some(description.into()),
            ..Notice::new(title)
        }
    }

    /// Says `description` under the title.
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Adds `action` beside the text.
    pub fn action(mut self, action: NoticeAction) -> Self {
        self.action = Some(action);
        self
    }

    /// Shows it at the bottom-right of `window`.
    pub fn show(self, window: &mut Window, cx: &mut App) {
        #[cfg(test)]
        cx.default_global::<Shown>().0.push(self.clone());
        let Notice {
            kind,
            title,
            description,
            action,
        } = self;
        let mut toast = Toast::new().title(ToastTitle::new(title));
        if let Some(kind) = kind {
            toast = toast.toast_type(kind);
        }
        if let Some(description) = description {
            toast = toast.description(ToastDescription::new(description));
        }
        if let Some(action) = action {
            toast = toast.action(
                ToastAction::new(action.label.clone())
                    .on_click(move |_, window, cx| action.run(window, cx)),
            );
        }
        window.push_toast(toast, cx);
    }
}

/// Puts `text` on the clipboard, then toasts `title` ("Copied pane id") to say so.
pub fn copy(text: String, title: &'static str, window: &mut Window, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text));
    Notice::new(title).show(window, cx);
}

/// What a failed call says to a person: the daemon's own message, without its code.
pub fn message(err: &anyhow::Error) -> SharedString {
    match err.downcast_ref::<ApiError>() {
        Some(err) => err.message.clone().into(),
        None => format!("{err:#}").into(),
    }
}

/// Every notice shown, oldest first.
#[cfg(test)]
#[derive(Default)]
struct Shown(Vec<Notice>);

#[cfg(test)]
impl gpui::Global for Shown {}

/// Every notice shown in the app so far, oldest first.
#[cfg(test)]
pub fn shown(cx: &App) -> Vec<Notice> {
    cx.try_global::<Shown>()
        .map(|shown| shown.0.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    mod message {
        use super::*;

        #[test]
        fn should_be_the_daemons_message_without_its_code() {
            let err = anyhow::Error::from(ApiError {
                message: "Close its 2 running panes first".into(),
                code: Some("panes_running".into()),
            });

            assert_eq!(message(&err), "Close its 2 running panes first");
        }

        #[test]
        fn should_be_the_whole_chain_for_any_other_failure() {
            let err = anyhow::Error::msg("refused").context("talking to agentdeckd");

            assert_eq!(message(&err), "talking to agentdeckd: refused");
        }
    }
}
