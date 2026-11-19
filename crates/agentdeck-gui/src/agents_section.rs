//! Settings → Agents (spec §11), Penpot "Screens · Settings" → "Settings · Agents" and "·
//! added": whether agentdeck's MCP server is in Claude Code's and Codex's own config, in a
//! status box built as the GitHub account's, with **Add agentdeck to Claude Code and Codex**
//! under it.
//!
//! New agent panes always get agentdeck's tools from their launch flags (spec §6); this adds
//! them to agents the user starts in a shell pane. The box says what the button changes before
//! it is pressed. It shows `GET /v1/agents/mcp`, read as the section is made, when Settings
//! opens, and on each `resync`; the button adds it with `PUT /v1/agents/mcp` and toasts what
//! changed, or why an agent couldn't be added.

use std::sync::Arc;

use agentdeck_client::types::{McpAgent, McpRegistration, McpState};
use agentdeck_core::ws::ServerMessage;
use brrritscold_core_gpui::{ActiveTheme as _, Button, ButtonSize, ButtonVariant};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::{
    Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Window, div,
};

use crate::api::Api;
use crate::connection::{Connection, DaemonEvent};
use crate::notice::{self, Notice};

/// Under the section's heading.
pub const NOTE: &str = "New agent panes always get agentdeck's tools. Add them to Claude Code and Codex to have them in agents you start yourself in a shell.";

/// The button's label.
pub const ADD: &str = "Add agentdeck to Claude Code and Codex";

/// Claude Code and Codex as the section names them, with where agentdeck stands in each.
fn agents(registration: &McpRegistration) -> [(&'static str, &McpAgent); 2] {
    [
        ("Claude Code", &registration.claude),
        ("Codex", &registration.codex),
    ]
}

/// `names` joined with `word`: "Claude Code and Codex", "Codex".
fn joined(names: &[&str], word: &str) -> String {
    names.join(&format!(" {word} "))
}

/// The agents whose state is one of `states`.
fn named(registration: &McpRegistration, states: &[McpState]) -> Vec<&'static str> {
    agents(registration)
        .into_iter()
        .filter(|(_, agent)| states.contains(&agent.state))
        .map(|(name, _)| name)
        .collect()
}

/// The states the button changes: an entry missing, running something else, or unreadable
/// (adding says why it couldn't).
const PENDING: &[McpState] = &[McpState::Missing, McpState::Outdated, McpState::Unreadable];

/// The box's title: which agents have agentdeck, and which don't yet.
pub fn title(registration: &McpRegistration) -> String {
    let added = named(registration, &[McpState::Added]);
    let pending = named(registration, PENDING);
    match (added.is_empty(), pending.is_empty()) {
        (false, true) => format!("In {}", joined(&added, "and")),
        (true, false) => format!("Not in {} yet", joined(&pending, "or")),
        (false, false) => format!(
            "In {}, not in {} yet",
            joined(&added, "and"),
            joined(&pending, "or")
        ),
        (true, true) => "Claude Code and Codex aren't installed".to_owned(),
    }
}

/// What the button does to `agent`'s config, when it changes it.
fn change(name: &str, agent: &McpAgent) -> Option<String> {
    let config = &agent.config;
    match (name, agent.state) {
        ("Claude Code", McpState::Missing) => {
            Some("runs claude mcp add --scope user agentdeck".to_owned())
        }
        ("Claude Code", McpState::Outdated) => Some(
            "replaces Claude Code's agentdeck server with claude mcp add --scope user agentdeck"
                .to_owned(),
        ),
        (_, McpState::Missing) => Some(format!("adds [mcp_servers.agentdeck] to {config}")),
        (_, McpState::Outdated) => Some(format!("replaces [mcp_servers.agentdeck] in {config}")),
        _ => None,
    }
}

