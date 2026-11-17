//! Whether the user is at a window, for the marks that turn (spec §11): a working agent's
//! `StateDot`, running Checks. A turning mark redraws its whole window 15 times a second, so
//! it turns only while the window is focused and has had pointer or keyboard input in the
//! last [`IDLE_AFTER`]; otherwise it holds still.
//!
//! The window's root view calls [`watch`] once and draws a [`listener`]. Pointer moves,
//! clicks and scrolls reach the listener; keystrokes reach an interceptor; focusing the
//! window counts as input too. The first input after the window idled redraws it, so the
//! marks turn again at once. Going idle needs no timer of its own: a turning mark redraws
//! its view on every step, and the first step past [`IDLE_AFTER`] draws it still.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{
    App, Context, DispatchPhase, Global, IntoElement, MouseDownEvent, MouseMoveEvent, ScrollEvent,
    Styled as _, Subscription, Window, WindowId, canvas,
};

/// How long a window goes without pointer or keyboard input before its marks hold still.
pub const IDLE_AFTER: Duration = Duration::from_secs(30);

/// When each watched window last had input.
#[derive(Default)]
struct LastInput(HashMap<WindowId, Instant>);

impl Global for LastInput {}

/// Whether a turning mark in `window` turns: the window is focused and has had input in the
/// last [`IDLE_AFTER`]. A window nothing [`watch`]es never idles.
pub fn spins(window: &Window, cx: &App) -> bool {
    window.is_window_active() && !idle(window, cx)
}

/// Whether the watched `window` has gone [`IDLE_AFTER`] without input.
fn idle(window: &Window, cx: &App) -> bool {
    let id = window.window_handle().window_id();
    (cx.try_global::<LastInput>())
        .and_then(|last| last.0.get(&id))
        .is_some_and(|at| cx.background_executor().now() - *at >= IDLE_AFTER)
}

/// Notes input in `window` now. When it had idled, redraws it so its marks turn again.
fn record(window: &mut Window, cx: &mut App) {
    let was_idle = idle(window, cx);
    let now = cx.background_executor().now();
    (cx.default_global::<LastInput>().0).insert(window.window_handle().window_id(), now);
    if was_idle {
        window.refresh();
    }
}

/// Starts timing `window`'s input from now, counting keystrokes and focusing it. Keep both
/// subscriptions for as long as the window lives, and draw a [`listener`] in it for the
/// pointer.
pub fn watch<V: 'static>(window: &mut Window, cx: &mut Context<V>) -> [Subscription; 2] {
    record(window, cx);
    let id = window.window_handle().window_id();
    let keys = cx.intercept_keystrokes(move |_, window, cx| {
        if window.window_handle().window_id() == id {
            record(window, cx);
        }
    });
    let focus = cx.observe_window_activation(window, |_, window, cx| {
        if window.is_window_active() {
            record(window, cx);
        }
    });
    [keys, focus]
}

/// An empty element that notes each pointer move, click and scroll anywhere in its window as
/// input, before any element sees it, so one that stops the event still counts.
pub fn listener() -> impl IntoElement {
    canvas(
        |_, _, _| {},
        |_, _, window, _| {
            window.on_mouse_event(|_: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture {
                    record(window, cx);
                }
            });
            window.on_mouse_event(|_: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture {
                    record(window, cx);
                }
            });
            window.on_mouse_event(|_: &ScrollEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture {
                    record(window, cx);
                }
            });
        },
    )
    .absolute()
    .size_0()
}

#[cfg(test)]
mod tests {
    use brrritscold_core_gpui::{StateDot, StateDotState};
    use gpui::{
        Entity, Modifiers, ParentElement as _, Render, ScrollDelta, TestAppContext,
        VisualTestContext, div, point, px,
    };

    use super::*;
    use crate::state_dot::tests::{activate, deactivate, play_frame, redraws_in_one_second};

    /// A watched window's root, drawing one working agent's mark.
    struct Host {
        _watch: [Subscription; 2],
    }

