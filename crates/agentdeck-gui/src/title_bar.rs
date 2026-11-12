//! The window's title bar, the Penpot part `agentdeck / TitleBar`, drawn when the compositor
//! leaves decorations to the client (GNOME's Mutter on Wayland does). Drag it to move the
//! window, double-click it to maximize, right-click it for the window menu; minimize,
//! maximize and close sit at the right end.
//!
//! The buttons and the double-click dispatch [`Minimize`], [`ToggleMaximize`] and [`Close`]
//! rather than calling the window themselves: the shell handles them, and a test can watch
//! them on GPUI's fake platform, which doesn't implement those window calls.

use brrritscold_core_gpui::{ActiveTheme as _, Button, ButtonSize, ButtonVariant, IconName};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Action, App, Entity, FocusHandle, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Pixels, RenderOnce, SharedString, Styled as _, Window, actions, div, px,
};

actions!(
    window,
    [
        /// Minimizes the window.
        Minimize,
        /// Maximizes the window, or restores it when it is maximized.
        ToggleMaximize,
        /// Closes the window, which quits the app.
        Close,
    ]
);

/// The bar's height. Raw in Penpot too: no sizing token is 32px.
const HEIGHT: Pixels = px(32.);

/// The title bar, full width above the window's content.
#[derive(IntoElement)]
pub struct TitleBar {
    title: SharedString,
}

impl TitleBar {
    /// A title bar showing `title`, centred.
    pub fn new(title: impl Into<SharedString>) -> Self {
        TitleBar {
            title: title.into(),
        }
    }
}

impl RenderOnce for TitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let s = cx.theme().semantic();
        let supported = window.window_controls();
        // A press on the bar arms a move, and the first motion with the button still down
        // hands the pointer to the compositor. Starting the move on the press itself would
        // swallow the second click of a double-click.
        let armed = window.use_keyed_state("title-bar-armed", cx, |_, _| false);
        // A press on a control focuses it, taking focus from whatever had it (the shell, a
        // terminal). The press records that focus and the click gives it back.
        let held = window.use_keyed_state("title-bar-held-focus", cx, |_, _| None);

        // Penpot `Controls`: ghost icon-xs buttons, `space.1` apart. A press on them never
        // arms a move.
        let buttons = div()
            .flex()
            .items_center()
            .gap(space::S1)
            .capture_any_mouse_down({
                let held = held.clone();
                move |_, window, cx| {
                    let focused = window.focused(cx);
                    held.update(cx, |held, _| *held = focused);
                }
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .when(supported.minimize, |row| {
                row.child(control("minimize", IconName::Minus, &Minimize, &held))
            })
            .when(supported.maximize, |row| {
                row.child(control(
                    "maximize",
                    IconName::Maximize2,
                    &ToggleMaximize,
                    &held,
                ))
            })
            .child(control("close", IconName::X, &Close, &held));

        div()
            .id("title-bar")
            .debug_selector(|| "title-bar".into())
            .flex()
            .flex_none()
            .items_center()
            .w_full()
            .h(HEIGHT)
            .pl(space::S3)
            .pr(space::S1)
            .bg(s.sidebar)
            .on_mouse_down(MouseButton::Left, {
                let armed = armed.clone();
                move |event, window, cx| {
                    if event.click_count == 2 {
                        armed.update(cx, |armed, _| *armed = false);
                        window.dispatch_action(ToggleMaximize.boxed_clone(), cx);
                    } else {
                        armed.update(cx, |armed, _| *armed = true);
                    }
                }
            })
            .on_mouse_up(MouseButton::Left, {
                let armed = armed.clone();
                move |_, _, cx| armed.update(cx, |armed, _| *armed = false)
            })
            .on_mouse_move(move |event, window, cx| {
                if !*armed.read(cx) {
                    return;
                }
                armed.update(cx, |armed, _| *armed = false);
                if event.pressed_button == Some(MouseButton::Left) {
                    window.start_window_move();
                }
            })
            .on_mouse_down(MouseButton::Right, |event, window, _| {
                window.show_window_menu(event.position);
            })
            // Penpot `Drag` | `Title` | `Controls`: the two ends share the free width, so the
            // title stays centred on the window.
            .child(div().flex_1())
            .child(
                div()
                    .text_size(font_size::XS)
                    .font_weight(font_weight::MEDIUM)
                    .text_color(s.muted_foreground)
                    .child(self.title),
            )
            .child(div().flex_1().flex().justify_end().child(buttons))
    }
}

