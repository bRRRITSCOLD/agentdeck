//! The PR sync parts of the Pull requests view (spec §11): the line saying
//! which PRs the age cutoff leaves out, its "Settings" link, and the banner shown while no
//! GitHub token works, with **Open Settings**. Each asks its view for Settings with
//! [`OpenSettings`].

use brrritscold_core_gpui::{
    ActiveTheme as _, Alert, AlertAction, AlertTitle, Button, ButtonSize, ButtonVariant, IconName,
};
use brrritscold_core_gpui_tokens::primitive::space;
use gpui::{
    Context, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div,
};

/// The cutoff line's "Settings" or the banner's Open Settings was clicked: show Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenSettings;

/// The end of a cutoff line that names Settings; its "Settings" is drawn as a link.
pub const SETTINGS_TAIL: &str = "Settings.";

/// Which open PRs sync, for a cutoff of `days` (`0` for none).
pub fn cutoff(days: u32) -> String {
    let unit = match days {
        0 => return "Every open PR syncs.".to_owned(),
        1 => "day",
        _ => "days",
    };
    format!("PRs not updated in the last {days} {unit} are left out; change that in Settings.")
}

/// The banner's title while no GitHub token works and no PR sync ever named a login.
pub const LOGGED_OUT: &str = "Not signed in to GitHub, so PRs don't sync.";

/// The banner's title while no GitHub token works: whose PRs it shows once a sync ran as
/// `synced_as` (Penpot "Pull requests · logged out · synced before"), else [`LOGGED_OUT`].
pub fn logged_out_title(synced_as: Option<&str>) -> SharedString {
    match synced_as {
        Some(login) => format!("Synced as {login}. Sign in to refresh.").into(),
        None => LOGGED_OUT.into(),
    }
}

/// "Settings", styled as core-gpui's link button (`primary`, underlined on hover), asking for
/// Settings when clicked. `key` names it: tests find it as `<key> settings link`.
pub fn settings_link<V: EventEmitter<OpenSettings>>(
    key: &'static str,
    cx: &mut Context<V>,
) -> impl IntoElement + use<V> {
    let s = cx.theme().semantic();
    div()
        .id(SharedString::from(format!("{key}-settings-link")))
        .debug_selector(move || format!("{key} settings link"))
        .text_color(s.primary)
        .cursor_pointer()
        .hover(|style| style.underline())
        .on_click(cx.listener(|_, _, _, cx| cx.emit(OpenSettings)))
        .child("Settings")
}

/// The banner while no GitHub token works: what's wrong, or whose PRs show, and **Open
/// Settings** to fix it (Penpot "Pull requests · logged out"). `key`
/// keys its button. Tests find it as `logged out banner`, its button as `open settings`.
pub fn logged_out_banner<V: EventEmitter<OpenSettings>>(
    key: &'static str,
    title: SharedString,
    cx: &mut Context<V>,
) -> impl IntoElement + use<V> {
    div()
        .flex_none()
        .debug_selector(|| "logged out banner".into())
        .child(
            Alert::new()
                .icon(IconName::Info)
                .title(AlertTitle::new(title))
                // Centred on the one-line alert, as Penpot's `Action` sits.
                .action(
                    AlertAction::new().top(space::S1_5).child(
                        div().debug_selector(|| "open settings".into()).child(
                            Button::new(SharedString::from(format!("{key}-open-settings")))
                                .variant(ButtonVariant::Link)
                                .size(ButtonSize::Sm)
                                .label("Open Settings")
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(OpenSettings))),
                        ),
                    ),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    mod logged_out_title {
        use super::*;

        #[test]
        fn should_name_the_login_prs_last_synced_as() {
            assert_eq!(
                logged_out_title(Some("bRRRITSCOLD")),
                "Synced as bRRRITSCOLD. Sign in to refresh."
            );
        }

        #[test]
        fn should_say_prs_do_not_sync_when_no_sync_named_a_login() {
            assert_eq!(logged_out_title(None), LOGGED_OUT);
        }
    }

    mod cutoff {
        use super::*;

        #[test]
        fn should_say_which_prs_it_leaves_out() {
            assert_eq!(
                cutoff(90),
                "PRs not updated in the last 90 days are left out; change that in Settings."
            );
            assert_eq!(
                cutoff(1),
                "PRs not updated in the last 1 day are left out; change that in Settings."
            );
        }

        #[test]
        fn should_say_every_open_pr_syncs_with_none() {
            assert_eq!(cutoff(0), "Every open PR syncs.");
        }
    }
}
