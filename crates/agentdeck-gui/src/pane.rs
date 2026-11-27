//! One pane of a tab (spec §11), Penpot's "Pane A" board: its header over its terminal.
//!
//! The header's title is the terminal's live one, then the one read over REST, then a name
//! for what runs there. Its state follows the daemon's `state` and `exit` broadcasts between
//! reads. Its menu asks the workspace holding it for a split, a close or its id copied, as a
//! [`PaneRequest`]: only the workspace knows the tab's layout.
//!
//! A shell pane hosting an agent (spec §3, §7) is an agent pane for all of that, and its
//! header says "in shell" (Penpot "Screens" → "Pane · hosted agent"). Once the agent leaves,
//! the title it showed by then no longer shows, so the header reads as the shell's again.
//!
//! An agent pane whose agent exited names no agent (Penpot "Screens" → "Pane · agent
//! exited"): its title is "Agent exited", and its header offers Restart agent and Open shell,
//! which the workspace carries out.
//!
//! A shell pane whose hosted agent left shows a strip under its header (Penpot "Screens ·
//! Main" → "Pane · hosted agent exited"): "**Claude Code** exited. Resume its session in this
//! shell?", then Resume · Dismiss. Resume asks the workspace to type the agent's resume
//! command into the shell; Dismiss hides the strip until an agent leaves again.

use std::rc::Rc;

use agentdeck_client::types::{LastHosted, Pane};
use agentdeck_client::ws;
use agentdeck_core::ws::ServerMessage;
use agentdeck_core::{Agent, AgentState, PaneId, PaneKind, SplitDirection};
use brrritscold_core_gpui::{
    ActiveTheme as _, Button, ButtonSize, ButtonVariant, DropdownMenu, DropdownMenuContent,
    DropdownMenuItem, DropdownMenuItemVariant, DropdownMenuSeparator, IconName, Separator,
};
use brrritscold_core_gpui_tokens::primitive::{opacity, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Window, div,
};

use crate::connection::{Connection, DaemonEvent};
use crate::new_agent::agent_name;
use crate::pane_header::PaneHeader;
use crate::suggestion_strip::{RowAction, Sentence, suggestion_row};
use crate::terminal::{self, TerminalEvent, TerminalView};

/// The agent running in `pane`: an agent pane's while its process runs, or the one a shell
/// pane hosts (spec §3).
pub fn running_agent(pane: &Pane) -> Option<Agent> {
    match pane.kind {
        PaneKind::Agent => pane.agent.filter(|_| pane.state != AgentState::Exited),
        PaneKind::Shell => pane.hosted_agent,
    }
}

/// The agent an agent pane ran, once its process has exited: what Restart agent runs again.
pub fn exited_agent(pane: &Pane) -> Option<Agent> {
    (pane.kind == PaneKind::Agent && pane.state == AgentState::Exited)
        .then_some(pane.agent)
        .flatten()
}

/// An exited agent pane's title, whatever the agent last set (spec §11).
pub const AGENT_EXITED: &str = "Agent exited";

/// Whether the header says "in shell": a shell pane hosting an agent, whose shell comes back
/// once the agent exits.
pub fn in_shell(pane: &Pane) -> bool {
    pane.kind == PaneKind::Shell && pane.hosted_agent.is_some()
}

