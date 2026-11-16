//! The Link to task dialog (spec §9, §11), Penpot "Screens" → "Link to task": links the agent
//! in a running pane to one of its workspace's tasks that aren't done, or unlinks it.
//!
//! A pane header's Link to task asks the window for it, through the workspace view's
//! [`LinkPane`](crate::workspace::LinkPane). Once linked, the daemon types the task, then its
//! comments no agent has had yet, into the pane when the agent is idle. A failed link keeps
//! the dialog open and toasts why.

use std::sync::Arc;

use agentdeck_core::{Agent, PaneId, TaskId, TaskStatus, WorkspaceId};
use brrritscold_core_gpui::{
    Button, ButtonVariant, Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter,
    DialogHandle, DialogHeader, DialogTitle, Field, FieldDescription, Label, Select, SelectContent,
    SelectItem, SelectTrigger,
};
use gpui::{Context, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Window};

use crate::api::Api;
use crate::notice::{self, Notice};
use crate::overview::Snapshot;
use crate::task_sheet::agent_name;

/// The pane the dialog is open on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The pane.
    pub pane: PaneId,
    /// The agent running in it.
    pub agent: Agent,
    /// The title of the tab it is in.
    pub tab: SharedString,
    /// Its workspace.
    pub workspace: WorkspaceId,
    /// The workspace's name.
    pub workspace_name: SharedString,
}

/// The dialog's title: `Link Claude Code to a task`.
pub fn title(agent: Agent) -> String {
    format!("Link {} to a task", agent_name(agent))
}

/// The dialog's description: which agent it links.
pub fn description(tab: &str, workspace: &str) -> String {
    format!("The agent in the {tab} tab of {workspace}.")
}

/// The Task field's description: what linking hands the agent.
pub fn hint(agent: Agent) -> String {
    format!(
        "Comments on it that no agent has had yet are typed in once {} is idle. None unlinks it.",
        agent_name(agent)
    )
}

/// The dialog, kept by the window.
pub struct LinkTaskDialog {
    api: Arc<dyn Api>,
    handle: DialogHandle,
    target: Option<Target>,
    snapshot: Snapshot,
    task: Option<TaskId>,
    /// Whether a link is out; another waits for it.
    linking: bool,
}

impl LinkTaskDialog {
    /// A closed dialog that links panes through `api`.
    pub fn new(api: Arc<dyn Api>) -> Self {
        LinkTaskDialog {
            api,
            handle: DialogHandle::new(false),
            target: None,
            snapshot: Snapshot::default(),
            task: None,
            linking: false,
        }
    }

    /// Opens it on `target`, offering the workspace's tasks from `snapshot` that aren't done,
    /// with `task`, the pane's link now, picked.
    pub fn open(
        &mut self,
        target: Target,
        task: Option<TaskId>,
        snapshot: Snapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.target = Some(target);
        self.task = task;
        self.snapshot = snapshot;
        self.handle.open(window, cx);
        cx.notify();
    }

