//! The new agent dialog (spec §6, §11), Penpot "Screens" → "New agent": an agent CLI and the
//! folder it runs in: where a new pane starts (the focused pane's current folder, else `~`),
//! another pane's folder, or `~`. A workspace has no folder of its own to offer. The agent
//! starts with no prompt. Enter starts, as Start agent does, unless a select
//! has it.
//!
//! The top bar's "New agent" button asks the window for it with [`NewAgent`]. Once the
//! daemon has started the agent and it has a place in a tab, the dialog closes and emits
//! [`Launched`], so the window can show that tab and focus the pane: the agent may be asking
//! about trust (spec §6).

use std::path::Path;
use std::sync::Arc;

use agentdeck_client::ApiError;
use agentdeck_core::{Agent, PaneId, TabId, WorkspaceId};
use brrritscold_core_gpui::{
    Button, ButtonVariant, Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter,
    DialogHandle, DialogHeader, DialogTitle, Field, FieldDescription, Label, Select, SelectContent,
    SelectItem, SelectTrigger,
};
use brrritscold_core_gpui_tokens::primitive::space;
use gpui::{
    Context, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Window, actions, div,
};

use crate::api::Api;
use crate::keymap::{self, ConfirmDialog};
use crate::workspace::failed;

actions!(
    agentdeck,
    [
        /// Opens the new agent dialog on the workspace shown.
        NewAgent
    ]
);

/// The dialog started an agent: its workspace, the tab it was placed in, and its pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Launched {
    /// The workspace.
    pub workspace: WorkspaceId,
    /// The tab.
    pub tab: TabId,
    /// The pane.
    pub pane: PaneId,
}

/// What the dialog asks the daemon to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The workspace.
    pub workspace: WorkspaceId,
    /// The agent CLI.
    pub agent: Agent,
    /// Where it runs: an absolute dir.
    pub cwd: String,
}

/// The agents the dialog offers, with their names.
pub const AGENTS: [(Agent, &str); 2] = [(Agent::Claude, "Claude Code"), (Agent::Codex, "Codex")];

/// The agent's name: `Claude Code`, or `Agent` for another CLI.
pub fn agent_name(agent: Agent) -> &'static str {
    (AGENTS.iter())
        .find(|(a, _)| *a == agent)
        .map_or("Agent", |(_, name)| name)
}

/// The folder field's description.
pub const FOLDER_HINT: &str =
    "Where the focused pane is, another pane's folder, or your home folder.";

/// One folder to run in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// What the picker shows: its path, with `~` for the home folder.
    pub label: SharedString,
    /// The absolute folder.
    pub path: SharedString,
}

/// `folder` as the dialog shows it: under `home`, with `~` for `home`.
pub fn shown_folder(folder: &str, home: Option<&Path>) -> String {
    let under_home = home.and_then(|home| Path::new(folder).strip_prefix(home).ok());
    match under_home {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => folder.to_owned(),
    }
}

/// The user's home folder, `~`: where a new pane starts when its workspace has no pane to
/// start beside (spec §11); `/` when there is none.
pub fn home_folder() -> String {
    std::env::home_dir().map_or_else(|| "/".to_owned(), |home| home.display().to_string())
}

/// The folders to offer, each once: `here`, where a new pane starts (spec §11), first; then
/// each folder in `panes` (the workspace's panes' current folders), in order; then `home`.
pub fn folders(here: &str, panes: &[String], home: Option<&Path>) -> Vec<Folder> {
    let home_folder = home.map(|home| home.display().to_string());
    let mut offered: Vec<Folder> = Vec::new();
    let candidates = (std::iter::once(here))
        .chain(panes.iter().map(String::as_str))
        .chain(home_folder.as_deref());
    for path in candidates {
        if !offered
            .iter()
            .any(|f| Path::new(f.path.as_ref()) == Path::new(path))
        {
            offered.push(Folder {
                label: shown_folder(path, home).into(),
                path: path.to_owned().into(),
            });
        }
    }
    offered
}

/// The workspace the dialog is open on.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// Its id.
    pub id: WorkspaceId,
    /// Its name, for the description.
    pub name: SharedString,
    /// The folder it starts in: where a new pane there starts (spec §11), one of the
    /// folders offered.
    pub folder: SharedString,
}