/// The title a pane's header shows: "Agent exited" for an agent pane whose agent exited; else
/// the first of the terminal's live title and the one read over REST that is neither empty
/// nor `left`, the title a shell's hosted agent left behind; else the agent, else the program
/// run.
pub fn shown_title(live: Option<&str>, pane: &Pane, left: Option<&str>) -> SharedString {
    if exited_agent(pane).is_some() {
        return AGENT_EXITED.into();
    }
    if let Some(title) = [live, Some(pane.title.as_str())]
        .into_iter()
        .flatten()
        .find(|title| !title.is_empty() && Some(*title) != left)
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

/// What the hosted agent exited strip says after the agent's name.
pub const EXITED_TAIL: &str = "\u{a0}exited. Resume its session in this shell?";

/// The agent a shell pane hosted last, while the pane offers to resume it (spec §11): a
/// shell pane whose shell runs and that hosts no agent now. Dismissing it is the view's.
pub fn hosted_exit(pane: &Pane) -> Option<&LastHosted> {
    let shell = pane.kind == PaneKind::Shell && pane.state != AgentState::Exited;
    (shell && pane.hosted_agent.is_none())
        .then_some(pane.last_hosted.as_ref())
        .flatten()
}

/// What Resume types into the shell for the agent that left: `claude --resume <id>` or `codex
/// resume <id>` on its session; with none known, `claude --continue` or `codex resume
/// --last`, the latest session in the shell's folder. Only a UUID is typed as a session.
pub fn resume_command(last: &LastHosted) -> Option<String> {
    let session = (last.session_id.as_deref())
        .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
    Some(match (last.agent, session) {
        (Agent::Claude, Some(id)) => format!("claude --resume {id}"),
        (Agent::Claude, None) => "claude --continue".to_owned(),
        (Agent::Codex, Some(id)) => format!("codex resume {id}"),
        (Agent::Codex, None) => "codex resume --last".to_owned(),
        (Agent::Other, _) => return None,
    })
}

/// What a pane's header menu asks of the workspace holding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneRequest {
    /// Split it, a new shell taking the second side.
    Split(SplitDirection),
    /// Close it, killing what runs there.
    Close,
    /// Run its agent again: exited agent panes only.
    RestartAgent,
    /// Turn it into a shell: exited agent panes only.
    OpenShell,
    /// Copy its id, for `agentdeck agent send <pane>` and the like: every pane.
    CopyId,
    /// Type the resume command of the agent it hosted last into its shell: a shell pane's
    /// hosted agent exited strip ([`hosted_exit`]).
    ResumeHosted,
}

/// The buttons an exited agent pane's header offers, in order: Restart agent, then Open shell
/// (Penpot PaneHeader `Actions`). None for any other pane.
pub fn header_actions(pane: &Pane) -> Vec<(&'static str, PaneRequest)> {
    if exited_agent(pane).is_none() {
        return Vec::new();
    }
    vec![
        ("Restart agent", PaneRequest::RestartAgent),
        ("Open shell", PaneRequest::OpenShell),
    ]
}

/// The menu items before the separator and Close pane for every pane: the splits, then Copy
/// pane id.
pub fn menu_items() -> Vec<(&'static str, IconName, PaneRequest)> {
    vec![
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
        ("Copy pane id", IconName::Copy, PaneRequest::CopyId),
    ]
}

/// A pane: header, rule, terminal.
pub struct PaneView {
    pane: Pane,
    /// The title the terminal reported last.
    live_title: Option<SharedString>,
    /// The title shown when the shell's hosted agent left: the agent's, so it no longer shows,
    /// whether it comes live or over REST. Kept until the shell hosts an agent again.
    left_title: Option<SharedString>,
    /// Being closed: shown dimmed until it goes.
    closing: bool,
    /// The hosted agent exit whose strip was dismissed, or resumed; GUI state only.
    dismissed: Option<LastHosted>,
    terminal: Entity<TerminalView>,
    _subscriptions: [Subscription; 2],
}

impl PaneView {
    /// `pane`, attached over `connection`'s socket through `sender`.
    pub fn new(
        pane: Pane,
        connection: &Entity<Connection>,
        sender: ws::Sender,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let folder = pane.folder.clone().into();
        let terminal =
            cx.new(|cx| TerminalView::new(pane.id, folder, connection, sender, window, cx));
        let titles = cx.subscribe(&terminal, |this, _, event, cx| match event {
            TerminalEvent::Title(title) => {
                this.live_title = Some(title.clone());
                cx.notify();
            }
        });
        let states = cx.subscribe(connection, |this, _, event, cx| this.on_event(event, cx));
        PaneView {
            pane,
            live_title: None,
            left_title: None,
            closing: false,
            dismissed: None,
            terminal,
            _subscriptions: [titles, states],
        }
    }

