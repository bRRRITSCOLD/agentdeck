//! Quitting (spec §11). Agents keep running after the GUI quits, so while one runs, closing the
//! window and Quit ask first, unless `gui.confirm_quit` is off. The main window's shell asks;
//! this module counts the agents, words the question and routes the app-wide Quit to the shell.

use std::rc::Rc;

use gpui::{App, Global};

use crate::overview::Snapshot;

/// How many agents run: agent panes whose process hasn't exited, and shell panes hosting an
/// agent. Plain shells don't count.
pub fn running_agents(snapshot: &Snapshot) -> usize {
    snapshot
        .panes
        .iter()
        .filter(|pane| pane.agent.is_some())
        .count()
}

/// The dialog's description for `n` running agents.
pub fn still_running(n: usize) -> String {
    let count = if n == 1 {
        "1 agent is".to_owned()
    } else {
        format!("{n} agents are")
    };
    format!(
        "{count} still running. They keep running in the background, and reopening agentdeck \
         reconnects to them."
    )
}

/// What Quit asks first: the main window's shell, which knows the agents and the setting.
/// It answers whether it asked.
struct Asker(Rc<dyn Fn(&mut App) -> bool>);

impl Global for Asker {}

/// Has Quit call `ask` first, which asks and answers `true` when the user must answer.
pub fn ask_first_with(ask: impl Fn(&mut App) -> bool + 'static, cx: &mut App) {
    cx.set_global(Asker(Rc::new(ask)));
}

/// What Quit does: asks first through the shell, or quits at once. It runs once the action's
/// dispatch is over, since the window is busy dispatching it until then.
pub fn request(cx: &mut App) {
    cx.defer(|cx| {
        let ask = cx.try_global::<Asker>().map(|asker| asker.0.clone());
        if !ask.is_some_and(|ask| ask(cx)) {
            now(cx);
        }
    });
}

/// Quits the app. Saving the window's bounds runs on quit, as it does when the window closes.
pub fn now(cx: &mut App) {
    #[cfg(test)]
    {
        cx.default_global::<Quits>().0 += 1;
    }
    cx.quit();
}

/// How many times the app quit, under test, where quitting does nothing.
#[cfg(test)]
#[derive(Default)]
struct Quits(usize);

#[cfg(test)]
impl Global for Quits {}

/// How many times the app has quit so far.
#[cfg(test)]
pub fn quits(cx: &App) -> usize {
    cx.try_global::<Quits>().map_or(0, |quits| quits.0)
}

#[cfg(test)]
mod tests {
    use agentdeck_core::{Agent, AgentState};

    use super::*;
    use crate::overview::fixtures::{id, pane};

    fn snapshot(panes: Vec<crate::overview::PaneState>) -> Snapshot {
        Snapshot {
            panes,
            ..Snapshot::default()
        }
    }

    mod running_agents {
        use super::*;

        #[test]
        fn should_count_agent_panes_and_shells_hosting_one() {
            let agent = |n, state| crate::overview::PaneState {
                agent: Some(Agent::Claude),
                ..pane(n, id(1), state)
            };

            let panes = vec![
                agent(1, AgentState::Working),
                agent(2, AgentState::Blocked),
                pane(3, id(1), AgentState::Idle),
            ];

            assert_eq!(running_agents(&snapshot(panes)), 2);
        }

        #[test]
        fn should_count_none_for_plain_shells() {
            let panes = vec![pane(1, id(1), AgentState::Idle)];

            assert_eq!(running_agents(&snapshot(panes)), 0);
        }
    }

    mod still_running {
        use super::*;

        #[test]
        fn should_say_one_agent_is() {
            assert!(still_running(1).starts_with("1 agent is still running. "));
        }

        #[test]
        fn should_say_n_agents_are() {
            assert_eq!(
                still_running(2),
                "2 agents are still running. They keep running in the background, and \
                 reopening agentdeck reconnects to them."
            );
        }
    }
}
