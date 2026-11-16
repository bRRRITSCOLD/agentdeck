//! `agentdeck-gui`: the GPUI app, built on `brrritscold-core-gpui` (spec §11).
//!
//! `--version` prints the version. Otherwise it opens the main window, which connects to the
//! daemon, starting `agentdeckd` when nothing answers (spec §2).

mod api;
mod close_prs;
mod connection;
mod count;
mod delete_workspace;
mod edit_workspace;
mod form_enter;
mod frame;
mod github;
mod inbox;
mod instruction_list;
mod keymap;
mod link_task;
mod logging;
mod new_agent;
mod new_task;
mod new_workspace;
mod notice;
mod notifications;
mod open_pr;
mod overview;
mod pane;
mod pane_header;
mod quit;
mod rereads;
mod right_panel;
mod runtime;
mod scrollbar;
mod settings;
mod shell;
mod sidebar;
mod state_dir;
mod state_dot;
mod task_list;
mod task_row;
mod task_sheet;
mod tasks;
mod terminal;
mod title_bar;
mod top_bar;
mod window_state;
mod workspace;

use std::rc::Rc;

use agentdeck_notify::DesktopNotifier;
use brrritscold_core_gpui::Root;
use gpui::{App, AppContext as _};

use crate::connection::{Connection, DaemonConnector};
use crate::shell::Shell;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("{}", agentdeck_core::version_line("agentdeck-gui"));
        return;
    }
    let state_dir = state_dir::state_dir(|name| std::env::var_os(name));
    // First, so GPUI's start and every later line reach the log (spec §11).
    logging::init(
        state_dir
            .as_ref()
            .map(|dir| dir.join(logging::FILE))
            .as_deref(),
        std::env::var("RUST_LOG").ok().as_deref(),
    );
    gpui_platform::application().run(|cx: &mut App| {
        brrritscold_core_gpui::init(cx);
        runtime::init(cx);
        terminal::init(cx);
        terminal::open::use_processes(cx);
        keymap::init(cx);
        // One connection to the desktop's notification server for every notification.
        let notifier = Rc::new(DesktopNotifier::session(cx));
        let connection = cx.new(|cx| Connection::new(Rc::new(DaemonConnector), cx));
        // 1440×900, the design's size, fitted to the display; or the bounds saved last time
        // while they still land on a connected display; never below the minimum (spec §11).
        let state_file = state_dir.map(|dir| dir.join("window.json"));
        let saved = state_file.as_deref().and_then(window_state::load);
        let displays: Vec<_> = cx
            .displays()
            .iter()
            .map(|display| display.visible_bounds())
            .collect();
        let primary = cx.primary_display().map(|display| display.visible_bounds());
        let window_bounds = window_state::initial_bounds(saved, &displays, primary);
        cx.open_window(window_state::options(window_bounds), |window, cx| {
            if let Some(path) = state_file {
                window_state::remember(path, window, cx);
            }
            let shell = cx.new(|cx| Shell::new(connection, notifier, window, cx));
            cx.new(|cx| Root::new(shell, window, cx))
        })
        .expect("the main window opens");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}
