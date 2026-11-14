//! The new agent dialog (spec §6, §11), Penpot "Screens" → "New agent": an agent CLI, the
//! folder it runs in (the workspace root or a repo inside it) and an optional task to link.
//! The description under the task says what the launch prompt will carry.
//!
//! The top bar's "New agent" button asks the window for it with [`NewAgent`]; the task
//! sheet's Start agent opens it on the task's workspace with the task picked. Once the
//! daemon has started the agent and it has a place in a tab, the dialog closes and emits
//! [`Launched`], so the window can show that tab and focus the pane: the agent may be asking
//! about trust (spec §6).

use std::path::Path;
use std::sync::Arc;

use agentdeck_client::ApiError;
use agentdeck_core::{Agent, PaneId, TabId, TaskId, TaskStatus, WorkspaceId};
use brrritscold_core_gpui::{
    Button, ButtonVariant, Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter,
    DialogHandle, DialogHeader, DialogTitle, Field, FieldDescription, Label, Select, SelectContent,
    SelectItem, SelectTrigger,
};
use brrritscold_core_gpui_tokens::primitive::space;
use gpui::{
    Context, EventEmitter, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Window, actions, div,
};

use crate::api::Api;
use crate::overview::Snapshot;
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
    /// The task to link.
    pub task: Option<TaskId>,
}

/// The agents the dialog offers, with their names.
pub const AGENTS: [(Agent, &str); 2] = [(Agent::Claude, "Claude Code"), (Agent::Codex, "Codex")];

/// One folder to run in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// What the picker shows.
    pub label: SharedString,
    /// The absolute dir.
    pub path: SharedString,
}

/// The folders under `root` to offer: the root first, then each repo in `repos` (names of
/// `root`'s child dirs that hold a git repo) in name order.
pub fn folders(root: &str, mut repos: Vec<String>) -> Vec<Folder> {
    repos.sort();
    let root_path = Path::new(root);
    std::iter::once(Folder {
        label: "Workspace root".into(),
        path: root.to_owned().into(),
    })
    .chain(repos.into_iter().map(|name| Folder {
        path: root_path.join(&name).display().to_string().into(),
        label: name.into(),
    }))
    .collect()
}

/// The names of `root`'s child dirs that hold a git repo (a `.git` dir or file); none if it
/// can't be read.
pub fn repos_in(root: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.path().join(".git").exists())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect()
}

/// The task field's description: what the launch prompt carries (spec §6), from whether a
/// task is picked (its title, body and new comments go in) and the workspace's instructions.
pub fn prompt_hint(task: bool, instructions: usize) -> String {
    let instructions = match instructions {
        0 => None,
        1 => Some("1 instruction".to_owned()),
        n => Some(format!("{n} instructions")),
    };
    match (task, instructions) {
        (true, Some(instructions)) => {
            format!("The launch prompt carries the task, its new comments and {instructions}.")
        }
        (true, None) => "The launch prompt carries the task and its new comments.".to_owned(),
        (false, Some(instructions)) => format!("The launch prompt carries {instructions}."),
        (false, None) => {
            "No task and no instructions, so the agent starts without a prompt.".to_owned()
        }
    }
}

/// The workspace the dialog is open on.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// Its id.
    pub id: WorkspaceId,
    /// Its name, for the description.
    pub name: SharedString,
    /// Its dir.
    pub dir: SharedString,
}

