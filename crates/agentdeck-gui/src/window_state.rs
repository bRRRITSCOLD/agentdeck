//! The main window's size, position and maximized state, kept across restarts in
//! `$XDG_STATE_HOME/agentdeck/window.json` (spec §11). GUI-only, apart from `config.toml`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, AppContext as _, Bounds, Context, Entity, Global, Pixels, Subscription, Task, Window,
    WindowBounds, point, px, size,
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

/// `$XDG_STATE_HOME/agentdeck/window.json`, else `~/.local/state/agentdeck/window.json`.
/// `None` without an absolute `XDG_STATE_HOME` or `HOME`: the window then isn't remembered.
/// A relative value is ignored, as the XDG spec requires.
pub fn state_file(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let absolute = |name: &str| {
        var(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let state =
        absolute("XDG_STATE_HOME").or_else(|| Some(absolute("HOME")?.join(".local/state")))?;
    Some(state.join("agentdeck").join("window.json"))
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
/// bounds), else [`DEFAULT_SIZE`] centred on `primary`, shrunk to fit it.
pub fn initial_bounds(
    saved: Option<SavedWindow>,
    displays: &[Bounds<Pixels>],
    primary: Option<Bounds<Pixels>>,
) -> WindowBounds {
    if let Some(saved) = saved {
        let bounds = saved.bounds();
        if displays
            .iter()
            .any(|display| display.contains(&bounds.center()))
        {
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
        Some(display) => Bounds::centered_at(display.center(), default.min(&display.size)),
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
                width: 1000.,
                height: 700.,
                maximized: false,
            };
            let bounds = windowed(initial_bounds(Some(saved), &[full_hd()], Some(full_hd())));
            assert_eq!(bounds, display(100., 50., 1000., 700.));
        }

        #[test]
        fn should_restore_a_maximized_window_with_its_restore_bounds() {
            let saved = SavedWindow {
                x: 100.,
                y: 50.,
                width: 1000.,
                height: 700.,
                maximized: true,
            };
            assert_eq!(
                initial_bounds(Some(saved), &[full_hd()], Some(full_hd())),
                WindowBounds::Maximized(display(100., 50., 1000., 700.))
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

    mod state_file {
        use super::*;

        fn vars(
            pairs: &'static [(&'static str, &'static str)],
        ) -> impl Fn(&str) -> Option<OsString> {
            move |name| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            }
        }

        #[test]
        fn should_live_under_xdg_state_home() {
            let path = state_file(vars(&[("XDG_STATE_HOME", "/state"), ("HOME", "/home/me")]));
            assert_eq!(path, Some(PathBuf::from("/state/agentdeck/window.json")));
        }

        #[test]
        fn should_default_to_the_local_state_dir() {
            let path = state_file(vars(&[("HOME", "/home/me")]));
            assert_eq!(
                path,
                Some(PathBuf::from("/home/me/.local/state/agentdeck/window.json"))
            );
        }

        #[test]
        fn should_ignore_a_relative_xdg_state_home() {
            let path = state_file(vars(&[("XDG_STATE_HOME", "state"), ("HOME", "/home/me")]));
            assert_eq!(
                path,
                Some(PathBuf::from("/home/me/.local/state/agentdeck/window.json"))
            );
        }

        #[test]
        fn should_be_none_without_a_home() {
            assert_eq!(state_file(vars(&[])), None);
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
