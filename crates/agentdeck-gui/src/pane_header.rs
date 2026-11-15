//! The Penpot part `agentdeck / PaneHeader`: a pane's state, title and linked task above its
//! terminal (spec §11). Penpot's page "Screens" board "Pane states" shows each state.

use agentdeck_core::AgentState;
use brrritscold_core_gpui::{
    ActiveTheme as _, Badge, BadgeVariant, DropdownMenu, Icon, IconName, StateDot, StateDotState,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div,
};

use crate::state_dot;
use crate::terminal::SYMBOL_FAMILIES;

/// A stretch of a title, and whether it is symbols to set in the symbol font.
type Run<'a> = (&'a str, bool);

/// Whether `c` is a symbol agents put in titles (`✳`, `✻`, `⚠`) that Inter lacks: the
/// Miscellaneous Symbols and Dingbats blocks. GPUI's Linux text system ignores a font's
/// fallbacks and finds these in a colour emoji font, so the header sets them in a symbol
/// font itself.
fn is_symbol(c: char) -> bool {
    matches!(c, '\u{2600}'..='\u{27BF}')
}

/// `title` split into runs of symbols and of everything else, in order.
fn runs(title: &str) -> Vec<Run<'_>> {
    let mut runs = Vec::new();
    let (mut start, mut current) = (0, None);
    for (at, c) in title.char_indices() {
        let symbol = is_symbol(c);
        if current.is_some_and(|run| run != symbol) {
            runs.push((&title[start..at], !symbol));
            start = at;
        }
        current = Some(symbol);
    }
    if let Some(symbol) = current {
        runs.push((&title[start..], symbol));
    }
    runs
}

/// `text` as a run of elements, each one line: its symbols in the symbol font, the rest
/// truncated with an ellipsis. Lay them out in a flex row.
pub fn symbol_runs(text: &str) -> Vec<gpui::Div> {
    (runs(text).into_iter())
        .map(|(run, symbol)| {
            let run = div()
                .whitespace_nowrap()
                .child(SharedString::from(run.to_owned()));
            if symbol {
                run.flex_none().font_family(SYMBOL_FAMILIES[0])
            } else {
                run.min_w_0().overflow_hidden().text_ellipsis()
            }
        })
        .collect()
}

/// The `StateBadge` part's variant and label for a pane in `state`; `exit_code` tells a
/// failed exit from a clean one and is shown when known.
pub fn state_badge(state: AgentState, exit_code: Option<i64>) -> (BadgeVariant, SharedString) {
    let variant = match state_dot::of(state, exit_code) {
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
    menu: Option<DropdownMenu>,
}

impl PaneHeader {
    /// An idle pane titled `title`, linked to no task.
    pub fn new(title: impl Into<SharedString>) -> Self {
        PaneHeader {
            title: title.into(),
            state: AgentState::Idle,
            exit_code: None,
            task: None,
            menu: None,
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

    /// The pane's actions: the `Menu` part, at the right end after the `Spacer`.
    pub fn menu(mut self, menu: DropdownMenu) -> Self {
        self.menu = Some(menu);
        self
    }
}

impl RenderOnce for PaneHeader {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let s = cx.theme().semantic();
        let (variant, label) = state_badge(self.state, self.exit_code);
        // A turning mark redraws the window, so it holds still while the window is inactive.
        let dot = StateDot::new(state_dot::of(self.state, self.exit_code))
            .spin(window.is_window_active());
        let title = symbol_runs(&self.title);
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
            .child(dot)
            // Penpot `Title`: sm, medium, `foreground`.
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .overflow_hidden()
                    .text_size(font_size::SM)
                    .font_weight(font_weight::MEDIUM)
                    .text_color(s.foreground)
                    .children(title),
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
            // Penpot `Spacer` then `Menu`.
            .when_some(self.menu, |header, menu| {
                header
                    .child(div().flex_1())
                    .child(div().flex_none().child(menu))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod runs {
        use super::*;

        #[test]
        fn should_set_a_leading_symbol_apart() {
            assert_eq!(
                runs("✳ Claude Code"),
                [("✳", true), (" Claude Code", false)]
            );
        }

        #[test]
        fn should_keep_a_title_without_symbols_whole() {
            assert_eq!(runs("zsh — ts-packages"), [("zsh — ts-packages", false)]);
        }

        #[test]
        fn should_split_around_each_run_of_symbols() {
            assert_eq!(
                runs("a ✻✳ b ⚠"),
                [("a ", false), ("✻✳", true), (" b ", false), ("⚠", true)]
            );
        }

        #[test]
        fn should_make_no_runs_of_an_empty_title() {
            assert!(runs("").is_empty());
        }
    }

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

    mod spin {
        use gpui::{AnyElement, TestAppContext};

        use super::*;
        use crate::state_dot::tests::{deactivate, host, redraws_in_one_second};

        fn working(_: &Window) -> AnyElement {
            PaneHeader::new("claude")
                .state(AgentState::Working, None)
                .into_any_element()
        }

        #[gpui::test]
        fn should_spin_a_working_agents_mark_while_the_window_is_active(cx: &mut TestAppContext) {
            let (header, cx) = host(working, cx);
            assert!(redraws_in_one_second(&header, cx) > 0);
        }

        #[gpui::test]
        fn should_hold_it_still_while_the_window_is_inactive(cx: &mut TestAppContext) {
            let (header, cx) = host(working, cx);
            deactivate(cx);
            assert_eq!(redraws_in_one_second(&header, cx), 0);
        }
    }
}
