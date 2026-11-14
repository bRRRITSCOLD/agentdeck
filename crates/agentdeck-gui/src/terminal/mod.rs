//! The terminal (spec §11): an `alacritty_terminal` model per attached pane, drawn by our own
//! renderer in JetBrains Mono with symbol fallbacks, in a palette that follows the theme.

mod element;
mod input;
mod link;
mod palette;
mod screen;
mod view;

pub use element::SYMBOL_FAMILIES;
pub use view::{TerminalEvent, TerminalView};

/// Registers the terminal's fonts; call once, before the first window.
pub fn init(cx: &mut gpui::App) {
    element::init(cx);
}
