//! The Penpot part `agentdeck / StateDot`: a pane's or a workspace's agent state as a 16px
//! mark (spec §11). Blocked and a failed exit share red; the shape (a filled dot vs an
//! `OctagonX`) tells them apart, so colour is never the only signal.

use agentdeck_core::AgentState;
use brrritscold_core_gpui::{ActiveTheme as _, Icon, IconName};
use brrritscold_core_gpui_tokens::primitive::space;
use gpui::{App, Hsla, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div};

/// The part's `state` variant property.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StateDotState {
    /// `working`: a `LoaderCircle` in `info`.
    Working,
    /// `blocked`: an 8px dot in `destructive`.
    Blocked,
    /// `done`: a `CircleCheck` in `success`.
    Done,
    /// `idle`: an 8px dot in `muted-foreground`.
    #[default]
    Idle,
    /// `exited`, with code 0 or none read: an `X` in `muted-foreground`.
    Exited,
    /// `unknown`: a `Circle` in `muted-foreground`.
    Unknown,
    /// `exited-error`, any other code: an `OctagonX` in `destructive`.
    ExitedError,
}

impl StateDotState {
    /// The mark for `state`; `exit_code` tells a failed exit from a clean one.
    pub fn of(state: AgentState, exit_code: Option<i64>) -> Self {
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
}

/// What a state paints: a filled dot, or an icon.
enum Mark {
    Dot(Hsla),
    Icon(IconName, Hsla),
}

/// A 16px state mark.
#[derive(IntoElement, Default)]
pub struct StateDot {
    state: StateDotState,
}

impl StateDot {
    /// An idle mark.
    pub fn new() -> Self {
        StateDot::default()
    }

    /// Penpot property `state`.
    pub fn state(mut self, state: StateDotState) -> Self {
        self.state = state;
        self
    }
}

impl RenderOnce for StateDot {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let s = cx.theme().semantic();
        let mark = match self.state {
            StateDotState::Working => Mark::Icon(IconName::LoaderCircle, s.info),
            StateDotState::Blocked => Mark::Dot(s.destructive),
            StateDotState::Done => Mark::Icon(IconName::CircleCheck, s.success),
            StateDotState::Idle => Mark::Dot(s.muted_foreground),
            StateDotState::Exited => Mark::Icon(IconName::X, s.muted_foreground),
            StateDotState::Unknown => Mark::Icon(IconName::Circle, s.muted_foreground),
            StateDotState::ExitedError => Mark::Icon(IconName::OctagonX, s.destructive),
        };
        let frame = div()
            .size(space::S4)
            .flex_none()
            .flex()
            .items_center()
            .justify_center();
        match mark {
            Mark::Dot(fill) => frame.child(div().size(space::S2).rounded_full().bg(fill)),
            Mark::Icon(icon, color) => {
                frame.child(Icon::new(icon).size(space::S4).text_color(color))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod state_dot_state {
        use super::*;

        #[test]
        fn should_mark_a_clean_exit_apart_from_a_failed_one() {
            assert_eq!(
                StateDotState::of(AgentState::Exited, Some(0)),
                StateDotState::Exited
            );
            assert_eq!(
                StateDotState::of(AgentState::Exited, Some(1)),
                StateDotState::ExitedError
            );
        }

        #[test]
        fn should_treat_an_unread_exit_code_as_clean() {
            assert_eq!(
                StateDotState::of(AgentState::Exited, None),
                StateDotState::Exited
            );
        }
    }
}
