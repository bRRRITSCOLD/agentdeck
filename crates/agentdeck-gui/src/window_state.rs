//! The main window's size, position and maximized state, kept across restarts in
//! `$XDG_STATE_HOME/agentdeck/window.json` (spec §11). GUI-only, apart from `config.toml`.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, AppContext as _, Bounds, Context, Entity, Global, Pixels, Size, Subscription, Task,
    TitlebarOptions, Window, WindowBounds, WindowDecorations, WindowOptions, point, px, size,
};
use serde::{Deserialize, Serialize};

/// The size the window opens at without a saved one: the Penpot screens' size.
pub const DEFAULT_SIZE: (f32, f32) = (1440., 900.);

/// The window as last seen: its windowed (restore) bounds and whether it was maximized.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SavedWindow {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

/// The least size the window shrinks to (spec §11, Window): the narrowest layout that fits
/// without clipping. 1030 is the sidebar and its separator, the Pull requests view's padding
/// and its table in Select mode, its narrowest PR table, 1025, rounded up; 600 holds the
/// tallest dialog, Delete workspace (520), under the title bar.
pub const MIN_SIZE: (f32, f32) = (1030., 600.);

/// [`MIN_SIZE`] as a size.
pub fn min_size() -> Size<Pixels> {
    let (width, height) = MIN_SIZE;
    size(px(width), px(height))
}

/// The main window's options: opening at `window_bounds`, never resized below [`MIN_SIZE`].
pub fn options(window_bounds: WindowBounds) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(window_bounds),
        window_min_size: Some(min_size()),
        titlebar: Some(TitlebarOptions {
            title: Some("agentdeck".into()),
            ..Default::default()
        }),
        // The compositor's frame where it draws one; where it doesn't (GNOME's Mutter), GPUI
        // falls back to client decorations and the shell draws the title bar and resize edges.
        window_decorations: Some(WindowDecorations::Server),
        // Names the window to the desktop, so Alt+Tab and the dock can find it again once it
        // is minimized.
        app_id: Some("agentdeck".into()),
        ..Default::default()
    }
}

/// The saved window in `text`, or `None` when it isn't one.
pub fn parse(text: &str) -> Option<SavedWindow> {
    let saved: SavedWindow = serde_json::from_str(text).ok()?;
    let finite = [saved.x, saved.y, saved.width, saved.height]
        .iter()
        .all(|value| value.is_finite());
    (finite && saved.width > 0. && saved.height > 0.).then_some(saved)
}

/// The bounds to open at: `saved` while its centre is on one of `displays` (their visible
/// bounds), else [`DEFAULT_SIZE`] centred on `primary`, shrunk to fit it. Either grows to
/// [`MIN_SIZE`] where it is smaller: from its origin when saved, about its centre otherwise.
pub fn initial_bounds(
    saved: Option<SavedWindow>,
    displays: &[Bounds<Pixels>],
    primary: Option<Bounds<Pixels>>,
) -> WindowBounds {
    if let Some(saved) = saved {
        let mut bounds = saved.bounds();
        if displays
            .iter()
            .any(|display| display.contains(&bounds.center()))
        {
            bounds.size = bounds.size.max(&min_size());
            return if saved.maximized {
                WindowBounds::Maximized(bounds)
            } else {
                WindowBounds::Windowed(bounds)
            };
        }
    }
    let (width, height) = DEFAULT_SIZE;
    let default = size(px(width), px(height));
    WindowBounds::Windowed(match primary {
        Some(display) => Bounds::centered_at(
            display.center(),
            default.min(&display.size).max(&min_size()),
        ),
        None => Bounds {
            origin: Default::default(),
            size: default,
        },
    })
}

