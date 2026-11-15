//! One pane of a tab (spec §11), Penpot's "Pane A" board: its header over its terminal.
//!
//! The header's title is the terminal's live one, then the one read over REST, then a name
//! for what runs there. Its state follows the daemon's `state` and `exit` broadcasts between
//! reads. Its menu asks the workspace holding it for a split, a close or, for an agent, a
//! link to a task, as a [`PaneRequest`]: only the workspace knows the tab's layout.
//!
//! A shell pane hosting an agent (spec §3, §7) is an agent pane for all of that, and its
//! header says "in shell" (Penpot "Screens" → "Pane · hosted agent").

use agentdeck_client::types::Pane;
use agentdeck_client::ws;
use agentdeck_core::ws::ServerMessage;
use agentdeck_core::{Agent, PaneId, PaneKind, SplitDirection};
use brrritscold_core_gpui::{
    ActiveTheme as _, Button, ButtonSize, ButtonVariant, DropdownMenu, DropdownMenuContent,
    DropdownMenuItem, DropdownMenuItemVariant, DropdownMenuSeparator, IconName, Separator,
};
use brrritscold_core_gpui_tokens::primitive::opacity;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
};

use crate::connection::{Connection, DaemonEvent};
use crate::pane_header::PaneHeader;
use crate::terminal::{TerminalEvent, TerminalView};

/// The agent running in `pane`: an agent pane's, or the one a shell pane hosts (spec §3).
pub fn running_agent(pane: &Pane) -> Option<Agent> {
    match pane.kind {
        PaneKind::Agent => pane.agent,
        PaneKind::Shell => pane.hosted_agent,
    }
}

/// Whether the header says "in shell": a shell pane hosting an agent, whose shell comes back
/// once the agent exits.
pub fn in_shell(pane: &Pane) -> bool {
    pane.kind == PaneKind::Shell && pane.hosted_agent.is_some()
}

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
    if let Some(agent) = running_agent(pane) {
        return agent.as_str().into();
    }
    let program = pane
        .argv
        .first()
        .map(|arg| arg.rsplit('/').next().unwrap_or(arg));
    program.unwrap_or("shell").to_owned().into()
}

/// What a pane's header menu asks of the workspace holding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneRequest {
    /// Split it, a new shell taking the second side.
    Split(SplitDirection),
    /// Close it, killing what runs there.
    Close,
    /// Link the agent in it to a task, or unlink it: agent panes only.
    LinkTask,
}

/// The menu items before the separator and Close pane for `pane`: the splits, then, for an
/// agent pane or a shell pane hosting an agent, Link to task….
pub fn menu_items(pane: &Pane) -> Vec<(&'static str, IconName, PaneRequest)> {
    let mut items = vec![
        (
            "Split right",
            IconName::Columns2,
            PaneRequest::Split(SplitDirection::Horizontal),
        ),
        (
            "Split down",
            IconName::Rows2,
            PaneRequest::Split(SplitDirection::Vertical),
        ),
    ];
    if running_agent(pane).is_some() {
        items.push(("Link to task…", IconName::Link, PaneRequest::LinkTask));
    }
    items
}

/// A pane: header, rule, terminal.
pub struct PaneView {
    pane: Pane,
    /// The linked task's title.
    task: Option<SharedString>,
    /// The title the terminal reported last.
    live_title: Option<SharedString>,
    /// Being closed: shown dimmed until it goes.
    closing: bool,
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
        let cwd = pane.cwd.clone().into();
        let terminal = cx.new(|cx| TerminalView::new(pane.id, cwd, connection, sender, window, cx));
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
            closing: false,
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
    pub fn pane(&self) -> &Pane {
        &self.pane
    }

    /// Dims it while it is being closed, or brings it back when the close failed.
    pub fn set_closing(&mut self, closing: bool, cx: &mut Context<Self>) {
        if self.closing != closing {
            self.closing = closing;
            cx.notify();
        }
    }

    /// Whether it is being closed.
    #[cfg(test)]
    pub fn is_closing(&self) -> bool {
        self.closing
    }

