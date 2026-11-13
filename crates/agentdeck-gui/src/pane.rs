//! One pane of a tab (spec §11), Penpot's "Pane A" board: its header over its terminal.
//!
//! The header's title is the terminal's live one, then the one read over REST, then a name
//! for what runs there. Its state follows the daemon's `state` and `exit` broadcasts between
//! reads.

use agentdeck_client::types::Pane;
use agentdeck_client::ws;
use agentdeck_core::PaneId;
use agentdeck_core::ws::ServerMessage;
use brrritscold_core_gpui::{ActiveTheme as _, Separator};
use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Subscription, Window, div,
};

use crate::connection::{Connection, DaemonEvent};
use crate::pane_header::PaneHeader;
use crate::terminal::{TerminalEvent, TerminalView};

/// The title a pane's header shows: the first that isn't empty of the terminal's live title
/// and the one read over REST; else the agent, else the program run.
pub fn shown_title(live: Option<&str>, pane: &Pane) -> SharedString {
    if let Some(title) = [live, Some(pane.title.as_str())]
        .into_iter()
        .flatten()
        .find(|title| !title.is_empty())
    {
        return SharedString::from(title.to_owned());
    }
    if let Some(agent) = pane.agent {
        return agent.as_str().into();
    }
    let program = pane
        .argv
        .first()
        .map(|arg| arg.rsplit('/').next().unwrap_or(arg));
    program.unwrap_or("shell").to_owned().into()
}

/// A pane: header, rule, terminal.
pub struct PaneView {
    pane: Pane,
    /// The linked task's title.
    task: Option<SharedString>,
    /// The title the terminal reported last.
    live_title: Option<SharedString>,
    terminal: Entity<TerminalView>,
    _subscriptions: [Subscription; 2],
}

impl PaneView {
    /// `pane`, linked to the task titled `task`, attached over `connection`'s socket through
    /// `sender`.
    pub fn new(
        pane: Pane,
        task: Option<SharedString>,
        connection: &Entity<Connection>,
        sender: ws::Sender,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let terminal = cx.new(|cx| TerminalView::new(pane.id, connection, sender, window, cx));
        let titles = cx.subscribe(&terminal, |this, _, event, cx| match event {
            TerminalEvent::Title(title) => {
                this.live_title = Some(title.clone());
                cx.notify();
            }
        });
        let states = cx.subscribe(connection, |this, _, event, cx| this.on_event(event, cx));
        PaneView {
            pane,
            task,
            live_title: None,
            terminal,
            _subscriptions: [titles, states],
        }
    }

    /// The pane it shows.
    pub fn id(&self) -> PaneId {
        self.pane.id
    }

    /// Takes a fresh read of the pane and its task's title.
    pub fn update(&mut self, pane: Pane, task: Option<SharedString>, cx: &mut Context<Self>) {
        if (&self.pane, &self.task) != (&pane, &task) {
            self.pane = pane;
            self.task = task;
            cx.notify();
        }
    }

    /// The title its header shows.
    pub fn title(&self) -> SharedString {
        shown_title(self.live_title.as_deref(), &self.pane)
    }

    /// The pane as last read, with the state broadcasts since applied.
    #[cfg(test)]
    pub fn pane(&self) -> &Pane {
        &self.pane
    }

    /// The linked task's title.
    #[cfg(test)]
    pub fn task(&self) -> Option<&SharedString> {
        self.task.as_ref()
    }

    fn on_event(&mut self, event: &DaemonEvent, cx: &mut Context<Self>) {
        match event {
            DaemonEvent::Message(ServerMessage::State { pane, state }) if *pane == self.id() => {
                self.pane.state = *state;
            }
            DaemonEvent::Message(ServerMessage::Exit { pane, code }) if *pane == self.id() => {
                self.pane.state = agentdeck_core::AgentState::Exited;
                self.pane.exit_code = *code;
            }
            _ => return,
        }
        cx.notify();
    }
}

impl Focusable for PaneView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.terminal.focus_handle(cx)
    }
}

impl Render for PaneView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let header = PaneHeader::new(self.title())
            .state(self.pane.state, self.pane.exit_code)
            .task(self.task.clone());
        // Penpot "Pane A": `background`, a `border` stroke inside, `radius-md`, clipped;
        // Header / HeaderRule / Terminal.
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(s.radius_md)
            .border_1()
            .border_color(s.border)
            .bg(s.background)
            .child(header)
            .child(Separator::new())
            .child(div().flex_1().min_h_0().child(self.terminal.clone()))
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use agentdeck_client::types::Pane;
    use agentdeck_core::{AgentState, PaneKind, TabId};

    use crate::overview::fixtures::id;

    /// An idle `/bin/sh` pane in `tab`, with no title yet.
    pub fn shell_pane(n: u32, tab: TabId) -> Pane {
        Pane {
            agent: None,
            argv: vec!["/bin/sh".into()],
            created_at: "2026-11-12T00:00:00Z".into(),
            cwd: "/w".into(),
            exit_code: None,
            id: id(n),
            kind: PaneKind::Shell,
            session_id: None,
            state: AgentState::Idle,
            tab_id: tab,
            task_id: None,
            title: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use agentdeck_core::Agent;

    use super::fixtures::shell_pane;
    use super::*;
    use crate::overview::fixtures::id;

    mod shown_title {
        use super::*;

        #[test]
        fn should_prefer_the_live_title() {
            let pane = Pane {
                title: "read".into(),
                ..shell_pane(1, id(1))
            };

            assert_eq!(shown_title(Some("live"), &pane), "live");
        }

        #[test]
        fn should_fall_back_to_the_title_read_while_none_is_live() {
            let pane = Pane {
                title: "read".into(),
                ..shell_pane(1, id(1))
            };

            assert_eq!(shown_title(None, &pane), "read");
            assert_eq!(shown_title(Some(""), &pane), "read");
        }

        #[test]
        fn should_name_the_agent_when_no_title_is_set() {
            let pane = Pane {
                agent: Some(Agent::Codex),
                argv: vec!["codex".into(), "resume".into()],
                ..shell_pane(1, id(1))
            };

            assert_eq!(shown_title(None, &pane), "codex");
        }

        #[test]
        fn should_name_the_program_of_an_untitled_shell() {
            assert_eq!(shown_title(None, &shell_pane(1, id(1))), "sh");
        }
    }
}
