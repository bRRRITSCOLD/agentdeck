//! The Penpot part `agentdeck / PaneHeader`: a pane's state, title and linked task above its
//! terminal (spec §11). Penpot's page "Screens" board "Pane states" shows each state.

use agentdeck_core::AgentState;
use brrritscold_core_gpui::{ActiveTheme as _, Badge, BadgeVariant, Icon, IconName};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div,
};

use crate::state_dot::{StateDot, StateDotState};

/// The `StateBadge` part's variant and label for a pane in `state`; `exit_code` tells a
/// failed exit from a clean one and is shown when known.
pub fn state_badge(state: AgentState, exit_code: Option<i64>) -> (BadgeVariant, SharedString) {
    let variant = match StateDotState::of(state, exit_code) {
        StateDotState::Working => BadgeVariant::Info,
        StateDotState::Blocked | StateDotState::ExitedError => BadgeVariant::Destructive,
        StateDotState::Done => BadgeVariant::Success,
        StateDotState::Idle | StateDotState::Exited | StateDotState::Unknown => {
            BadgeVariant::Outline
        }
    };
    let label = match (state, exit_code) {
        (AgentState::Exited, Some(code)) => format!("exited {code}").into(),
        _ => state.as_str().into(),
    };
    (variant, label)
}

/// A pane's header.
#[derive(IntoElement)]
pub struct PaneHeader {
    title: SharedString,
    state: AgentState,
    exit_code: Option<i64>,
    task: Option<SharedString>,
}

impl PaneHeader {
    /// An idle pane titled `title`, linked to no task.
    pub fn new(title: impl Into<SharedString>) -> Self {
        PaneHeader {
            title: title.into(),
            state: AgentState::Idle,
            exit_code: None,
            task: None,
        }
    }

    /// The pane's state, and its exit code once it has exited: the `State` and `StateBadge`
    /// parts.
    pub fn state(mut self, state: AgentState, exit_code: Option<i64>) -> Self {
        self.state = state;
        self.exit_code = exit_code;
        self
    }

    /// The linked task's title: the `Task` part, absent without one.
    pub fn task(mut self, task: Option<SharedString>) -> Self {
        self.task = task;
        self
    }
}

impl RenderOnce for PaneHeader {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let s = cx.theme().semantic();
        let (variant, label) = state_badge(self.state, self.exit_code);
        // Penpot: `card`, 36px high (raw in Penpot too), px `space.3`, `space.2` apart. The
        // pane's corners clip it there; GPUI doesn't clip to a radius, so it rounds its own top.
        div()
            .flex_none()
            .h(space::S9)
            .flex()
            .items_center()
            .gap(space::S2)
            .px(space::S3)
            .rounded_t(s.radius_md)
            .bg(s.card)
            .child(StateDot::new().state(StateDotState::of(self.state, self.exit_code)))
            // Penpot `Title`: sm, medium, `foreground`.
            .child(
                div()
                    .min_w_0()
                    .text_size(font_size::SM)
                    .font_weight(font_weight::MEDIUM)
                    .text_color(s.foreground)
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(self.title),
            )
            .child(
                div()
                    .flex_none()
                    .child(Badge::new().variant(variant).label(label)),
            )
            // Penpot `Task`: a 14px `ListTodo` and xs muted text, `space.1-5` apart.
            .when_some(self.task, |header, task| {
                header.child(
                    div()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(space::S1_5)
                        .text_size(font_size::XS)
                        .text_color(s.muted_foreground)
                        // The icon keeps the library icon's `foreground`, as the part does.
                        .child(
                            Icon::new(IconName::ListTodo)
                                .size(space::S3_5)
                                .flex_none()
                                .text_color(s.foreground),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .child(task),
                        ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod state_badge {
        use super::*;

        #[test]
        fn should_colour_each_live_state_as_the_spec_does() {
            assert_eq!(
                state_badge(AgentState::Working, None),
                (BadgeVariant::Info, "working".into())
            );
            assert_eq!(
                state_badge(AgentState::Blocked, None),
                (BadgeVariant::Destructive, "blocked".into())
            );
            assert_eq!(
                state_badge(AgentState::Done, None),
                (BadgeVariant::Success, "done".into())
            );
            assert_eq!(
                state_badge(AgentState::Idle, None),
                (BadgeVariant::Outline, "idle".into())
            );
        }

        #[test]
        fn should_show_a_failed_exit_code_in_red() {
            assert_eq!(
                state_badge(AgentState::Exited, Some(1)),
                (BadgeVariant::Destructive, "exited 1".into())
            );
        }

        #[test]
        fn should_keep_a_clean_exit_muted() {
            assert_eq!(
                state_badge(AgentState::Exited, Some(0)),
                (BadgeVariant::Outline, "exited 0".into())
            );
        }

        #[test]
        fn should_say_exited_alone_when_the_code_is_unknown() {
            assert_eq!(
                state_badge(AgentState::Exited, None),
                (BadgeVariant::Outline, "exited".into())
            );
        }
    }
}