impl SavedWindow {
    /// The window's windowed bounds; a maximized or fullscreen window keeps its restore bounds.
    pub fn from_window_bounds(window_bounds: WindowBounds) -> SavedWindow {
        let (bounds, maximized) = match window_bounds {
            WindowBounds::Windowed(bounds) | WindowBounds::Fullscreen(bounds) => (bounds, false),
            WindowBounds::Maximized(bounds) => (bounds, true),
        };
        SavedWindow {
            x: bounds.origin.x.into(),
            y: bounds.origin.y.into(),
            width: bounds.size.width.into(),
            height: bounds.size.height.into(),
            maximized,
        }
    }

    fn bounds(&self) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(self.x), px(self.y)),
            size: size(px(self.width), px(self.height)),
        }
    }
}

/// Reads `path`; a missing or unreadable file is `None`.
pub fn load(path: &Path) -> Option<SavedWindow> {
    parse(&std::fs::read_to_string(path).ok()?)
}

/// Writes `saved` to `path` through a temp file, so a crash mid-write leaves the old file.
pub fn save(path: &Path, saved: &SavedWindow) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(saved).map_err(std::io::Error::other)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, path)
}

/// How long the window's bounds must stay put before they are written.
const SAVE_DELAY: Duration = Duration::from_millis(500);

/// Watches a window's bounds and hands them to `save` once a resize or move settles, and on
/// quit, so a quit inside [`SAVE_DELAY`] keeps the last bounds too.
pub struct WindowMemory {
    save: Rc<dyn Fn(&SavedWindow)>,
    /// The bounds not yet handed to `save`.
    unsaved: Option<SavedWindow>,
    /// The timer that saves `unsaved`; a newer change replaces it.
    pending: Task<()>,
    _subscriptions: [Subscription; 2],
}

impl WindowMemory {
    pub fn new(
        save: impl Fn(&SavedWindow) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> WindowMemory {
        WindowMemory {
            save: Rc::new(save),
            unsaved: None,
            pending: Task::ready(()),
            _subscriptions: [
                cx.observe_window_bounds(window, |this, window, cx| {
                    this.unsaved = Some(SavedWindow::from_window_bounds(window.window_bounds()));
                    this.pending = cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(SAVE_DELAY).await;
                        this.update(cx, |this, _| this.flush()).ok();
                    });
                }),
                cx.on_app_quit(|this, _| {
                    this.flush();
                    async {}
                }),
            ],
        }
    }

    fn flush(&mut self) {
        if let Some(saved) = self.unsaved.take() {
            (self.save)(&saved);
        }
    }
}

/// Keeps the main window's [`WindowMemory`] alive for the app's lifetime.
struct Remembered {
    _memory: Entity<WindowMemory>,
}

impl Global for Remembered {}

/// Saves `window`'s bounds to `path` from now on. A failed write is logged, not shown: the
/// next start just opens at the default size.
pub fn remember(path: PathBuf, window: &mut Window, cx: &mut App) {
    let memory = cx.new(|cx| {
        WindowMemory::new(
            move |saved| {
                if let Err(err) = save(&path, saved) {
                    tracing::warn!("saving the window to {}: {err}", path.display());
                }
            },
            window,
            cx,
        )
    });
    cx.set_global(Remembered { _memory: memory });
}

#[cfg(test)]
mod tests {
    use gpui::Point;

    use super::*;

