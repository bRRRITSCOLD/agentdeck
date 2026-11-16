//! Send to agent on a task (spec §9, §11; Penpot "Screens" → "Tasks · row menu · send to
//! agent", "Task sheet · send to agent", "New task · send to"): the live agent panes a task
//! can go to, the submenu that lists them, and the send itself, which links the pane to the
//! task (`PATCH /v1/panes/{id}`) so the daemon tells its agent the task. An Inbox task moves
//! to the pane's workspace first, since a pane links only to a task in its own workspace.
//!
//! Every task row's `…` menu, the task sheet's menu and New task's Send to field use it.

use std::rc::Rc;
use std::sync::Arc;

use agentdeck_client::types::TaskPatch;
use agentdeck_core::{Agent, AgentState, PaneId, TaskId, TaskPlace, TaskStatus, WorkspaceId};
use brrritscold_core_gpui::{DropdownMenuItem, DropdownMenuSubContent, DropdownMenuSubTrigger};
use gpui::{App, SharedString, Window};

use crate::api::Api;
use crate::overview::Snapshot;
use crate::task_sheet::{agent_label, agent_name};
use crate::workspace::failed;

/// What picking a pane in the submenu does.
pub type OnPick = Rc<dyn Fn(TaskTarget, &mut Window, &mut App)>;

/// What the item reads while no agent pane can take the task.
pub const NO_AGENT: &str = "No agent is running here";

/// A live agent pane a task can be sent to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskTarget {
    /// The pane.
    pub pane: PaneId,
    /// Its workspace, which the task moves to from the Inbox.
    pub workspace: WorkspaceId,
    /// The agent it runs, or hosts.
    pub agent: Agent,
    /// What its item reads: `✳ Claude Code · tabs-a11y / agents`.
    pub label: SharedString,
}

/// Where `task` can be sent: the live agent panes in its workspace, or in every workspace for
/// an Inbox task, leaving out those already linked to it. `None` for a done task, which is
/// never sent, and for one `snapshot` doesn't hold.
pub fn targets(snapshot: &Snapshot, task: TaskId) -> Option<Vec<TaskTarget>> {
    let held = snapshot.tasks.iter().find(|t| t.id == task)?;
    if held.status == TaskStatus::Done {
        return None;
    }
    Some(
        targets_in(snapshot, held.workspace_id)
            .into_iter()
            .filter(|target| !linked_to(snapshot, target.pane, task))
            .collect(),
    )
}

/// The live agent panes in `workspace`, or in every workspace for the Inbox (`None`), in the
/// snapshot's order: shell panes hosting an agent included, exited panes left out.
pub fn targets_in(snapshot: &Snapshot, workspace: Option<WorkspaceId>) -> Vec<TaskTarget> {
    (snapshot.panes.iter())
        .filter(|p| p.state != AgentState::Exited)
        .filter(|p| workspace.is_none_or(|w| p.workspace == w))
        .filter_map(|p| {
            let agent = p.agent?;
            let name = (snapshot.workspaces.iter())
                .find(|w| w.id == p.workspace)
                .map_or("", |w| w.name.as_str());
            Some(TaskTarget {
                pane: p.pane,
                workspace: p.workspace,
                agent,
                label: format!("{} · {name} / {}", agent_label(agent), p.tab).into(),
            })
        })
        .collect()
}

/// Whether `pane` is linked to `task` already.
fn linked_to(snapshot: &Snapshot, pane: PaneId, task: TaskId) -> bool {
    (snapshot.panes.iter()).any(|p| p.pane == pane && p.task == Some(task))
}

/// The task a send is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sending {
    /// The task.
    pub task: TaskId,
    /// Its title, for the toast on a failure.
    pub title: SharedString,
    /// Whether it is in the Inbox, so it moves to the pane's workspace first.
    pub inbox: bool,
}

