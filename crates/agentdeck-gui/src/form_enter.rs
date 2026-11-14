//! Enter in a dialog's single-line input belongs to the dialog's form.
//!
//! gpui-base's single-line input emits `PressEnter` and passes Enter on, and the core-gpui
//! `Dialog` it sits in takes that as its `Confirm` and closes, whatever the form's Enter
//! handler made of it: https://github.com/bRRRITSCOLD/rust-packages/issues/49. Until the
//! dialog lets an app decide, each input is wrapped in [`form_enter`].

use gpui::{Div, InteractiveElement as _, IntoElement, ParentElement as _, div};
use gpui_base::actions::Confirm;

/// `input`, with the dialog's `Confirm` stopped before it reaches the dialog, so only the
/// form's `PressEnter` handler decides whether the dialog closes.
pub fn form_enter(input: impl IntoElement) -> Div {
    div().on_action(|_: &Confirm, _, _| {}).child(input)
}
