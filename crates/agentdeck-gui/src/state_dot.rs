//! Which core-gpui [`StateDot`] a pane's or a workspace's agent state gets (spec §11), drawn
//! still ([`mark`]). Blocked and a failed exit share red; the shape (a filled dot vs an
//! `OctagonX`) tells them apart, so colour is never the only signal.

use agentdeck_core::AgentState;
use brrritscold_core_gpui::{StateDot, StateDotState};

/// The mark for `state`; `exit_code` tells a failed exit from a clean one.
pub fn of(state: AgentState, exit_code: Option<i64>) -> StateDotState {
    match state {
        AgentState::Working => StateDotState::Working,
        AgentState::Blocked => StateDotState::Blocked,
        AgentState::Done => StateDotState::Done,
        AgentState::Idle => StateDotState::Idle,
        AgentState::Unknown => StateDotState::Unknown,
        AgentState::Exited => match exit_code {
            None | Some(0) => StateDotState::Exited,
            Some(_) => StateDotState::ExitedError,
        },
    }
}

/// The `StateDot` for `state`, drawn still: `working`'s `LoaderCircle` at rest, as Penpot
/// draws it. A turning mark redraws the whole window 15 times a second, which cost 1.9–2.4%
/// of a core in a focused window, above the 1.5% the app allows itself (spec §11).
pub fn mark(state: StateDotState) -> StateDot {
    StateDot::new(state).spin(false)
}

#[cfg(test)]
pub mod tests {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    use gpui::{
        AnyElement, Context, Entity, IntoElement, Render, TestAppContext, VisualTestContext, Window,
    };

    use super::*;

    /// A view drawing what its fn builds for the window it's in.
    pub struct Host(fn(&Window) -> AnyElement);

    impl Render for Host {
        fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            (self.0)(window)
        }
    }

    /// Opens an active window on a [`Host`] drawing `element`.
    pub fn host(
        element: fn(&Window) -> AnyElement,
        cx: &mut TestAppContext,
    ) -> (Entity<Host>, &mut VisualTestContext) {
        cx.update(brrritscold_core_gpui::init);
        let (host, cx) = cx.add_window_view(|_, _| Host(element));
        activate(cx);
        (host, cx)
    }

    /// Focuses the window, as the user would. A test window opens inactive.
    pub fn activate(cx: &mut VisualTestContext) {
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
    }

    /// Plays one second of a 60 Hz display and returns how often `view` asked to redraw in
    /// it: 0 while nothing in it moves.
    pub fn redraws_in_one_second<V: 'static>(
        view: &Entity<V>,
        cx: &mut VisualTestContext,
    ) -> usize {
        let redraws = Rc::new(Cell::new(0));
        let _observing = cx.update(|_, cx| {
            let redraws = redraws.clone();
            cx.observe(view, move |_, _| redraws.set(redraws.get() + 1))
        });
        for _ in 0..60 {
            play_frame(cx);
        }
        redraws.get()
    }

    /// Redraws `view`, and nothing else, `times` times, a frame each.
    pub fn redraw<V: 'static>(view: &Entity<V>, times: usize, cx: &mut VisualTestContext) {
        for _ in 0..times {
            view.update(cx, |_, cx| cx.notify());
            play_frame(cx);
        }
    }

    /// Moves the clock on by one 60 Hz frame, delivers the frame and lets whatever it woke
    /// run.
    pub fn play_frame(cx: &mut VisualTestContext) {
        cx.executor()
            .advance_clock(Duration::from_secs_f64(1. / 60.));
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
    }

    mod of {
        use super::*;

        #[test]
        fn should_give_each_live_state_its_namesake() {
            for (state, mark) in [
                (AgentState::Working, StateDotState::Working),
                (AgentState::Blocked, StateDotState::Blocked),
                (AgentState::Done, StateDotState::Done),
                (AgentState::Idle, StateDotState::Idle),
                (AgentState::Unknown, StateDotState::Unknown),
            ] {
                assert_eq!(of(state, None), mark, "{state:?}");
            }
        }

        #[test]
        fn should_mark_a_clean_exit_apart_from_a_failed_one() {
            assert_eq!(of(AgentState::Exited, Some(0)), StateDotState::Exited);
            assert_eq!(of(AgentState::Exited, Some(1)), StateDotState::ExitedError);
        }

        #[test]
        fn should_treat_an_unread_exit_code_as_clean() {
            assert_eq!(of(AgentState::Exited, None), StateDotState::Exited);
        }
    }

    mod spin {
        use super::*;

        #[gpui::test]
        fn should_hold_a_working_agents_mark_still(cx: &mut TestAppContext) {
            let (host, cx) = host(
                |_| mark(of(AgentState::Working, None)).into_any_element(),
                cx,
            );
            assert_eq!(redraws_in_one_second(&host, cx), 0);
        }
    }
}