/// Sends `sending`'s task to `target`: moves an Inbox task to the pane's workspace, then links
/// the pane to it, which tells its agent the task (spec §9). A failure toasts "Couldn't send
/// `title` to `Agent`" with the daemon's reason.
pub fn send(
    api: Arc<dyn Api>,
    sending: Sending,
    target: TaskTarget,
    window: &mut Window,
    cx: &mut App,
) {
    let Sending { task, title, inbox } = sending;
    let moving = inbox.then(|| {
        let patch = TaskPatch {
            workspace_id: Some(TaskPlace::Workspace(target.workspace)),
            ..TaskPatch::default()
        };
        api.edit_task(task, patch, cx)
    });
    window
        .spawn(cx, async move |cx| {
            let sent = async {
                if let Some(moving) = moving {
                    moving.await?;
                }
                cx.update(|_, cx| api.link_task(target.pane, Some(task), cx))?
                    .await
            }
            .await;
            if let Err(err) = sent {
                let why = format!("Couldn't send {title} to {}", agent_name(target.agent));
                let _ = cx.update(|window, cx| failed(why, &err, window, cx));
            }
        })
        .detach();
}

/// The Send to agent submenu over `targets`: an item per pane, or one disabled
/// [`NO_AGENT`] with none. Picking one calls `on_pick` with it.
pub fn submenu(
    targets: Vec<TaskTarget>,
    on_pick: OnPick,
) -> (DropdownMenuSubTrigger, DropdownMenuSubContent) {
    let content = if targets.is_empty() {
        DropdownMenuSubContent::new().item(DropdownMenuItem::new(NO_AGENT).disabled(true))
    } else {
        targets
            .into_iter()
            .fold(DropdownMenuSubContent::new(), |content, target| {
                let on_pick = on_pick.clone();
                let label = target.label.clone();
                content.item(
                    DropdownMenuItem::new(label)
                        .on_click(move |_, window, cx| on_pick(target.clone(), window, cx)),
                )
            })
    };
    (DropdownMenuSubTrigger::new("Send to agent"), content)
}

