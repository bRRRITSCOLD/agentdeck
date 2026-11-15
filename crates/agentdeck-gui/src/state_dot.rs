//! Which core-gpui [`StateDot`](brrritscold_core_gpui::StateDot) a pane's or a workspace's
//! agent state gets (spec §11). Blocked and a failed exit share red; the shape (a filled dot vs
//! an `OctagonX`) tells them apart, so colour is never the only signal.

use agentdeck_core::AgentState;
use brrritscold_core_gpui::StateDotState;

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

#[cfg(test)]
pub mod tests {
    use brrritscold_core_gpui::StateDot;
    use gpui::{AnyElement, Context, IntoElement, Render, TestAppContext, Window};

    use super::*;

    /// How many next-frame callbacks one drawn frame of `element` leaves behind: above 0
    /// while something in it animates.
    pub fn frames_requested(element: fn() -> AnyElement, cx: &mut TestAppContext) -> usize {
        struct Host(fn() -> AnyElement);
        impl Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                (self.0)()
            }
        }
        cx.update(brrritscold_core_gpui::init);
        let (_, cx) = cx.add_window_view(|_, _| Host(element));
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.simulate_next_frame(cx)
        })
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
        fn should_spin_a_working_agents_mark(cx: &mut TestAppContext) {
            let working = || StateDot::new(of(AgentState::Working, None)).into_any_element();
            assert!(frames_requested(working, cx) > 0);
        }

        #[gpui::test]
        fn should_hold_an_idle_agents_mark_still(cx: &mut TestAppContext) {
            let idle = || StateDot::new(of(AgentState::Idle, None)).into_any_element();
            assert_eq!(frames_requested(idle, cx), 0);
        }
    }
}
