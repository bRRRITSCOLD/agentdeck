//! `agentdeck-gui`: the GPUI app, built on `brrritscold-core-gpui`.
//!
//! A stub until the spec's later tickets fill it: `--version`, or an empty themed window.

use brrritscold_core_gpui::Root;
use gpui::{
    App, AppContext as _, Bounds, Context, IntoElement, Render, Window, WindowBounds,
    WindowOptions, div, px, size,
};

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("{}", agentdeck_core::version_line("agentdeck-gui"));
        return;
    }
    gpui_platform::application().run(|cx: &mut App| {
        brrritscold_core_gpui::init(cx);
        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|_| Empty);
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .expect("the main window opens");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}

/// The main window's placeholder content.
struct Empty;

impl Render for Empty {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