/// The line under the title: what the button changes, or that adding again changes nothing;
/// then why a config can't be read, and which agent isn't installed.
pub fn detail(registration: &McpRegistration) -> String {
    let changes: Vec<String> = agents(registration)
        .into_iter()
        .filter_map(|(name, agent)| change(name, agent))
        .collect();
    let mut sentences = Vec::new();
    if let Some((first, rest)) = changes.split_first() {
        let mut sentence = capitalized(first);
        for change in rest {
            sentence.push_str(&format!(", and {change}"));
        }
        sentences.push(format!("{sentence}."));
    } else if !named(registration, &[McpState::Added]).is_empty() {
        sentences.push(
            "Agents you start in a shell have agentdeck's tools. Adding again changes nothing."
                .to_owned(),
        );
    }
    for (_, agent) in agents(registration) {
        if let (McpState::Unreadable, Some(why)) = (agent.state, &agent.error) {
            sentences.push(format!("Can't read {}: {why}", agent.config));
        }
    }
    match named(registration, &[McpState::NotInstalled]).as_slice() {
        [] => {}
        [_, _] => sentences.push("Install either, then add agentdeck to it here.".to_owned()),
        [name] => sentences.push(format!("{name} isn't installed.")),
        _ => unreachable!("two agents"),
    }
    sentences.join(" ")
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// The toast after adding, from where agentdeck stood `before` to `after`: an error naming each
/// agent it couldn't be added to and why, else what was added, else that nothing changed.
pub fn added_notice(before: &McpRegistration, after: &McpRegistration) -> Notice {
    let failed: Vec<(&str, &str)> = agents(after)
        .into_iter()
        .filter_map(|(name, agent)| Some((name, agent.error.as_deref()?)))
        .collect();
    if !failed.is_empty() {
        let names: Vec<&str> = failed.iter().map(|(name, _)| *name).collect();
        let why = match failed.as_slice() {
            [(_, why)] => (*why).to_owned(),
            _ => failed
                .iter()
                .map(|(name, why)| format!("{name}: {why}"))
                .collect::<Vec<_>>()
                .join(" "),
        };
        return Notice::error(
            format!("Couldn't add agentdeck to {}", joined(&names, "and")),
            why,
        );
    }
    let now_added: Vec<&str> = agents(before)
        .into_iter()
        .zip(agents(after))
        .filter(|((_, was), (_, now))| was.state != McpState::Added && now.state == McpState::Added)
        .map(|((name, _), _)| name)
        .collect();
    if now_added.is_empty() {
        let added = named(after, &[McpState::Added]);
        return Notice::new(format!("agentdeck is already in {}", joined(&added, "and")));
    }
    Notice::new(format!("Added agentdeck to {}", joined(&now_added, "and")))
}

/// The section, kept by the Settings view.
pub struct AgentsSection {
    api: Arc<dyn Api>,
    /// As last read or added; `None` until the first read lands.
    registration: Option<McpRegistration>,
    /// While an add is on its way.
    adding: bool,
    _events: Subscription,
}

impl AgentsSection {
    /// A section over `api` that reads where agentdeck stands now, and again on each of
    /// `connection`'s `resync`.
    pub fn new(
        api: Arc<dyn Api>,
        connection: &Entity<Connection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let _events = cx.subscribe_in(connection, window, |this, _, event, window, cx| {
            if let DaemonEvent::Message(ServerMessage::Resync) = event {
                this.reread(window, cx);
            }
        });
        let mut section = AgentsSection {
            api,
            registration: None,
            adding: false,
            _events,
        };
        section.reread(window, cx);
        section
    }

    /// Where agentdeck stands, as last read.
    #[cfg(test)]
    pub fn registration(&self) -> Option<&McpRegistration> {
        self.registration.as_ref()
    }

    /// Reads where agentdeck stands, and shows it.
    pub fn reread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reading = self.api.mcp_registration(cx);
        cx.spawn_in(window, async move |this, cx| {
            let read = reading.await;
            let _ = this.update_in(cx, |this, window, cx| match read {
                Ok(registration) => {
                    this.registration = Some(registration);
                    cx.notify();
                }
                Err(err) => {
                    Notice::error(
                        "Couldn't check Claude Code and Codex",
                        notice::message(&err),
                    )
                    .show(window, cx);
                }
            });
        })
        .detach();
    }

    /// Whether the button does anything: a read has landed, no add is on its way, and at least
    /// one agent is installed.
    fn can_add(&self) -> bool {
        !self.adding
            && self.registration.as_ref().is_some_and(|registration| {
                agents(registration)
                    .iter()
                    .any(|(_, agent)| agent.state != McpState::NotInstalled)
            })
    }

    /// Adds agentdeck to the agents that don't have it yet, then shows where it stands and
    /// toasts what changed.
    pub fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_add() {
            return;
        }
        let Some(before) = self.registration.clone() else {
            return;
        };
        self.adding = true;
        cx.notify();
        let adding = self.api.add_mcp(cx);
        cx.spawn_in(window, async move |this, cx| {
            let added = adding.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.adding = false;
                match added {
                    Ok(after) => {
                        added_notice(&before, &after).show(window, cx);
                        this.registration = Some(after);
                    }
                    Err(err) => {
                        Notice::error(
                            "Couldn't add agentdeck to Claude Code and Codex",
                            notice::message(&err),
                        )
                        .show(window, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for AgentsSection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let heading = div()
            .flex()
            .flex_col()
            .gap(space::S1)
            .child(
                div()
                    .text_size(font_size::BASE)
                    .font_weight(font_weight::SEMIBOLD)
                    .text_color(s.foreground)
                    .child("Agents"),
            )
            .child(
                div()
                    .debug_selector(|| "agents note".into())
                    .text_size(font_size::SM)
                    .text_color(s.muted_foreground)
                    .child(NOTE),
            );
        // Penpot `Status`: the GitHub account's box, full width, its text at the top.
        let status = self.registration.as_ref().map(|registration| {
            div()
                .debug_selector(|| "agents status".into())
                .flex()
                .flex_col()
                .gap(space::S0_5)
                .p(space::S3)
                .rounded(s.radius_md)
                .border_1()
                .border_color(s.border)
                .child(
                    div()
                        .debug_selector(|| "agents title".into())
                        .text_size(font_size::SM)
                        .font_weight(font_weight::MEDIUM)
                        .text_color(s.foreground)
                        .child(title(registration)),
                )
                .child(
                    div()
                        .debug_selector(|| "agents detail".into())
                        .text_size(font_size::SM)
                        .text_color(s.muted_foreground)
                        .child(detail(registration)),
                )
        });
        let add = div().debug_selector(|| "agents add".into()).child(
            Button::new("agents-add")
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Sm)
                .label(ADD)
                .disabled(!self.can_add())
                .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
        );
        div()
            .debug_selector(|| "settings section Agents".into())
            .flex()
            .flex_col()
            .gap(space::S4)
            .child(heading)
            .child(
                // The button sits 12px under the box, at its own width.
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(space::S3)
                    .children(status.map(|status| status.w_full()))
                    .child(add),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use agentdeck_client::ws::{self, Remote};
    use brrritscold_core_gpui::Root;
    use gpui::{AppContext as _, Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::{FakeApi, mcp_missing};
    use crate::connection::fake::FakeConnector;

    fn with(claude: McpState, codex: McpState) -> McpRegistration {
        let mut registration = mcp_missing();
        registration.claude.state = claude;
        registration.codex.state = codex;
        registration
    }

    struct Harness {
        api: Arc<FakeApi>,
        view: Entity<AgentsSection>,
        remote: Remote,
        _connection: Entity<Connection>,
    }

    impl Harness {
        /// A `Root` window on the section, over a daemon where agentdeck stands as
        /// `registration` says.
        fn open(
            registration: McpRegistration,
            cx: &mut TestAppContext,
        ) -> (Harness, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            let api = Arc::new(FakeApi::default());
            api.set_mcp(registration);
            let connector = Rc::new(FakeConnector::default());
            let remote = connector.then_connect_to(api.clone());
            let connection = cx.update(|cx| cx.new(|cx| Connection::new(connector, cx)));
            let mut view = None;
            let window = cx.add_window(|window, cx| {
                let api: Arc<dyn Api> = api.clone();
                let section = cx.new(|cx| AgentsSection::new(api, &connection, window, cx));
                view = Some(section.clone());
                Root::new(section, window, cx)
            });
            let cx = VisualTestContext::from_window(*window, cx).into_mut();
            cx.update(|window, _| window.activate_window());
            let harness = Harness {
                api,
                view: view.unwrap(),
                remote,
                _connection: connection,
            };
            harness.settle(cx);
            (harness, cx)
        }

        fn settle(&self, cx: &mut VisualTestContext) {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        fn click_add(&self, cx: &mut VisualTestContext) {
            let bounds = cx.debug_bounds("agents add").expect("the button is drawn");
            cx.simulate_click(bounds.center(), Modifiers::default());
            self.settle(cx);
        }

        fn shown(&self, cx: &mut VisualTestContext) -> (String, String) {
            self.view.read_with(cx, |view, _| {
                let registration = view.registration().expect("a read landed");
                (title(registration), detail(registration))
            })
        }

        fn toasts(&self, cx: &mut VisualTestContext) -> Vec<Notice> {
            cx.update(|_, cx| notice::shown(cx))
        }
    }

    mod status {
        use super::*;

        /// Penpot "Settings · Agents".
        #[gpui::test]
        fn should_say_neither_has_agentdeck_and_what_adding_changes(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(mcp_missing(), cx);

            assert_eq!(
                harness.shown(cx),
                (
                    "Not in Claude Code or Codex yet".to_owned(),
                    "Runs claude mcp add --scope user agentdeck, and adds \
                     [mcp_servers.agentdeck] to ~/.codex/config.toml."
                        .to_owned()
                )
            );
        }

        /// Penpot "Settings · Agents · added".
        #[gpui::test]
        fn should_say_adding_again_changes_nothing_once_in_both(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(with(McpState::Added, McpState::Added), cx);

            assert_eq!(
                harness.shown(cx),
                (
                    "In Claude Code and Codex".to_owned(),
                    "Agents you start in a shell have agentdeck's tools. Adding again changes \
                     nothing."
                        .to_owned()
                )
            );
        }

        #[gpui::test]
        fn should_read_again_on_resync(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(mcp_missing(), cx);
            harness.api.set_mcp(with(McpState::Added, McpState::Added));

            harness
                .remote
                .events
                .try_send(ws::Event::Message(ServerMessage::Resync))
                .unwrap();
            harness.settle(cx);

            assert_eq!(harness.shown(cx).0, "In Claude Code and Codex");
        }

        /// The box spans the section, and the button sits 12px under it at its own width.
        #[gpui::test]
        fn should_put_the_button_12px_under_the_box(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(mcp_missing(), cx);

            let status = cx.debug_bounds("agents status").expect("the box");
            let add = cx.debug_bounds("agents add").expect("the button");

            assert_eq!(add.top(), status.bottom() + space::S3);
            assert_eq!(add.left(), status.left());
            assert!(add.size.width < status.size.width, "{add:?} {status:?}");
        }
    }

    mod add {
        use super::*;

        #[gpui::test]
        fn should_add_to_both_and_toast_it(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(mcp_missing(), cx);

            harness.click_add(cx);

            assert_eq!(*harness.api.mcp_adds.lock().unwrap(), 1);
            assert_eq!(harness.shown(cx).0, "In Claude Code and Codex");
            let toast = harness.toasts(cx).pop().expect("a toast");
            assert_eq!(toast.title, "Added agentdeck to Claude Code and Codex");
            assert!(toast.kind.is_none());
        }

        #[gpui::test]
        fn should_say_nothing_changed_when_already_in_both(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(with(McpState::Added, McpState::Added), cx);

            harness.click_add(cx);

            let toast = harness.toasts(cx).pop().expect("a toast");
            assert_eq!(toast.title, "agentdeck is already in Claude Code and Codex");
        }

        #[gpui::test]
        fn should_toast_why_codex_failed_and_show_claude_added(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(mcp_missing(), cx);
            let mut after = with(McpState::Added, McpState::Unreadable);
            after.codex.error =
                Some("~/.codex/config.toml: TOML parse error at line 1, column 4".into());
            harness.api.set_mcp_added(after);

            harness.click_add(cx);

            let toast = harness.toasts(cx).pop().expect("a toast");
            assert_eq!(toast.title, "Couldn't add agentdeck to Codex");
            assert_eq!(
                toast.description.as_deref(),
                Some("~/.codex/config.toml: TOML parse error at line 1, column 4")
            );
            assert_eq!(harness.shown(cx).0, "In Claude Code, not in Codex yet");
        }

        #[gpui::test]
        fn should_toast_why_when_the_daemon_could_not_be_asked(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(mcp_missing(), cx);
            harness.api.fail_next("connection refused");

            harness.click_add(cx);

            let toast = harness.toasts(cx).pop().expect("a toast");
            assert_eq!(
                toast.title,
                "Couldn't add agentdeck to Claude Code and Codex"
            );
            assert!(
                toast
                    .description
                    .as_deref()
                    .unwrap()
                    .contains("connection refused"),
                "{:?}",
                toast.description
            );
            assert_eq!(harness.shown(cx).0, "Not in Claude Code or Codex yet");
        }

        #[gpui::test]
        fn should_do_nothing_when_neither_is_installed(cx: &mut TestAppContext) {
            let (harness, cx) =
                Harness::open(with(McpState::NotInstalled, McpState::NotInstalled), cx);

            harness.click_add(cx);

            assert_eq!(*harness.api.mcp_adds.lock().unwrap(), 0);
            assert!(harness.toasts(cx).is_empty());
        }
    }

    mod wording {
        use super::*;

        #[test]
        fn should_name_the_one_agent_without_it_and_the_one_not_installed() {
            let registration = with(McpState::Added, McpState::NotInstalled);

            assert_eq!(title(&registration), "In Claude Code");
            assert_eq!(
                detail(&registration),
                "Agents you start in a shell have agentdeck's tools. Adding again changes \
                 nothing. Codex isn't installed."
            );
        }

        #[test]
        fn should_say_what_adding_does_to_the_one_without_it() {
            let registration = with(McpState::Added, McpState::Missing);

            assert_eq!(title(&registration), "In Claude Code, not in Codex yet");
            assert_eq!(
                detail(&registration),
                "Adds [mcp_servers.agentdeck] to ~/.codex/config.toml."
            );
        }

        #[test]
        fn should_say_what_it_replaces() {
            let registration = with(McpState::Outdated, McpState::Outdated);

            assert_eq!(title(&registration), "Not in Claude Code or Codex yet");
            assert_eq!(
                detail(&registration),
                "Replaces Claude Code's agentdeck server with claude mcp add --scope user \
                 agentdeck, and replaces [mcp_servers.agentdeck] in ~/.codex/config.toml."
            );
        }

        #[test]
        fn should_say_why_a_config_cant_be_read() {
            let mut registration = with(McpState::Missing, McpState::Unreadable);
            registration.codex.error = Some("TOML parse error at line 1, column 4".into());

            assert_eq!(
                detail(&registration),
                "Runs claude mcp add --scope user agentdeck. Can't read ~/.codex/config.toml: \
                 TOML parse error at line 1, column 4"
            );
        }

        #[test]
        fn should_say_neither_is_installed() {
            let registration = with(McpState::NotInstalled, McpState::NotInstalled);

            assert_eq!(
                title(&registration),
                "Claude Code and Codex aren't installed"
            );
            assert_eq!(
                detail(&registration),
                "Install either, then add agentdeck to it here."
            );
        }

        #[test]
        fn should_show_the_real_codex_home() {
            let mut registration = mcp_missing();
            registration.codex.config = "/srv/codex/config.toml".into();

            assert!(detail(&registration).ends_with("to /srv/codex/config.toml."));
        }
    }
}