/// The dialog, kept by the window.
pub struct NewAgentDialog {
    api: Arc<dyn Api>,
    handle: DialogHandle,
    target: Option<Target>,
    snapshot: Snapshot,
    folders: Vec<Folder>,
    agent: Agent,
    folder: SharedString,
    task: Option<TaskId>,
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
            snapshot: Snapshot::default(),
            folders: Vec::new(),
            agent: Agent::Claude,
            folder: SharedString::default(),
            task: None,
            launching: false,
        }
    }

    /// Opens it on `target`, offering `folders` and the workspace's open tasks from
    /// `snapshot`: Claude Code in the workspace root with no task.
    pub fn open(
        &mut self,
        target: Target,
        folders: Vec<Folder>,
        snapshot: Snapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.folder = target.dir.clone();
        self.folders = folders;
        self.target = Some(target);
        self.snapshot = snapshot;
        self.agent = Agent::Claude;
        self.task = None;
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

    /// The task picked.
    #[cfg(test)]
    pub fn task(&self) -> Option<TaskId> {
        self.task
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

    /// Picks the task, or none.
    pub fn set_task(&mut self, task: Option<TaskId>, cx: &mut Context<Self>) {
        self.task = task;
        cx.notify();
    }

    /// The workspace's tasks that aren't done, and the one picked, in the daemon's order.
    fn tasks(&self) -> Vec<(TaskId, SharedString)> {
        let Some(target) = &self.target else {
            return Vec::new();
        };
        (self.snapshot.tasks.iter())
            .filter(|t| t.workspace_id == Some(target.id))
            .filter(|t| t.status != TaskStatus::Done || Some(t.id) == self.task)
            .map(|t| (t.id, SharedString::from(t.title.clone())))
            .collect()
    }

    /// The workspace's instructions, which every launch prompt there carries.
    fn instructions(&self) -> usize {
        let Some(target) = &self.target else {
            return 0;
        };
        (self.snapshot.instructions.iter())
            .filter(|i| i.workspace_id == target.id)
            .count()
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
            task: self.task,
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
        let task_select = Select::new("new-agent-task")
            .trigger(SelectTrigger::new().w_full())
            .content(self.tasks().into_iter().fold(
                SelectContent::new().item(SelectItem::new("none", "None")),
                |c, (id, title)| c.item(SelectItem::new(id.to_string(), title)),
            ))
            .value(Some(self.task.map_or("none".to_owned(), |t| t.to_string())))
            .on_value_change(cx.listener(|this, value: &SharedString, _, cx| {
                this.set_task(value.parse().ok(), cx);
            }));
        let hint = prompt_hint(self.task.is_some(), self.instructions());
        Dialog::new("new-agent")
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
                                Some("The workspace root or a repo inside it.".to_owned()),
                            ))
                            .child(Self::field("Task (optional)", task_select, Some(hint))),
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
            )
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use agentdeck_core::TaskKind;
    use brrritscold_core_gpui::Root;
    use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::notice;
    use crate::overview::fixtures::{id, instruction, task, workspace};

    struct Harness {
        api: Arc<FakeApi>,
        dialog: Entity<NewAgentDialog>,
        launched: Rc<RefCell<Vec<Launched>>>,
        _launched: gpui::Subscription,
    }

    /// A dialog opened on workspace 1 (`/w/tabs`, with repo `ts-packages`), whose tasks are
    /// 10 (todo) and 11 (done), with an instruction for the workspace.
    fn open(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
        cx.update(brrritscold_core_gpui::init);
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
        let ws = workspace(1, "tabs");
        let snapshot = Snapshot {
            tasks: vec![
                task(10, TaskKind::Plain, Some(ws.id)),
                Task {
                    status: TaskStatus::Done,
                    ..task(11, TaskKind::Plain, Some(ws.id))
                },
            ],
            instructions: vec![instruction(20, ws.id)],
            workspaces: vec![ws],
            ..Snapshot::default()
        };
        let target = Target {
            id: id(1),
            name: "tabs".into(),
            dir: "/w/tabs".into(),
        };
        cx.update(|window, cx| {
            dialog.update(cx, |d, cx| {
                d.open(
                    target,
                    folders("/w/tabs", vec!["ts-packages".into()]),
                    snapshot,
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

    use agentdeck_client::types::Task;

    mod dialog {
        use super::*;

        #[gpui::test]
        fn should_start_claude_in_the_workspace_root_with_no_task(cx: &mut TestAppContext) {
            let (harness, cx) = open(cx);

            start(&harness, cx);

            assert_eq!(
                *harness.api.launched.lock().unwrap(),
                [Launch {
                    workspace: id(1),
                    agent: Agent::Claude,
                    cwd: "/w/tabs".into(),
                    task: None,
                }]
            );
            assert_eq!(harness.launched.borrow().len(), 1);
            assert!(!harness.dialog.read_with(cx, |d, _| d.is_open()));
        }

        #[gpui::test]
        fn should_start_what_was_picked(cx: &mut TestAppContext) {
            let (harness, cx) = open(cx);
            harness.dialog.update(cx, |d, cx| {
                d.set_agent(Agent::Codex, cx);
                d.set_folder("/w/tabs/ts-packages".into(), cx);
                d.set_task(Some(id(10)), cx);
            });

            start(&harness, cx);

            let launch = harness.api.launched.lock().unwrap()[0].clone();
            assert_eq!(launch.agent, Agent::Codex);
            assert_eq!(launch.cwd, "/w/tabs/ts-packages");
            assert_eq!(launch.task, Some(id(10)));
        }

        #[gpui::test]
        fn should_offer_only_tasks_that_are_not_done(cx: &mut TestAppContext) {
            let (harness, cx) = open(cx);

            let tasks: Vec<TaskId> = harness
                .dialog
                .read_with(cx, |d, _| d.tasks().into_iter().map(|(t, _)| t).collect());
            assert_eq!(tasks, [id(10)]);
        }

        #[gpui::test]
        fn should_offer_a_done_task_once_it_is_picked(cx: &mut TestAppContext) {
            let (harness, cx) = open(cx);

            harness
                .dialog
                .update(cx, |d, cx| d.set_task(Some(id(11)), cx));

            let tasks: Vec<TaskId> = harness
                .dialog
                .read_with(cx, |d, _| d.tasks().into_iter().map(|(t, _)| t).collect());
            assert_eq!(tasks, [id(10), id(11)]);
        }

        #[gpui::test]
        fn should_count_the_workspaces_instructions_for_the_prompt(cx: &mut TestAppContext) {
            let (harness, cx) = open(cx);

            assert_eq!(harness.dialog.read_with(cx, |d, _| d.instructions()), 1);
            cx.update(|window, cx| window.draw(cx).clear(cx));
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

        #[test]
        fn should_offer_the_root_first_then_its_repos_by_name() {
            let offered = folders("/w/tabs", vec!["ts-packages".into(), "agentdeck".into()]);

            let paths: Vec<_> = offered.iter().map(|f| f.path.to_string()).collect();
            assert_eq!(
                paths,
                ["/w/tabs", "/w/tabs/agentdeck", "/w/tabs/ts-packages"]
            );
            assert_eq!(offered[0].label, "Workspace root");
            assert_eq!(offered[2].label, "ts-packages");
        }
    }

    mod prompt_hint {
        use super::*;

        #[test]
        fn should_say_the_task_and_how_many_instructions() {
            assert_eq!(
                prompt_hint(true, 2),
                "The launch prompt carries the task, its new comments and 2 instructions."
            );
            assert_eq!(
                prompt_hint(true, 0),
                "The launch prompt carries the task and its new comments."
            );
            assert_eq!(
                prompt_hint(false, 1),
                "The launch prompt carries 1 instruction."
            );
        }

        #[test]
        fn should_say_when_there_is_no_prompt() {
            assert_eq!(
                prompt_hint(false, 0),
                "No task and no instructions, so the agent starts without a prompt."
            );
        }
    }
}