    /// Whether it is open.
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.handle.is_open()
    }

    /// The pane it's open on.
    #[cfg(test)]
    pub fn pane(&self) -> Option<PaneId> {
        self.target.as_ref().map(|target| target.pane)
    }

    /// The name of the workspace it's open on.
    #[cfg(test)]
    pub fn workspace_name(&self) -> Option<SharedString> {
        self.target
            .as_ref()
            .map(|target| target.workspace_name.clone())
    }

    /// The task picked.
    #[cfg(test)]
    pub fn task(&self) -> Option<TaskId> {
        self.task
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
            .filter(|t| t.workspace_id == Some(target.workspace))
            .filter(|t| t.status != TaskStatus::Done || Some(t.id) == self.task)
            .map(|t| (t.id, SharedString::from(t.title.clone())))
            .collect()
    }

    /// Links the pane to the task picked, or unlinks it: what "Link" does.
    pub fn link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone().filter(|_| !self.linking) else {
            return;
        };
        self.linking = true;
        cx.notify();
        let linking = self.api.link_task(target.pane, self.task, cx);
        cx.spawn_in(window, async move |this, cx| {
            let linked = linking.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.linking = false;
                match linked {
                    Ok(()) => this.handle.close(window, cx),
                    Err(err) => {
                        let title = format!("Couldn't link {}", agent_name(target.agent));
                        Notice::error(title, notice::message(&err)).show(window, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for LinkTaskDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (heading, about, field_hint) = match &self.target {
            Some(target) => (
                title(target.agent),
                description(&target.tab, &target.workspace_name),
                hint(target.agent),
            ),
            None => Default::default(),
        };
        let task_select = Select::new("link-task-task")
            .trigger(SelectTrigger::new().w_full())
            .content(self.tasks().into_iter().fold(
                SelectContent::new().item(SelectItem::new("none", "None")),
                |c, (id, title)| c.item(SelectItem::new(id.to_string(), title)),
            ))
            .value(Some(self.task.map_or("none".to_owned(), |t| t.to_string())))
            .on_value_change(cx.listener(|this, value: &SharedString, _, cx| {
                this.set_task(value.parse().ok(), cx);
            }));
        Dialog::new("link-task")
            .handle(self.handle.clone())
            .content(
                DialogContent::new()
                    .header(
                        DialogHeader::new()
                            .title(DialogTitle::new(heading))
                            .description(DialogDescription::new(about)),
                    )
                    .child(
                        Field::new()
                            .child(Label::new().child("Task"))
                            .child(task_select)
                            .child(FieldDescription::new(field_hint)),
                    )
                    .footer(
                        DialogFooter::new()
                            .child(DialogClose::new(
                                Button::new("link-task-cancel")
                                    .variant(ButtonVariant::Outline)
                                    .label("Cancel"),
                            ))
                            .child(
                                Button::new("link-task-link")
                                    .label("Link")
                                    .disabled(self.linking)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.link(window, cx)),
                                    ),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use agentdeck_client::types::Task;
    use agentdeck_core::TaskKind;
    use brrritscold_core_gpui::Root;
    use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::overview::fixtures::{id, task, workspace};

    struct Harness {
        api: Arc<FakeApi>,
        dialog: Entity<LinkTaskDialog>,
    }

    /// A dialog opened on pane 7, Claude Code in the `agents` tab of workspace 1 `tabs`,
    /// linked to `linked`. Workspace 1's tasks are 10 (todo) and 11 (done); workspace 2's is 12.
    fn open(linked: Option<TaskId>, cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
        cx.update(brrritscold_core_gpui::init);
        let api = Arc::new(FakeApi::default());
        let mut dialog = None;
        let window = cx.add_window(|window, cx| {
            let fake: Arc<dyn Api> = api.clone();
            let view = cx.new(|_| LinkTaskDialog::new(fake));
            dialog = Some(view.clone());
            Root::new(view, window, cx)
        });
        let cx = VisualTestContext::from_window(*window, cx).into_mut();
        let dialog = dialog.unwrap();
        let ws = workspace(1, "tabs");
        let snapshot = Snapshot {
            tasks: vec![
                task(10, TaskKind::Plain, Some(ws.id)),
                Task {
                    status: TaskStatus::Done,
                    ..task(11, TaskKind::Plain, Some(ws.id))
                },
                task(12, TaskKind::Plain, Some(id(2))),
            ],
            workspaces: vec![ws],
            ..Snapshot::default()
        };
        let target = Target {
            pane: id(7),
            agent: Agent::Claude,
            tab: "agents".into(),
            workspace: id(1),
            workspace_name: "tabs".into(),
        };
        cx.update(|window, cx| {
            dialog.update(cx, |d, cx| d.open(target, linked, snapshot, window, cx));
        });
        cx.run_until_parked();
        (Harness { api, dialog }, cx)
    }

    fn link(harness: &Harness, cx: &mut VisualTestContext) {
        cx.update(|window, cx| harness.dialog.update(cx, |d, cx| d.link(window, cx)));
        cx.run_until_parked();
    }

    fn offered(harness: &Harness, cx: &mut VisualTestContext) -> Vec<TaskId> {
        harness
            .dialog
            .read_with(cx, |d, _| d.tasks().into_iter().map(|(t, _)| t).collect())
    }

    mod dialog {
        use super::*;

        #[gpui::test]
        fn should_preset_the_panes_current_link(cx: &mut TestAppContext) {
            let (harness, cx) = open(Some(id(10)), cx);

            assert!(harness.dialog.read_with(cx, |d, _| d.is_open()));
            assert_eq!(harness.dialog.read_with(cx, |d, _| d.pane()), Some(id(7)));
            assert_eq!(harness.dialog.read_with(cx, |d, _| d.task()), Some(id(10)));
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        #[gpui::test]
        fn should_offer_only_the_workspaces_tasks_that_are_not_done(cx: &mut TestAppContext) {
            let (harness, cx) = open(None, cx);

            assert_eq!(offered(&harness, cx), [id(10)]);
        }

        #[gpui::test]
        fn should_link_the_pane_to_the_task_picked_and_close(cx: &mut TestAppContext) {
            let (harness, cx) = open(None, cx);
            harness
                .dialog
                .update(cx, |d, cx| d.set_task(Some(id(10)), cx));

            link(&harness, cx);

            assert_eq!(*harness.api.linked.lock().unwrap(), [(id(7), Some(id(10)))]);
            assert!(!harness.dialog.read_with(cx, |d, _| d.is_open()));
        }

        #[gpui::test]
        fn should_unlink_the_pane_when_none_is_picked(cx: &mut TestAppContext) {
            let (harness, cx) = open(Some(id(10)), cx);
            harness.dialog.update(cx, |d, cx| d.set_task(None, cx));

            link(&harness, cx);

            assert_eq!(*harness.api.linked.lock().unwrap(), [(id(7), None)]);
        }

        #[gpui::test]
        fn should_stay_open_and_toast_why_when_the_link_fails(cx: &mut TestAppContext) {
            let (harness, cx) = open(None, cx);
            harness
                .dialog
                .update(cx, |d, cx| d.set_task(Some(id(10)), cx));
            harness.api.refuse_next("invalid_task", "task is done");

            link(&harness, cx);

            assert!(harness.dialog.read_with(cx, |d, _| d.is_open()));
            let shown = cx.update(|_, cx| notice::shown(cx));
            let toast = shown.last().expect("a toast");
            assert_eq!(toast.title, "Couldn't link Claude Code");
            assert_eq!(toast.description.as_deref(), Some("task is done"));
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }

    mod copy {
        use super::*;

        #[test]
        fn should_name_the_agent_in_the_title_and_hint() {
            assert_eq!(title(Agent::Codex), "Link Codex to a task");
            assert_eq!(
                hint(Agent::Claude),
                "Comments on it that no agent has had yet are typed in once Claude Code is \
                 idle. None unlinks it."
            );
        }

        #[test]
        fn should_say_which_tab_and_workspace_the_agent_is_in() {
            assert_eq!(
                description("agents", "tabs"),
                "The agent in the agents tab of tabs."
            );
        }
    }
}