#[cfg(test)]
mod tests {
    use agentdeck_client::types::{Task, Workspace};
    use agentdeck_core::TaskKind;
    use brrritscold_core_gpui::Root;
    use gpui::{AppContext as _, TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::overview::PaneState;
    use crate::overview::fixtures::{id, pane, task, workspace};

    fn agent(n: u32, ws: &Workspace, agent: Agent, tab: &str) -> PaneState {
        PaneState {
            agent: Some(agent),
            tab: tab.into(),
            ..pane(n, ws.id, AgentState::Idle)
        }
    }

    /// `tabs-a11y` (1) holding task 10, `scratch` (2), and task 11 in the Inbox; Claude Code in
    /// `tabs-a11y`'s agents tab, Codex hosted in `scratch`'s shell tab, a bare shell, and an
    /// exited Claude Code.
    fn snapshot() -> Snapshot {
        let (a, b) = (workspace(1, "tabs-a11y"), workspace(2, "scratch"));
        Snapshot {
            panes: vec![
                agent(30, &a, Agent::Claude, "agents"),
                pane(31, a.id, AgentState::Idle),
                PaneState {
                    state: AgentState::Exited,
                    ..agent(32, &a, Agent::Claude, "old")
                },
                agent(33, &b, Agent::Codex, "shell"),
            ],
            tasks: vec![
                Task {
                    title: "Tabs: arrow-key navigation".into(),
                    ..task(10, TaskKind::Plain, Some(a.id))
                },
                Task {
                    title: "Write the codex state rules".into(),
                    ..task(11, TaskKind::Plain, None)
                },
            ],
            workspaces: vec![a, b],
            ..Snapshot::default()
        }
    }

    fn labels(targets: Option<Vec<TaskTarget>>) -> Option<Vec<(PaneId, String)>> {
        targets.map(|targets| {
            (targets.into_iter())
                .map(|t| (t.pane, t.label.to_string()))
                .collect()
        })
    }

    mod targets {
        use super::*;

        #[test]
        fn should_offer_the_live_agents_in_the_tasks_workspace_by_workspace_and_tab() {
            assert_eq!(
                labels(targets(&snapshot(), id(10))),
                Some(vec![(id(30), "✳ Claude Code · tabs-a11y / agents".into())])
            );
        }

        #[test]
        fn should_offer_the_live_agents_in_every_workspace_for_an_inbox_task() {
            assert_eq!(
                labels(targets(&snapshot(), id(11))),
                Some(vec![
                    (id(30), "✳ Claude Code · tabs-a11y / agents".into()),
                    (id(33), "Codex · scratch / shell".into()),
                ])
            );
        }

        #[test]
        fn should_leave_out_a_pane_already_linked_to_the_task_but_offer_one_on_another() {
            let mut snapshot = snapshot();
            snapshot.panes[0].task = Some(id(10));
            snapshot.panes[3].task = Some(id(10));

            assert_eq!(
                labels(targets(&snapshot, id(11))),
                Some(vec![
                    (id(30), "✳ Claude Code · tabs-a11y / agents".into()),
                    (id(33), "Codex · scratch / shell".into()),
                ])
            );
            assert_eq!(labels(targets(&snapshot, id(10))), Some(vec![]));
        }

        #[test]
        fn should_offer_nothing_for_a_done_task() {
            let mut snapshot = snapshot();
            snapshot.tasks[0].status = TaskStatus::Done;

            assert_eq!(targets(&snapshot, id(10)), None);
        }

        #[test]
        fn should_be_none_for_a_task_it_doesnt_hold() {
            assert_eq!(targets(&snapshot(), id(99)), None);
        }
    }

    mod send {
        use super::*;

        fn window(cx: &mut TestAppContext) -> (Arc<FakeApi>, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            let api = Arc::new(FakeApi::with(snapshot()));
            let window = cx.add_window(|window, cx| {
                let view = cx.new(|_| gpui::Empty);
                Root::new(view, window, cx)
            });
            (api, VisualTestContext::from_window(*window, cx).into_mut())
        }

        fn send_to(
            api: &Arc<FakeApi>,
            task: u32,
            pane: u32,
            inbox: bool,
            cx: &mut VisualTestContext,
        ) {
            let target = (targets_in(&snapshot(), None).into_iter())
                .find(|t| t.pane == id(pane))
                .expect("a live agent pane");
            let sending = Sending {
                task: id(task),
                title: "Tabs: arrow-key navigation".into(),
                inbox,
            };
            let api: Arc<dyn Api> = api.clone();
            cx.update(|window, cx| send(api, sending, target, window, cx));
            cx.run_until_parked();
        }

        #[gpui::test]
        fn should_link_the_pane_to_the_task(cx: &mut TestAppContext) {
            let (api, cx) = window(cx);

            send_to(&api, 10, 30, false, cx);

            assert_eq!(*api.linked.lock().unwrap(), [(id(30), Some(id(10)))]);
            assert_eq!(*api.edited_tasks.lock().unwrap(), []);
        }

        #[gpui::test]
        fn should_move_an_inbox_task_to_the_panes_workspace_then_link_it(cx: &mut TestAppContext) {
            let (api, cx) = window(cx);

            send_to(&api, 11, 33, true, cx);

            let moved = TaskPatch {
                workspace_id: Some(TaskPlace::Workspace(id(2))),
                ..TaskPatch::default()
            };
            assert_eq!(*api.edited_tasks.lock().unwrap(), [(id(11), moved)]);
            assert_eq!(*api.linked.lock().unwrap(), [(id(33), Some(id(11)))]);
        }

        #[gpui::test]
        fn should_toast_the_daemons_reason_when_it_refuses(cx: &mut TestAppContext) {
            let (api, cx) = window(cx);
            api.refuse_next("invalid_task", "Task is done");

            send_to(&api, 10, 30, false, cx);

            let shown = cx.update(|_, cx| crate::notice::shown(cx));
            let toast = shown.last().expect("a toast");
            assert_eq!(
                toast.title.as_ref(),
                "Couldn't send Tabs: arrow-key navigation to Claude Code"
            );
            assert!(
                toast
                    .description
                    .as_deref()
                    .is_some_and(|why| why.starts_with("Task is done"))
            );
        }

        #[gpui::test]
        fn should_link_nothing_when_the_move_fails(cx: &mut TestAppContext) {
            let (api, cx) = window(cx);
            api.fail_next("no such workspace");

            send_to(&api, 11, 33, true, cx);

            assert_eq!(*api.linked.lock().unwrap(), []);
        }
    }
}