/// One window control: a ghost icon-xs button that dispatches `action`, then gives focus
/// back to what `held` recorded when it was pressed.
fn control(
    name: &'static str,
    icon: IconName,
    action: &dyn Action,
    held: &Entity<Option<FocusHandle>>,
) -> impl IntoElement {
    let action = action.boxed_clone();
    let held = held.clone();
    div()
        .debug_selector(move || format!("title-bar-{name}"))
        .child(
            Button::new(SharedString::new_static(name))
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::IconXs)
                .icon(icon)
                .on_click(move |_, window, cx| {
                    window.dispatch_action(action.boxed_clone(), cx);
                    if let Some(focus) = held.update(cx, |held, _| held.take()) {
                        window.focus(&focus, cx);
                    }
                }),
        )
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use brrritscold_core_gpui::{Theme, ThemePreference};
    use gpui::{
        Context, FocusHandle, Modifiers, MouseDownEvent, MouseUpEvent, Point, Render,
        TestAppContext, VisualTestContext,
    };

    use super::*;

    /// A window holding only a [`TitleBar`], recording the actions it dispatches. It holds
    /// focus, as the shell does, so the actions reach its listeners.
    struct Harness {
        focus: FocusHandle,
        seen: Rc<RefCell<Vec<&'static str>>>,
    }

    impl Render for Harness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .track_focus(&self.focus)
                .on_action(record::<Minimize>(&self.seen, "minimize"))
                .on_action(record::<ToggleMaximize>(&self.seen, "maximize"))
                .on_action(record::<Close>(&self.seen, "close"))
                .child(TitleBar::new("agentdeck"))
        }
    }

    /// A listener that records `name` when an `A` arrives.
    fn record<A: Action>(
        seen: &Rc<RefCell<Vec<&'static str>>>,
        name: &'static str,
    ) -> impl Fn(&A, &mut Window, &mut App) + 'static {
        let seen = seen.clone();
        move |_, _, _| seen.borrow_mut().push(name)
    }

    /// Opens a [`Harness`] window and draws it; returns the actions it will record.
    fn open(cx: &mut TestAppContext) -> (Rc<RefCell<Vec<&'static str>>>, &mut VisualTestContext) {
        cx.update(brrritscold_core_gpui::init);
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (_, cx) = cx.add_window_view({
            let seen = seen.clone();
            move |window, cx| {
                let focus = cx.focus_handle();
                window.focus(&focus, cx);
                Harness { focus, seen }
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        (seen, cx)
    }

    fn centre_of(selector: &'static str, cx: &mut VisualTestContext) -> Point<Pixels> {
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("`{selector}` is drawn"))
            .center()
    }

    mod render {
        use super::*;

        #[gpui::test]
        fn should_draw_in_both_themes(cx: &mut TestAppContext) {
            let (_, cx) = open(cx);
            for preference in [ThemePreference::Light, ThemePreference::Dark] {
                cx.update(|_, cx| Theme::set_preference(preference, cx));
                cx.run_until_parked();
                cx.update(|window, cx| window.draw(cx).clear(cx));
            }
        }

        #[gpui::test]
        fn should_be_32_pixels_tall_across_the_window(cx: &mut TestAppContext) {
            let (_, cx) = open(cx);
            let bar = cx.debug_bounds("title-bar").expect("the bar is drawn");
            let window = cx.update(|window, _| window.viewport_size());

            assert_eq!(bar.size.height, px(32.));
            assert_eq!(bar.size.width, window.width);
        }

        #[gpui::test]
        fn should_order_the_controls_minimize_maximize_close(cx: &mut TestAppContext) {
            let (_, cx) = open(cx);
            let [minimize, maximize, close] = [
                "title-bar-minimize",
                "title-bar-maximize",
                "title-bar-close",
            ]
            .map(|selector| centre_of(selector, cx).x);

            assert!(minimize < maximize && maximize < close);
        }
    }

    mod controls {
        use super::*;

        #[gpui::test]
        fn should_dispatch_minimize(cx: &mut TestAppContext) {
            let (seen, cx) = open(cx);
            let at = centre_of("title-bar-minimize", cx);

            cx.simulate_click(at, Modifiers::default());

            assert_eq!(*seen.borrow(), ["minimize"]);
        }

        #[gpui::test]
        fn should_dispatch_toggle_maximize(cx: &mut TestAppContext) {
            let (seen, cx) = open(cx);
            let at = centre_of("title-bar-maximize", cx);

            cx.simulate_click(at, Modifiers::default());

            assert_eq!(*seen.borrow(), ["maximize"]);
        }

        #[gpui::test]
        fn should_dispatch_close(cx: &mut TestAppContext) {
            let (seen, cx) = open(cx);
            let at = centre_of("title-bar-close", cx);

            cx.simulate_click(at, Modifiers::default());

            assert_eq!(*seen.borrow(), ["close"]);
        }

        #[gpui::test]
        fn should_leave_focus_where_it_was(cx: &mut TestAppContext) {
            let (_, cx) = open(cx);
            let at = centre_of("title-bar-maximize", cx);

            cx.simulate_click(at, Modifiers::default());
            cx.update(|window, cx| window.draw(cx).clear(cx));

            let harness_focused = cx.update(|window, cx| {
                let focused = window.focused(cx).expect("something has focus");
                window
                    .root::<Harness>()
                    .flatten()
                    .expect("the harness is the root")
                    .read(cx)
                    .focus
                    == focused
            });
            assert!(harness_focused);
        }
    }

    mod bar {
        use super::*;

        /// A press and release of the left button at `at`, the `click_count`-th in a row.
        fn click(at: Point<Pixels>, click_count: usize, cx: &mut VisualTestContext) {
            cx.simulate_event(MouseDownEvent {
                position: at,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
                click_count,
                first_mouse: false,
            });
            cx.simulate_event(MouseUpEvent {
                position: at,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
                click_count,
            });
        }

        #[gpui::test]
        fn should_toggle_maximize_on_a_double_click(cx: &mut TestAppContext) {
            let (seen, cx) = open(cx);
            let at = centre_of("title-bar", cx);

            click(at, 1, cx);
            click(at, 2, cx);

            assert_eq!(*seen.borrow(), ["maximize"]);
        }

        #[gpui::test]
        fn should_dispatch_nothing_on_a_single_click(cx: &mut TestAppContext) {
            let (seen, cx) = open(cx);
            let at = centre_of("title-bar", cx);

            click(at, 1, cx);

            assert!(seen.borrow().is_empty());
        }
    }
}