    /// Asks the workspace holding it for `request`.
    pub fn request(&mut self, request: PaneRequest, cx: &mut Context<Self>) {
        cx.emit(request);
    }

    /// The header's `Menu` part: a ghost icon-xs `Ellipsis` button opening the pane's actions.
    fn menu(&self, cx: &mut Context<Self>) -> DropdownMenu {
        let this = cx.entity().downgrade();
        let item = |label: &'static str, icon: IconName, request: PaneRequest| {
            let this = this.clone();
            DropdownMenuItem::new(label)
                .icon(icon)
                .on_click(move |_, _, cx| {
                    let _ = this.update(cx, |pane, cx| pane.request(request, cx));
                })
        };
        DropdownMenu::new(SharedString::from(format!("pane-menu-{}", self.pane.id)))
            .trigger(
                Button::new("menu")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::IconXs)
                    .icon(IconName::Ellipsis),
            )
            .content(
                (menu_items(&self.pane).into_iter())
                    .fold(
                        DropdownMenuContent::new(),
                        |content, (label, icon, request)| content.item(item(label, icon, request)),
                    )
                    .separator(DropdownMenuSeparator::new())
                    .item(
                        item("Close pane", IconName::X, PaneRequest::Close)
                            .variant(DropdownMenuItemVariant::Destructive),
                    ),
            )
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

impl EventEmitter<PaneRequest> for PaneView {}

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
            .in_shell(in_shell(&self.pane))
            .task(self.task.clone())
            .menu(self.menu(cx));
        // Penpot "Pane A": `background`, a `border` stroke inside, `radius-md`, clipped;
        // Header / HeaderRule / Terminal.
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            // Being closed: dimmed as a disabled control is (`opacity-50`).
            .when(self.closing, |pane| pane.opacity(opacity::O50))
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
            hosted_agent: None,
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

    mod menu_items {
        use super::*;

        fn labels(pane: &Pane) -> Vec<&'static str> {
            menu_items(pane)
                .into_iter()
                .map(|(label, ..)| label)
                .collect()
        }

        #[test]
        fn should_offer_link_to_task_after_the_splits_for_an_agent() {
            let pane = agent_pane(Agent::Claude);
            assert_eq!(
                labels(&pane),
                ["Split right", "Split down", "Link to task…"]
            );
            assert_eq!(menu_items(&pane)[2].2, PaneRequest::LinkTask);
        }

        #[test]
        fn should_offer_link_to_task_for_a_shell_while_it_hosts_an_agent() {
            assert_eq!(
                labels(&hosting(Agent::Codex)),
                ["Split right", "Split down", "Link to task…"]
            );
        }

        #[test]
        fn should_offer_only_the_splits_for_a_shell() {
            assert_eq!(labels(&shell_pane(1, id(1))), ["Split right", "Split down"]);
        }
    }

    fn agent_pane(agent: Agent) -> Pane {
        Pane {
            kind: PaneKind::Agent,
            agent: Some(agent),
            ..shell_pane(1, id(1))
        }
    }

    fn hosting(agent: Agent) -> Pane {
        Pane {
            hosted_agent: Some(agent),
            ..shell_pane(1, id(1))
        }
    }

    mod running_agent {
        use super::*;

        #[test]
        fn should_be_an_agent_panes_agent() {
            assert_eq!(running_agent(&agent_pane(Agent::Codex)), Some(Agent::Codex));
        }

        #[test]
        fn should_be_the_agent_a_shell_hosts() {
            assert_eq!(running_agent(&hosting(Agent::Claude)), Some(Agent::Claude));
        }

        #[test]
        fn should_be_none_for_a_shell_hosting_none() {
            assert_eq!(running_agent(&shell_pane(1, id(1))), None);
        }
    }

    mod in_shell {
        use super::*;

        #[test]
        fn should_mark_only_a_shell_hosting_an_agent() {
            assert!(in_shell(&hosting(Agent::Claude)));
            assert!(!in_shell(&shell_pane(1, id(1))));
            assert!(!in_shell(&agent_pane(Agent::Claude)));
        }
    }

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