    /// Its terminal.
    #[cfg(test)]
    pub fn terminal(&self) -> Entity<TerminalView> {
        self.terminal.clone()
    }

    /// The pane it shows.
    pub fn id(&self) -> PaneId {
        self.pane.id
    }

    /// Takes a fresh read of the pane.
    pub fn update(&mut self, pane: Pane, cx: &mut Context<Self>) {
        if self.pane != pane {
            match (in_shell(&self.pane), in_shell(&pane)) {
                (true, false) => self.left_title = Some(self.title()),
                (_, true) => self.left_title = None,
                (false, false) => {}
            }
            if self.pane.folder != pane.folder {
                let folder = pane.folder.clone().into();
                self.terminal
                    .update(cx, |terminal, _| terminal.set_folder(folder));
            }
            self.pane = pane;
            cx.notify();
        }
    }

    /// The title its header shows.
    pub fn title(&self) -> SharedString {
        shown_title(
            self.live_title.as_deref(),
            &self.pane,
            self.left_title.as_deref(),
        )
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

    /// The agent its hosted agent exited strip offers to resume, while it shows.
    pub fn hosted_exit(&self) -> Option<&LastHosted> {
        hosted_exit(&self.pane).filter(|last| self.dismissed.as_ref() != Some(*last))
    }

    /// Hides the hosted agent exited strip until another agent leaves.
    pub fn dismiss_hosted_exit(&mut self, cx: &mut Context<Self>) {
        self.dismissed = self.pane.last_hosted.clone();
        cx.notify();
    }

    /// Hides the strip and asks the workspace to resume the agent in the shell.
    pub fn resume_hosted(&mut self, cx: &mut Context<Self>) {
        self.dismiss_hosted_exit(cx);
        self.request(PaneRequest::ResumeHosted, cx);
    }

    /// Shows the strip again after a resume that failed.
    pub fn show_hosted_exit(&mut self, cx: &mut Context<Self>) {
        self.dismissed = None;
        cx.notify();
    }

    /// The hosted agent exited strip (Penpot "Pane · hosted agent exited"): a
    /// `SuggestionRow` with an `Info` icon, padded `space.3` across. Tests find it as `hosted
    /// exit`, its text as `hosted exit text` and its buttons as `resume hosted` and `dismiss
    /// hosted`.
    fn hosted_strip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let last = self.hosted_exit()?;
        let this = cx.entity().downgrade();
        let resume = this.clone();
        let id = self.pane.id;
        let row = suggestion_row(
            "hosted exit row".into(),
            IconName::Info,
            Sentence {
                selector: "hosted exit text".into(),
                lead: "",
                name: agent_name(last.agent).into(),
                tail: EXITED_TAIL,
            },
            [
                RowAction {
                    id: format!("resume-hosted-{id}").into(),
                    label: "Resume",
                    selector: "resume hosted".into(),
                    on_click: Rc::new(move |_, cx| {
                        let _ = resume.update(cx, |pane, cx| pane.resume_hosted(cx));
                    }),
                },
                RowAction {
                    id: format!("dismiss-hosted-{id}").into(),
                    label: "Dismiss",
                    selector: "dismiss hosted".into(),
                    on_click: Rc::new(move |_, cx| {
                        let _ = this.update(cx, |pane, cx| pane.dismiss_hosted_exit(cx));
                    }),
                },
            ],
            cx,
        );
        Some(
            div()
                .debug_selector(|| "hosted exit".into())
                .flex_none()
                .px(space::S3)
                .child(row)
                .into_any_element(),
        )
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
                (menu_items().into_iter())
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

    /// The header's `Actions` part: an outline xs button per [`header_actions`]; tests find
    /// each by its label.
    fn actions(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        (header_actions(&self.pane).into_iter())
            .map(|(label, request)| {
                div()
                    .debug_selector(move || label.into())
                    .child(
                        Button::new(label)
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Xs)
                            .label(label)
                            .on_click(cx.listener(move |pane, _, _, cx| pane.request(request, cx))),
                    )
                    .into_any_element()
            })
            .collect()
    }

