//! Quitting (spec §11). Agents keep running after the GUI quits, so while one runs, closing the
//! window and Quit ask first, unless `gui.confirm_quit` is off; the dialog can stop them
//! instead. The main window's shell asks; this module finds the running agents, words the
//! question and routes the app-wide Quit to the shell.

use std::rc::Rc;
use std::time::Duration;

use agentdeck_core::{AgentState, PaneId};
use gpui::{App, Global};

use crate::overview::Snapshot;

/// How long quitting with Stop the agents ticked waits for the stops to land: the daemon's
/// grace between SIGHUP and SIGKILL (spec §4). It quits then whether or not they all did.
pub const STOP_GRACE: Duration = Duration::from_secs(3);

/// The panes whose agents run: agent panes whose process hasn't exited, and shell panes
/// hosting an agent. Plain shells don't count.
pub fn running_agents(snapshot: &Snapshot) -> Vec<PaneId> {
    (snapshot.panes.iter())
        .filter(|pane| pane.agent.is_some() && pane.state != AgentState::Exited)
        .map(|pane| pane.pane)
        .collect()
}

/// The dialog's description for `n` running agents, as it reads while Stop the agents is
/// ticked (`stop`) or not.
pub fn still_running(n: usize, stop: bool) -> String {
    let count = if n == 1 {
        "1 agent is".to_owned()
    } else {
        format!("{n} agents are")
    };
    let then = match (stop, n == 1) {
        (false, _) => {
            "They keep running in the background, and reopening agentdeck reconnects to them."
        }
        (true, true) => "It'll be stopped; Restart agent on its pane starts it again.",
        (true, false) => "They'll be stopped; Restart agent on each pane starts them again.",
    };
    format!("{count} still running. {then}")
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

            assert_eq!(running_agents(&snapshot(panes)), [id(1), id(2)]);
        }

        #[test]
        fn should_count_none_for_plain_shells() {
            let panes = vec![pane(1, id(1), AgentState::Idle)];

            assert_eq!(running_agents(&snapshot(panes)), []);
        }

        #[test]
        fn should_leave_out_an_agent_that_exited_since_the_read() {
            let panes = vec![crate::overview::PaneState {
                agent: Some(Agent::Codex),
                ..pane(1, id(1), AgentState::Exited)
            }];

            assert_eq!(running_agents(&snapshot(panes)), []);
        }
    }

    mod still_running {
        use super::*;

        #[test]
        fn should_say_one_agent_is() {
            assert!(still_running(1, false).starts_with("1 agent is still running. "));
        }

        #[test]
        fn should_say_n_agents_are() {
            assert_eq!(
                still_running(2, false),
                "2 agents are still running. They keep running in the background, and \
                 reopening agentdeck reconnects to them."
            );
        }

        #[test]
        fn should_say_they_will_be_stopped_while_stop_the_agents_is_ticked() {
            assert_eq!(
                still_running(2, true),
                "2 agents are still running. They'll be stopped; Restart agent on each pane \
                 starts them again."
            );
        }

        #[test]
        fn should_say_it_will_be_stopped_for_one_agent() {
            assert_eq!(
                still_running(1, true),
                "1 agent is still running. It'll be stopped; Restart agent on its pane starts \
                 it again."
            );
        }
    }
}