/// The dialog, kept by the window.
pub struct NewAgentDialog {
    api: Arc<dyn Api>,
    handle: DialogHandle,
    target: Option<Target>,
    folders: Vec<Folder>,
    agent: Agent,
    folder: SharedString,
    /// Whether a launch is out; another waits for it.
    launching: bool,
}

impl EventEmitter<Launched> for NewAgentDialog {}

impl NewAgentDialog {
    /// A closed dialog that starts agents through `api`.
    pub fn new(api: Arc<dyn Api>) -> Self {
        NewAgentDialog {
            api,
            handle: DialogHandle::new(false),
            target: None,
            folders: Vec::new(),
            agent: Agent::Claude,
            folder: SharedString::default(),
            launching: false,
        }
    }

    /// Opens it on `target`, offering `folders`: Claude Code in the target's folder.
    pub fn open(
        &mut self,
        target: Target,
        folders: Vec<Folder>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.folder = target.folder.clone();
        self.folders = folders;
        self.target = Some(target);
        self.agent = Agent::Claude;
        self.handle.open(window, cx);
        cx.notify();
    }

    /// Whether it is open.
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.handle.is_open()
    }

    /// The workspace it's open on.
    #[cfg(test)]
    pub fn workspace(&self) -> Option<WorkspaceId> {
        self.target.as_ref().map(|target| target.id)
    }

    /// The folder picked, and the folders offered.
    #[cfg(test)]
    pub fn folders(&self) -> (SharedString, Vec<Folder>) {
        (self.folder.clone(), self.folders.clone())
    }

    /// Picks the agent.
    pub fn set_agent(&mut self, agent: Agent, cx: &mut Context<Self>) {
        self.agent = agent;
        cx.notify();
    }

    /// Picks the folder, by its path.
    pub fn set_folder(&mut self, path: SharedString, cx: &mut Context<Self>) {
        self.folder = path;
        cx.notify();
    }

    /// Starts the agent picked: what "Start agent" does.
    pub fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone().filter(|_| !self.launching) else {
            return;
        };
        self.launching = true;
        cx.notify();
        let launch = Launch {
            workspace: target.id,
            agent: self.agent,
            cwd: self.folder.to_string(),
        };
        let launching = self.api.launch_agent(launch, cx);
        cx.spawn_in(window, async move |this, cx| {
            let launched = launching.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.launching = false;
                match launched {
                    Ok(launched) => {
                        this.handle.close(window, cx);
                        cx.emit(launched);
                    }
                    Err(err) => failed("Couldn't start the agent", &refusal(err), window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn field(label: &'static str, select: Select, description: Option<String>) -> impl IntoElement {
        Field::new()
            .child(Label::new().child(label))
            .child(select)
            .children(description.map(FieldDescription::new))
    }
}

/// `err`; an agent that can't start as the daemon's message alone, which says what to do
/// when it isn't on the daemon's `PATH`.
fn refusal(err: anyhow::Error) -> anyhow::Error {
    match err.downcast_ref::<ApiError>() {
        Some(api) if api.code.as_deref() == Some("invalid_argv") => {
            anyhow::anyhow!(api.message.clone())
        }
        _ => err,
    }
}

impl Render for NewAgentDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.target.as_ref().map_or("".into(), |t| t.name.clone());
        let agent_select = Select::new("new-agent-agent")
            .trigger(SelectTrigger::new().w_full())
            .content(
                AGENTS
                    .iter()
                    .fold(SelectContent::new(), |c, (agent, label)| {
                        c.item(SelectItem::new(agent.as_str(), *label))
                    }),
            )
            .value(Some(self.agent.as_str()))
            .on_value_change(cx.listener(|this, value: &SharedString, _, cx| {
                if let Ok(agent) = value.parse() {
                    this.set_agent(agent, cx);
                }
            }));
        let folder_select = Select::new("new-agent-folder")
            .trigger(SelectTrigger::new().w_full())
            .content(self.folders.iter().fold(SelectContent::new(), |c, folder| {
                c.item(SelectItem::new(folder.path.clone(), folder.label.clone()))
            }))
            .value(Some(self.folder.clone()))
            .on_value_change(cx.listener(|this, value: &SharedString, _, cx| {
                this.set_folder(value.clone(), cx);
            }));
        let dialog = Dialog::new("new-agent")
            .handle(self.handle.clone())
            .content(
                DialogContent::new()
                    .header(
                        DialogHeader::new()
                            .title(DialogTitle::new("New agent"))
                            .description(DialogDescription::new(format!(
                                "Starts the agent CLI in a terminal in {name}."
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(space::S4)
                            .child(Self::field("Agent", agent_select, None))
                            .child(Self::field(
                                "Folder",
                                folder_select,
                                Some(FOLDER_HINT.to_owned()),
                            )),
                    )
                    .footer(
                        DialogFooter::new()
                            .child(DialogClose::new(
                                Button::new("new-agent-cancel")
                                    .variant(ButtonVariant::Outline)
                                    .label("Cancel"),
                            ))
                            .child(
                                Button::new("new-agent-start")
                                    .label("Start agent")
                                    .disabled(self.launching)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.start(window, cx)),
                                    ),
                            ),
                    ),
            );
        // Enter, while no select has the keyboard, starts too.
        div()
            .key_context(keymap::DIALOG)
            .on_action(cx.listener(|this, _: &ConfirmDialog, window, cx| this.start(window, cx)))
            .child(dialog)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use brrritscold_core_gpui::Root;
    use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::notice;
    use crate::overview::fixtures::id;

    struct Harness {
        api: Arc<FakeApi>,
        dialog: Entity<NewAgentDialog>,
        launched: Rc<RefCell<Vec<Launched>>>,
        _launched: gpui::Subscription,
    }

    /// A dialog opened on workspace 1, its focused pane in `/w/tabs`, another in
    /// `/w/tabs/ts-packages`.
    fn open(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
        cx.update(brrritscold_core_gpui::init);
        cx.update(crate::keymap::init);
        let api = Arc::new(FakeApi::default());
        let mut dialog = None;
        let window = cx.add_window(|window, cx| {
            let fake: Arc<dyn Api> = api.clone();
            let view = cx.new(|_| NewAgentDialog::new(fake));
            dialog = Some(view.clone());
            Root::new(view, window, cx)
        });
        let cx = VisualTestContext::from_window(*window, cx).into_mut();
        let dialog = dialog.unwrap();
        let launched = Rc::new(RefCell::new(Vec::new()));
        let seen = launched.clone();
        let _launched = cx.update(|_, cx| {
            cx.subscribe(&dialog, move |_, l: &Launched, _| {
                seen.borrow_mut().push(*l)
            })
        });
        let target = Target {
            id: id(1),
            name: "tabs".into(),
            folder: "/w/tabs".into(),
        };
        cx.update(|window, cx| {
            dialog.update(cx, |d, cx| {
                d.open(
                    target,
                    folders("/w/tabs", &["/w/tabs/ts-packages".into()], None),
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();
        let harness = Harness {
            api,
            dialog,
            launched,
            _launched,
        };
        (harness, cx)
    }

    fn start(harness: &Harness, cx: &mut VisualTestContext) {
        cx.update(|window, cx| harness.dialog.update(cx, |d, cx| d.start(window, cx)));
        cx.run_until_parked();
    }

    /// Shows the dialog, so it takes the keyboard, then presses `keys`.
    fn press(keys: &str, cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
        cx.simulate_keystrokes(keys);
        cx.run_until_parked();
    }

    mod dialog {
        use super::*;

        #[gpui::test]
        fn should_start_claude_where_a_new_pane_starts(cx: &mut TestAppContext) {
            let (harness, cx) = open(cx);

            start(&harness, cx);

            assert_eq!(
                *harness.api.launched.lock().unwrap(),
                [Launch {
                    workspace: id(1),
                    agent: Agent::Claude,
                    cwd: "/w/tabs".into(),
                }]
            );
            assert_eq!(harness.launched.borrow().len(), 1);
            assert!(!harness.dialog.read_with(cx, |d, _| d.is_open()));
        }

        #[gpui::test]
        fn should_start_on_enter(cx: &mut TestAppContext) {
            let (harness, cx) = open(cx);

            press("enter", cx);

            assert_eq!(harness.api.launched.lock().unwrap().len(), 1);
            assert_eq!(harness.launched.borrow().len(), 1);
            assert!(!harness.dialog.read_with(cx, |d, _| d.is_open()));
        }

        #[gpui::test]
        fn should_close_without_starting_on_escape(cx: &mut TestAppContext) {
            let (harness, cx) = open(cx);

            press("escape", cx);

            assert!(harness.api.launched.lock().unwrap().is_empty());
            assert!(!harness.dialog.read_with(cx, |d, _| d.is_open()));
        }

        #[gpui::test]
        fn should_start_what_was_picked(cx: &mut TestAppContext) {
            let (harness, cx) = open(cx);
            harness.dialog.update(cx, |d, cx| {
                d.set_agent(Agent::Codex, cx);
                d.set_folder("/w/tabs/ts-packages".into(), cx);
            });

            start(&harness, cx);

            let launch = harness.api.launched.lock().unwrap()[0].clone();
            assert_eq!(launch.agent, Agent::Codex);
            assert_eq!(launch.cwd, "/w/tabs/ts-packages");
        }

        #[gpui::test]
        fn should_stay_open_when_the_agent_cant_start(cx: &mut TestAppContext) {
            let (harness, cx) = open(cx);
            harness.api.refuse_next("invalid_argv", "claude: not found");

            start(&harness, cx);

            assert!(harness.launched.borrow().is_empty());
            assert!(harness.dialog.read_with(cx, |d, _| d.is_open()));
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        #[gpui::test]
        fn should_toast_only_the_daemons_message_when_the_agent_cant_start(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = open(cx);
            let hint = "`claude` isn't on agentdeckd's PATH. Install it, or start agentdeckd \
                        from a shell where it's found.";
            harness.api.refuse_next("invalid_argv", hint);

            start(&harness, cx);

            let shown = cx.update(|_, cx| notice::shown(cx));
            let toast = shown.last().expect("a toast");
            assert_eq!(toast.title, "Couldn't start the agent");
            assert_eq!(toast.description.as_deref(), Some(hint));
        }
    }

    mod folders {
        use super::*;

        fn paths(offered: &[Folder]) -> Vec<String> {
            offered.iter().map(|f| f.path.to_string()).collect()
        }

        fn labels(offered: &[Folder]) -> Vec<String> {
            offered.iter().map(|f| f.label.to_string()).collect()
        }

        fn home() -> Option<&'static Path> {
            Some(Path::new("/home/ada"))
        }

        #[test]
        fn should_offer_where_a_new_pane_starts_then_the_other_panes_folders_then_home() {
            let panes = ["/srv/web".into(), "/home/ada/Code/api".into()];

            let offered = folders("/home/ada/Code/agentdeck", &panes, home());

            assert_eq!(
                paths(&offered),
                [
                    "/home/ada/Code/agentdeck",
                    "/srv/web",
                    "/home/ada/Code/api",
                    "/home/ada"
                ]
            );
            assert_eq!(
                labels(&offered),
                ["~/Code/agentdeck", "/srv/web", "~/Code/api", "~"]
            );
        }

        #[test]
        fn should_offer_each_folder_once() {
            let panes = [
                "/srv/web".into(),
                "/home/ada".into(),
                "/srv/web".into(),
                "/srv/web/".into(),
            ];

            let offered = folders("/srv/web", &panes, home());

            assert_eq!(paths(&offered), ["/srv/web", "/home/ada"]);
        }

        #[test]
        fn should_offer_only_home_in_a_workspace_with_no_panes() {
            let offered = folders("/home/ada", &[], home());

            assert_eq!(paths(&offered), ["/home/ada"]);
            assert_eq!(labels(&offered), ["~"]);
        }

        #[test]
        fn should_offer_no_home_when_it_is_unknown() {
            let offered = folders("/srv/web", &[], None);

            assert_eq!(paths(&offered), ["/srv/web"]);
        }

        #[test]
        fn should_say_where_its_folders_come_from() {
            assert_eq!(
                FOLDER_HINT,
                "Where the focused pane is, another pane's folder, or your home folder."
            );
        }
    }

    mod shown_folder {
        use super::*;

        fn home() -> Option<&'static Path> {
            Some(Path::new("/home/ada"))
        }

        #[test]
        fn should_put_a_tilde_for_the_home_folder() {
            assert_eq!(shown_folder("/home/ada/Code/x", home()), "~/Code/x");
            assert_eq!(shown_folder("/home/ada", home()), "~");
        }

        #[test]
        fn should_keep_a_folder_outside_home_as_it_is() {
            assert_eq!(shown_folder("/srv/x", home()), "/srv/x");
        }

        #[test]
        fn should_not_abbreviate_a_sibling_that_shares_homes_prefix() {
            assert_eq!(shown_folder("/home/adam/x", home()), "/home/adam/x");
        }

        #[test]
        fn should_keep_the_folder_when_home_is_unknown() {
            assert_eq!(shown_folder("/home/ada/x", None), "/home/ada/x");
        }
    }
}