    fn on_event(&mut self, event: &DaemonEvent, cx: &mut Context<Self>) {
        match event {
            DaemonEvent::Message(ServerMessage::State { pane, state }) if *pane == self.id() => {
                self.pane.state = *state;
            }
            DaemonEvent::Message(ServerMessage::Exit { pane, code }) if *pane == self.id() => {
                self.pane.state = AgentState::Exited;
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

impl PaneView {
    /// Its header: its title, state, "in shell" while it hosts an agent, its actions and its
    /// menu.
    pub fn header(&self, cx: &mut Context<Self>) -> PaneHeader {
        PaneHeader::new(self.title())
            .state(self.pane.state, self.pane.exit_code)
            .in_shell(in_shell(&self.pane))
            .actions(self.actions(cx))
            .menu(self.menu(cx))
    }
}

impl Render for PaneView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let header = self.header(cx);
        let strip = self.hosted_strip(cx);
        // Penpot "Pane A": `background`, a `border` stroke inside, `radius-md`, clipped;
        // Header / HeaderRule / Terminal, with the hosted agent exited strip and its own rule
        // between the header's rule and the terminal while it shows.
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
            .when_some(strip, |pane, strip| {
                pane.child(strip).child(Separator::new())
            })
            .child(
                div()
                    .debug_selector(|| "pane terminal".into())
                    .flex_1()
                    .min_h_0()
                    .child(terminal::cached(self.terminal.clone())),
            )
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
            folder: "/w".into(),
            hosted_agent: None,
            last_hosted: None,
            id: id(n),
            kind: PaneKind::Shell,
            session_id: None,
            state: AgentState::Idle,
            tab_id: tab,
            title: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use agentdeck_core::{Agent, AgentState};

    use super::fixtures::shell_pane;
    use super::*;
    use crate::overview::fixtures::id;

    mod menu_items {
        use super::*;

        #[test]
        fn should_offer_the_splits_then_copy_pane_id() {
            let labels: Vec<_> = (menu_items().into_iter())
                .map(|(label, ..)| label)
                .collect();

            assert_eq!(labels, ["Split right", "Split down", "Copy pane id"]);
            assert_eq!(menu_items()[2].2, PaneRequest::CopyId);
        }
    }

    fn exited(agent: Agent) -> Pane {
        Pane {
            state: AgentState::Exited,
            exit_code: Some(0),
            ..agent_pane(agent)
        }
    }

    mod header_actions {
        use super::*;

        #[test]
        fn should_offer_restart_agent_then_open_shell_once_the_agent_exited() {
            assert_eq!(
                header_actions(&exited(Agent::Codex)),
                [
                    ("Restart agent", PaneRequest::RestartAgent),
                    ("Open shell", PaneRequest::OpenShell)
                ]
            );
        }

        #[test]
        fn should_offer_none_while_the_agent_runs_or_for_a_shell() {
            assert!(header_actions(&agent_pane(Agent::Claude)).is_empty());
            let shell = Pane {
                state: AgentState::Exited,
                ..shell_pane(1, id(1))
            };
            assert!(header_actions(&shell).is_empty());
        }
    }

    const SESSION: &str = "5973b6c0-94b8-487b-a530-2aeb6098ae0e";

    fn last(agent: Agent, session_id: Option<&str>) -> LastHosted {
        LastHosted {
            agent,
            session_id: session_id.map(str::to_owned),
            left_at: "2026-11-27T10:00:00Z".into(),
        }
    }

    mod resume_command {
        use super::*;

        #[test]
        fn should_resume_the_agents_session() {
            assert_eq!(
                resume_command(&last(Agent::Claude, Some(SESSION))).unwrap(),
                format!("claude --resume {SESSION}")
            );
            assert_eq!(
                resume_command(&last(Agent::Codex, Some(SESSION))).unwrap(),
                format!("codex resume {SESSION}")
            );
        }

        #[test]
        fn should_resume_the_folders_latest_session_without_a_known_one() {
            assert_eq!(
                resume_command(&last(Agent::Claude, None)).unwrap(),
                "claude --continue"
            );
            assert_eq!(
                resume_command(&last(Agent::Codex, None)).unwrap(),
                "codex resume --last"
            );
        }

        #[test]
        fn should_type_only_a_uuid_into_the_shell() {
            assert_eq!(
                resume_command(&last(Agent::Claude, Some("x; rm -rf ~"))).unwrap(),
                "claude --continue"
            );
        }

        #[test]
        fn should_have_none_for_another_agent() {
            assert_eq!(resume_command(&last(Agent::Other, None)), None);
        }
    }

    mod hosted_exit {
        use super::*;

        fn left(agent: Agent) -> Pane {
            Pane {
                last_hosted: Some(last(agent, None)),
                ..shell_pane(1, id(1))
            }
        }

        #[test]
        fn should_offer_the_agent_a_shell_hosted_last() {
            assert_eq!(
                hosted_exit(&left(Agent::Codex)),
                Some(&last(Agent::Codex, None))
            );
        }

        #[test]
        fn should_offer_none_while_the_shell_hosts_an_agent() {
            let hosting = Pane {
                hosted_agent: Some(Agent::Claude),
                ..left(Agent::Claude)
            };
            assert_eq!(hosted_exit(&hosting), None);
        }

        #[test]
        fn should_offer_none_once_the_shell_exited() {
            let exited = Pane {
                state: AgentState::Exited,
                ..left(Agent::Claude)
            };
            assert_eq!(hosted_exit(&exited), None);
        }

        #[test]
        fn should_offer_none_in_an_agent_pane_or_a_shell_that_hosted_none() {
            let agent = Pane {
                last_hosted: Some(last(Agent::Claude, None)),
                ..exited(Agent::Claude)
            };
            assert_eq!(hosted_exit(&agent), None);
            assert_eq!(hosted_exit(&shell_pane(1, id(1))), None);
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

        #[test]
        fn should_be_none_once_an_agent_panes_process_exited() {
            assert_eq!(running_agent(&exited(Agent::Claude)), None);
            assert_eq!(exited_agent(&exited(Agent::Claude)), Some(Agent::Claude));
            assert_eq!(exited_agent(&agent_pane(Agent::Claude)), None);
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

            assert_eq!(shown_title(Some("live"), &pane, None), "live");
        }

        #[test]
        fn should_fall_back_to_the_title_read_while_none_is_live() {
            let pane = Pane {
                title: "read".into(),
                ..shell_pane(1, id(1))
            };

            assert_eq!(shown_title(None, &pane, None), "read");
            assert_eq!(shown_title(Some(""), &pane, None), "read");
        }

        #[test]
        fn should_name_the_agent_when_no_title_is_set() {
            let pane = Pane {
                agent: Some(Agent::Codex),
                argv: vec!["codex".into(), "resume".into()],
                ..shell_pane(1, id(1))
            };

            assert_eq!(shown_title(None, &pane, None), "codex");
        }

        #[test]
        fn should_say_agent_exited_for_an_exited_agent_whatever_it_last_set() {
            let pane = Pane {
                title: "✳ Fix the login bug".into(),
                ..exited(Agent::Claude)
            };

            assert_eq!(
                shown_title(Some("✳ Fix the login bug"), &pane, None),
                "Agent exited"
            );
            assert_eq!(
                shown_title(None, &exited(Agent::Codex), None),
                "Agent exited"
            );
        }

        #[test]
        fn should_skip_the_title_a_hosted_agent_left_behind() {
            let pane = Pane {
                title: "✳ Claude Code".into(),
                ..shell_pane(1, id(1))
            };
            let left = Some("✳ Claude Code");

            assert_eq!(shown_title(Some("✳ Claude Code"), &pane, left), "sh");
            assert_eq!(shown_title(Some("~/w"), &pane, left), "~/w");
        }

        #[test]
        fn should_name_the_program_of_an_untitled_shell() {
            assert_eq!(shown_title(None, &shell_pane(1, id(1)), None), "sh");
        }
    }
}