    impl Render for Host {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(StateDot::new(StateDotState::Working).spin(spins(window, cx)))
                .child(listener())
        }
    }

    /// Opens a focused, watched window on a [`Host`].
    fn open(cx: &mut TestAppContext) -> (Entity<Host>, &mut VisualTestContext) {
        cx.update(brrritscold_core_gpui::init);
        let (host, cx) = cx.add_window_view(|window, cx| Host {
            _watch: watch(window, cx),
        });
        activate(cx);
        (host, cx)
    }

    /// Lets `wait` pass with no input, playing the frames at its end.
    fn wait(wait: Duration, cx: &mut VisualTestContext) {
        cx.executor().advance_clock(wait);
        for _ in 0..12 {
            play_frame(cx);
        }
    }

    fn move_pointer(cx: &mut VisualTestContext) {
        cx.simulate_mouse_move(point(px(10.), px(10.)), None, Modifiers::default());
        play_frame(cx);
    }

    #[gpui::test]
    fn should_spin_while_the_window_has_had_input_lately(cx: &mut TestAppContext) {
        let (host, cx) = open(cx);
        assert!(redraws_in_one_second(&host, cx) > 0);
    }

    #[gpui::test]
    fn should_hold_still_after_30_s_without_input(cx: &mut TestAppContext) {
        let (host, cx) = open(cx);
        wait(IDLE_AFTER, cx);
        assert_eq!(redraws_in_one_second(&host, cx), 0);
    }

    #[gpui::test]
    fn should_keep_spinning_while_input_comes_within_30_s(cx: &mut TestAppContext) {
        let (host, cx) = open(cx);
        wait(Duration::from_secs(20), cx);
        move_pointer(cx);
        wait(Duration::from_secs(20), cx);
        assert!(redraws_in_one_second(&host, cx) > 0);
    }

    #[gpui::test]
    fn should_hold_still_while_the_window_is_inactive(cx: &mut TestAppContext) {
        let (host, cx) = open(cx);
        deactivate(cx);
        move_pointer(cx);
        assert_eq!(redraws_in_one_second(&host, cx), 0);
    }

    mod after_idling {
        use super::*;

        #[gpui::test]
        fn should_spin_again_when_the_pointer_moves(cx: &mut TestAppContext) {
            let (host, cx) = open(cx);
            wait(IDLE_AFTER, cx);
            move_pointer(cx);
            assert!(redraws_in_one_second(&host, cx) > 0);
        }

        #[gpui::test]
        fn should_spin_again_on_a_click(cx: &mut TestAppContext) {
            let (host, cx) = open(cx);
            wait(IDLE_AFTER, cx);
            cx.simulate_mouse_down(
                point(px(10.), px(10.)),
                gpui::MouseButton::Left,
                Modifiers::default(),
            );
            play_frame(cx);
            assert!(redraws_in_one_second(&host, cx) > 0);
        }

        #[gpui::test]
        fn should_spin_again_on_a_scroll(cx: &mut TestAppContext) {
            let (host, cx) = open(cx);
            wait(IDLE_AFTER, cx);
            cx.simulate_event(ScrollEvent {
                position: point(px(10.), px(10.)),
                delta: ScrollDelta::Lines(point(0., 1.)),
                ..Default::default()
            });
            play_frame(cx);
            assert!(redraws_in_one_second(&host, cx) > 0);
        }

        #[gpui::test]
        fn should_spin_again_on_a_keystroke(cx: &mut TestAppContext) {
            let (host, cx) = open(cx);
            wait(IDLE_AFTER, cx);
            cx.simulate_keystrokes("a");
            play_frame(cx);
            assert!(redraws_in_one_second(&host, cx) > 0);
        }

        #[gpui::test]
        fn should_spin_again_when_the_window_is_focused(cx: &mut TestAppContext) {
            let (host, cx) = open(cx);
            deactivate(cx);
            wait(IDLE_AFTER, cx);
            activate(cx);
            assert!(redraws_in_one_second(&host, cx) > 0);
        }
    }
}
