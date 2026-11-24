//! The PR sync parts of the Pull requests view (spec §11): the line saying what the age
//! cutoff does to older PRs, with its inline "Settings" link, and the banner shown while no
//! GitHub token works, with **Open Settings**. Each asks its view for Settings with
//! [`OpenSettings`].

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use brrritscold_core_gpui::{
    ActiveTheme as _, Alert, AlertAction, AlertTitle, Button, ButtonSize, ButtonVariant, IconName,
};
use brrritscold_core_gpui_tokens::primitive::space;
use gpui::{
    AnyElement, Context, EventEmitter, HighlightStyle, InteractiveElement as _, InteractiveText,
    IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledText, TextLayout, UnderlineStyle, div, px,
};

/// The cutoff line's "Settings" or the banner's Open Settings was clicked: show Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenSettings;

/// The end of a cutoff line that names Settings; its "Settings" is drawn as a link.
pub const SETTINGS_TAIL: &str = "Settings.";

/// What the age cutoff does, for a cutoff of `days` (`0` for none): older PRs are Finished at
/// the next full sync and leave 7 days later (spec §8), as Settings' age cutoff says.
pub fn cutoff(days: u32) -> String {
    let unit = match days {
        0 => return "Every open PR syncs.".to_owned(),
        1 => "day",
        _ => "days",
    };
    format!(
        "PRs not updated in the last {days} {unit} are finished and leave within 7 days; \
         change that in Settings."
    )
}

/// The cutoff line's "Settings" link. The line is one text that wraps as a paragraph, so the
/// link flows inline at any width (Penpot's `Description` is one text too). "Settings" is drawn
/// as core-gpui's link button draws (`primary`, underlined on hover) and asks for Settings when
/// clicked. Its view keeps it, since the hover lives across frames.
#[derive(Default)]
pub struct SettingsLink {
    /// Whether the pointer is over "Settings".
    hovered: Rc<Cell<bool>>,
    /// The line's text as last drawn, for tests to find "Settings" in.
    layout: RefCell<TextLayout>,
}

impl SettingsLink {
    /// `line`, wrapping in what its parent leaves it; when it ends in [`SETTINGS_TAIL`], its
    /// "Settings" is the link. `id` keys it.
    pub fn line<V: EventEmitter<OpenSettings>>(
        &self,
        id: &'static str,
        line: SharedString,
        cx: &mut Context<V>,
    ) -> AnyElement {
        let Some(link) = link_range(&line) else {
            return line.into_any_element();
        };
        let style = HighlightStyle {
            color: Some(cx.theme().semantic().primary),
            // As `Styled::underline`, which core-gpui's link button takes on hover.
            underline: self.hovered.get().then(|| UnderlineStyle {
                thickness: px(1.),
                ..Default::default()
            }),
            ..Default::default()
        };
        let text = StyledText::new(line).with_highlights([(link.clone(), style)]);
        *self.layout.borrow_mut() = text.layout().clone();
        let this = cx.entity().downgrade();
        let left = this.clone();
        let (hovered, leaving) = (self.hovered.clone(), self.hovered.clone());
        // The text reports the character under the pointer only while the pointer is over it,
        // so leaving the text takes the underline off here.
        div()
            .id(id)
            .on_hover(move |inside, _, cx| {
                if !inside && leaving.replace(false) {
                    let _ = left.update(cx, |_, cx| cx.notify());
                }
            })
            .child(
                InteractiveText::new(id, text)
                    .on_click(vec![link.clone()], move |_, _, cx| {
                        let _ = this.update(cx, |_, cx| cx.emit(OpenSettings));
                    })
                    // The text redraws its view when the character under the pointer changes.
                    .on_hover(move |ix, _, _, _| {
                        hovered.set(ix.is_some_and(|ix| link.contains(&ix)));
                    }),
            )
            .into_any_element()
    }

    /// The cutoff line's text as last drawn.
    #[cfg(test)]
    pub fn layout(&self) -> TextLayout {
        self.layout.borrow().clone()
    }

    /// Whether the pointer is over "Settings", underlining it.
    #[cfg(test)]
    pub fn hovered(&self) -> bool {
        self.hovered.get()
    }
}

/// Where "Settings" sits in `line`, when the line ends in [`SETTINGS_TAIL`].
fn link_range(line: &str) -> Option<Range<usize>> {
    let start = line.strip_suffix(SETTINGS_TAIL)?.len();
    Some(start..start + SETTINGS_TAIL.len() - ".".len())
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

    mod link_range {
        use super::*;

        #[test]
        fn should_cover_settings_without_its_full_stop() {
            let line = cutoff(90);
            let range = link_range(&line).expect("a link");
            assert_eq!(&line[range], "Settings");
        }

        #[test]
        fn should_be_none_for_a_line_not_naming_settings() {
            assert_eq!(link_range(&cutoff(0)), None);
        }
    }

    mod cutoff {
        use super::*;

        #[test]
        fn should_say_older_prs_finish_and_leave() {
            assert_eq!(
                cutoff(90),
                "PRs not updated in the last 90 days are finished and leave within 7 days; \
                 change that in Settings."
            );
            assert_eq!(
                cutoff(1),
                "PRs not updated in the last 1 day are finished and leave within 7 days; \
                 change that in Settings."
            );
        }

        #[test]
        fn should_say_every_open_pr_syncs_with_none() {
            assert_eq!(cutoff(0), "Every open PR syncs.");
        }
    }
}