    fn display(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(x), px(y)),
            size: size(px(width), px(height)),
        }
    }

    fn windowed(bounds: WindowBounds) -> Bounds<Pixels> {
        match bounds {
            WindowBounds::Windowed(bounds) => bounds,
            other => panic!("expected a windowed window, got {other:?}"),
        }
    }

    const FULL_HD: (f32, f32, f32, f32) = (0., 0., 1920., 1080.);

    fn full_hd() -> Bounds<Pixels> {
        let (x, y, width, height) = FULL_HD;
        display(x, y, width, height)
    }

    mod initial_bounds {
        use super::*;

        #[test]
        fn should_open_at_1440_by_900() {
            let bounds = windowed(initial_bounds(None, &[full_hd()], Some(full_hd())));
            assert_eq!(bounds.size, size(px(1440.), px(900.)));
            assert_eq!(bounds.center(), full_hd().center());
        }

        #[test]
        fn should_clamp_to_a_smaller_display() {
            let laptop = display(0., 32., 1366., 736.);
            let bounds = windowed(initial_bounds(None, &[laptop], Some(laptop)));
            assert_eq!(bounds.size, size(px(1366.), px(736.)));
            assert_eq!(bounds.origin, Point::new(px(0.), px(32.)));
        }

        #[test]
        fn should_open_at_1440_by_900_without_a_display() {
            let bounds = windowed(initial_bounds(None, &[], None));
            assert_eq!(bounds.size, size(px(1440.), px(900.)));
        }

        #[test]
        fn should_restore_the_saved_bounds() {
            let saved = SavedWindow {
                x: 100.,
                y: 50.,
                width: 1300.,
                height: 700.,
                maximized: false,
            };
            let bounds = windowed(initial_bounds(Some(saved), &[full_hd()], Some(full_hd())));
            assert_eq!(bounds, display(100., 50., 1300., 700.));
        }

        #[test]
        fn should_restore_a_maximized_window_with_its_restore_bounds() {
            let saved = SavedWindow {
                x: 100.,
                y: 50.,
                width: 1300.,
                height: 700.,
                maximized: true,
            };
            assert_eq!(
                initial_bounds(Some(saved), &[full_hd()], Some(full_hd())),
                WindowBounds::Maximized(display(100., 50., 1300., 700.))
            );
        }

        #[test]
        fn should_restore_onto_a_second_display() {
            let right = display(1920., 0., 2560., 1440.);
            let saved = SavedWindow {
                x: 2200.,
                y: 100.,
                width: 1440.,
                height: 900.,
                maximized: false,
            };
            let bounds = windowed(initial_bounds(
                Some(saved),
                &[full_hd(), right],
                Some(full_hd()),
            ));
            assert_eq!(bounds, display(2200., 100., 1440., 900.));
        }

        #[test]
        fn should_grow_saved_bounds_smaller_than_the_minimum() {
            let saved = SavedWindow {
                x: 100.,
                y: 50.,
                width: 800.,
                height: 400.,
                maximized: false,
            };
            let bounds = windowed(initial_bounds(Some(saved), &[full_hd()], Some(full_hd())));
            assert_eq!(bounds, display(100., 50., 1030., 600.));
        }

        #[test]
        fn should_grow_only_the_side_below_the_minimum() {
            let saved = SavedWindow {
                x: 100.,
                y: 50.,
                width: 1600.,
                height: 500.,
                maximized: true,
            };
            assert_eq!(
                initial_bounds(Some(saved), &[full_hd()], Some(full_hd())),
                WindowBounds::Maximized(display(100., 50., 1600., 600.))
            );
        }

        #[test]
        fn should_open_no_smaller_than_the_minimum_on_a_small_display() {
            let small = display(0., 0., 1024., 576.);
            let bounds = windowed(initial_bounds(None, &[small], Some(small)));
            assert_eq!(bounds.size, size(px(1030.), px(600.)));
        }

        #[test]
        fn should_ignore_bounds_off_every_display() {
            // Saved on a second display that has since been unplugged.
            let saved = SavedWindow {
                x: 2200.,
                y: 100.,
                width: 1440.,
                height: 900.,
                maximized: true,
            };
            let bounds = windowed(initial_bounds(Some(saved), &[full_hd()], Some(full_hd())));
            assert_eq!(bounds.size, size(px(1440.), px(900.)));
            assert_eq!(bounds.center(), full_hd().center());
        }
    }

    mod min_size {
        use brrritscold_core_gpui_tokens::primitive::space;

        use super::*;

        #[test]
        fn should_fit_the_narrowest_pr_table_in_select_mode_beside_the_sidebar() {
            // The sidebar and its separator, the Pull requests view's `space.6` padding either
            // side, and its table at its least in Select mode, which is wider than out of it.
            let narrowest = crate::pr_table::min_width(true);
            assert!(narrowest > crate::pr_table::min_width(false));
            let needed = crate::sidebar::WIDTH + px(1.) + space::S6 * 2. + narrowest;
            let min = min_size();
            assert!(min.width >= needed, "{min:?} < {needed:?}");
            // Rounded up, not padded.
            assert!(min.width - needed < px(10.), "{min:?} for {needed:?}");
        }

        #[test]
        fn should_fit_a_pane_header_beside_the_sidebar() {
            // Penpot's PaneHeader is 560 wide, and the panes sit `space.2` in.
            let centre = min_size().width - crate::sidebar::WIDTH - px(1.);
            assert!(centre >= px(560.) + space::S2 * 2., "{centre:?}");
        }

        #[test]
        fn should_be_the_window_options_minimum() {
            let options = options(initial_bounds(None, &[], None));
            assert_eq!(options.window_min_size, Some(min_size()));
        }
    }

    mod parse {
        use super::*;

        #[test]
        fn should_read_what_it_wrote() {
            let saved = SavedWindow {
                x: -10.,
                y: 20.,
                width: 1200.,
                height: 800.,
                maximized: true,
            };
            let text = serde_json::to_string(&saved).expect("serializes");
            assert_eq!(parse(&text), Some(saved));
        }

        #[test]
        fn should_ignore_a_broken_state_file() {
            assert_eq!(parse("{\"x\": 1, \"y\":"), None);
            assert_eq!(parse(""), None);
            assert_eq!(parse("[]"), None);
        }

        #[test]
        fn should_ignore_a_size_that_is_not_positive() {
            let text = r#"{"x":0,"y":0,"width":0,"height":800,"maximized":false}"#;
            assert_eq!(parse(text), None);
            let text = r#"{"x":0,"y":0,"width":1200,"height":-1,"maximized":false}"#;
            assert_eq!(parse(text), None);
        }
    }

    mod window_memory {
        use std::cell::RefCell;

        use gpui::{Entity, IntoElement, Render, TestAppContext, VisualTestContext, div};

        use super::*;

        /// A window holding a [`WindowMemory`] that records what it would save.
        struct Harness {
            _memory: Entity<WindowMemory>,
        }

        impl Render for Harness {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
            }
        }

        fn open(
            cx: &mut TestAppContext,
        ) -> (Rc<RefCell<Vec<SavedWindow>>>, &mut VisualTestContext) {
            let saved = Rc::new(RefCell::new(Vec::new()));
            let (_, cx) = cx.add_window_view({
                let saved = saved.clone();
                move |window, cx| Harness {
                    _memory: cx.new(|cx| {
                        WindowMemory::new(move |s| saved.borrow_mut().push(*s), window, cx)
                    }),
                }
            });
            (saved, cx)
        }

        fn sizes(saved: &RefCell<Vec<SavedWindow>>) -> Vec<(f32, f32)> {
            saved.borrow().iter().map(|s| (s.width, s.height)).collect()
        }

        #[gpui::test]
        fn should_save_once_a_resize_settles(cx: &mut TestAppContext) {
            let (saved, cx) = open(cx);
            cx.simulate_resize(size(px(1000.), px(700.)));
            cx.simulate_resize(size(px(1100.), px(750.)));
            cx.run_until_parked();
            assert_eq!(sizes(&saved), []);
            cx.executor().advance_clock(SAVE_DELAY);
            cx.run_until_parked();
            assert_eq!(sizes(&saved), [(1100., 750.)]);
        }

        #[gpui::test]
        fn should_save_the_last_bounds_on_quit(cx: &mut TestAppContext) {
            let (saved, cx) = open(cx);
            cx.simulate_resize(size(px(1000.), px(700.)));
            cx.run_until_parked();
            cx.quit();
            assert_eq!(sizes(&saved), [(1000., 700.)]);
        }

        #[gpui::test]
        fn should_not_save_on_quit_when_nothing_moved(cx: &mut TestAppContext) {
            let (saved, cx) = open(cx);
            cx.quit();
            assert_eq!(sizes(&saved), []);
        }
    }
}
