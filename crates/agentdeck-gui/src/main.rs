//! `agentdeck-gui`: the GPUI app, built on `brrritscold-core-gpui` (spec §11).
//!
//! `--version` prints the version. Otherwise it opens the main window, which connects to the
//! daemon, starting `agentdeckd` when nothing answers (spec §2).

mod api;
mod connection;
mod count;
mod delete_workspace;
mod form_enter;
mod frame;
mod inbox;
mod instruction_list;
mod keymap;
mod new_agent;
mod new_task;
mod new_workspace;
mod notice;
mod overview;
mod pane;
mod pane_header;
mod rereads;
mod right_panel;
mod runtime;
mod shell;
mod sidebar;
mod state_dot;
mod task_list;
mod task_row;
mod task_sheet;
mod tasks;
mod terminal;
mod title_bar;
mod top_bar;
mod workspace;

use std::rc::Rc;

use brrritscold_core_gpui::Root;
use gpui::{
    App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowDecorations, WindowOptions,
    px, size,
};

use crate::connection::{Connection, DaemonConnector};
use crate::shell::Shell;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("{}", agentdeck_core::version_line("agentdeck-gui"));
        return;
    }
    gpui_platform::application().run(|cx: &mut App| {
        brrritscold_core_gpui::init(cx);
        runtime::init(cx);
        terminal::init(cx);
        keymap::init(cx);
        let connection = cx.new(|cx| Connection::new(Rc::new(DaemonConnector), cx));
        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("agentdeck".into()),
                    ..Default::default()
                }),
                // The compositor's frame where it draws one; where it doesn't (GNOME's
                // Mutter), GPUI falls back to client decorations and the shell draws the
                // title bar and resize edges.
                window_decorations: Some(WindowDecorations::Server),
                // Names the window to the desktop, so Alt+Tab and the dock can find it
                // again once it is minimized.
                app_id: Some("agentdeck".into()),
                ..Default::default()
            },
            |window, cx| {
                let shell = cx.new(|cx| Shell::new(connection, window, cx));
                cx.new(|cx| Root::new(shell, window, cx))
            },
        )
        .expect("the main window opens");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}
