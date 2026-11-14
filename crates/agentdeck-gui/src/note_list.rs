//! The notes the right panel lists (spec §9, §11), from the sidebar's [`Snapshot`]: those on
//! a workspace and on its tasks, newest first, each with the line Penpot's `NoteItem` shows
//! under it, and the agent panes each can be sent to.

use agentdeck_client::types::Note;
use agentdeck_core::{AgentState, NoteId, PaneId, WorkspaceId};
use chrono::{DateTime, Utc};
use gpui::SharedString;

use crate::new_agent::AGENTS;
use crate::overview::{Snapshot, ago};

/// One note, as the panel shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteRow {
    /// The note.
    pub id: NoteId,
    /// Its text.
    pub body: SharedString,
    /// What it is on, then who read it or how old it is: `workspace · 2 h ago`,
    /// `task · read by claude`.
    pub meta: SharedString,
    /// Whether no one has read it: the row's `unread` badge.
    pub unread: bool,
}

/// `workspace`'s notes and its tasks' notes, newest first, at `now`.
pub fn notes(snapshot: &Snapshot, workspace: WorkspaceId, now: DateTime<Utc>) -> Vec<NoteRow> {
    let on_its_task = |note: &Note| {
        note.task_id.is_some_and(|task| {
            (snapshot.tasks.iter()).any(|t| t.id == task && t.workspace_id == Some(workspace))
        })
    };
    let mut picked: Vec<&Note> = (snapshot.notes.iter())
        .filter(|note| note.workspace_id == Some(workspace) || on_its_task(note))
        .collect();
    // RFC 3339 UTC stamps sort as text.
    picked.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    picked
        .into_iter()
        .map(|note| NoteRow {
            id: note.id,
            body: note.body.clone().into(),
            meta: meta(snapshot, note, now).into(),
            unread: note.read_at.is_none(),
        })
        .collect()
}

/// A pane a note can be sent to, from its "Send to agent" menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendTarget {
    /// The pane.
    pub pane: PaneId,
    /// What its item reads: the agent, ` · `, then the linked task's title, else the tab's.
    pub label: SharedString,
}

/// The live agent panes in `note`'s workspace (its own, or its task's), in the snapshot's
/// order: what its "Send to agent" menu offers.
pub fn send_targets(snapshot: &Snapshot, note: NoteId) -> Vec<SendTarget> {
    let Some(note) = snapshot.notes.iter().find(|n| n.id == note) else {
        return Vec::new();
    };
    let task_title = |task| {
        (snapshot.tasks.iter())
            .find(|t| t.id == task)
            .map(|t| t.title.as_str())
    };
    let workspace = note.workspace_id.or_else(|| {
        let task = note.task_id?;
        (snapshot.tasks.iter()).find(|t| t.id == task)?.workspace_id
    });
    let Some(workspace) = workspace else {
        return Vec::new();
    };
    (snapshot.panes.iter())
        .filter(|p| p.workspace == workspace && p.state != AgentState::Exited)
        .filter_map(|p| {
            let agent = AGENTS.iter().find(|(a, _)| Some(*a) == p.agent)?.1;
            let on = p.task.and_then(task_title).unwrap_or(&p.tab);
            Some(SendTarget {
                pane: p.pane,
                label: format!("{agent} · {on}").into(),
            })
        })
        .collect()
}

/// `workspace · 2 h ago`, `task · read by claude` or `workspace · read`.
fn meta(snapshot: &Snapshot, note: &Note, now: DateTime<Utc>) -> String {
    let on = if note.task_id.is_some() {
        "task"
    } else {
        "workspace"
    };
    let reader = note.read_by_pane.and_then(|pane| {
        (snapshot.panes.iter())
            .find(|p| p.pane == pane)
            .map(|p| p.agent.map_or("shell", |agent| agent.as_str()))
    });
    let when = match (note.read_at.as_ref(), reader) {
        (Some(_), Some(reader)) => format!("read by {reader}"),
        (Some(_), None) => "read".to_owned(),
        (None, _) => match note.created_at.parse::<DateTime<Utc>>() {
            Ok(created) => ago(now - created),
            Err(_) => "unread".to_owned(),
        },
    };
    format!("{on} · {when}")
}

#[cfg(test)]
mod tests {
    use agentdeck_client::types::Task;
    use agentdeck_core::{Agent, TaskKind};

    use super::*;
    use crate::overview::PaneState;
    use crate::overview::fixtures::{id, pane, task, task_note, workspace, workspace_note};

    const NOW: &str = "2026-11-13T12:00:00Z";

    fn now() -> DateTime<Utc> {
        NOW.parse().unwrap()
    }

    fn at(note: Note, created_at: &str) -> Note {
        Note {
            created_at: created_at.into(),
            ..note
        }
    }

    #[test]
    fn should_list_the_workspaces_and_its_tasks_notes_newest_first() {
        let (a, b) = (workspace(1, "a"), workspace(2, "b"));
        let (on_a, on_b) = (
            task(10, TaskKind::Plain, Some(a.id)),
            task(11, TaskKind::Plain, Some(b.id)),
        );
        let snapshot = Snapshot {
            notes: vec![
                at(workspace_note(20, a.id), "2026-11-13T09:00:00Z"),
                at(task_note(21, &on_a), "2026-11-13T11:00:00Z"),
                workspace_note(22, b.id),
                task_note(23, &on_b),
            ],
            tasks: vec![on_a, on_b],
            workspaces: vec![a.clone(), b],
            ..Snapshot::default()
        };

        let ids: Vec<NoteId> = notes(&snapshot, a.id, now())
            .into_iter()
            .map(|n| n.id)
            .collect();
        assert_eq!(ids, [id(21), id(20)]);
    }

    #[test]
    fn should_say_what_an_unread_note_is_on_and_how_old_it_is() {
        let a = workspace(1, "a");
        let snapshot = Snapshot {
            notes: vec![at(workspace_note(20, a.id), "2026-11-13T10:00:00Z")],
            workspaces: vec![a.clone()],
            ..Snapshot::default()
        };

        let row = &notes(&snapshot, a.id, now())[0];
        assert_eq!(row.meta, "workspace · 2 h ago");
        assert!(row.unread);
    }

    #[test]
    fn should_say_which_agent_read_a_note() {
        let a = workspace(1, "a");
        let on_a = task(10, TaskKind::Plain, Some(a.id));
        let reader = PaneState {
            agent: Some(Agent::Claude),
            ..pane(30, a.id, AgentState::Working)
        };
        let read = Note {
            read_at: Some("2026-11-13T11:00:00Z".into()),
            read_by_pane: Some(reader.pane),
            ..task_note(20, &on_a)
        };
        let snapshot = Snapshot {
            notes: vec![read],
            panes: vec![reader],
            tasks: vec![on_a],
            workspaces: vec![a.clone()],
            ..Snapshot::default()
        };

        let row = &notes(&snapshot, a.id, now())[0];
        assert_eq!(row.meta, "task · read by claude");
        assert!(!row.unread);
    }

    #[test]
    fn should_say_read_when_no_pane_it_knows_read_it() {
        let a = workspace(1, "a");
        let read = Note {
            read_at: Some("2026-11-13T11:00:00Z".into()),
            ..workspace_note(20, a.id)
        };
        let snapshot = Snapshot {
            notes: vec![read],
            workspaces: vec![a.clone()],
            ..Snapshot::default()
        };

        assert_eq!(notes(&snapshot, a.id, now())[0].meta, "workspace · read");
    }

    mod send_targets {
        use super::*;

        fn agent(n: u32, ws: WorkspaceId, agent: Agent, task: Option<u32>) -> PaneState {
            PaneState {
                agent: Some(agent),
                task: task.map(id),
                tab: format!("tab {n}"),
                ..pane(n, ws, AgentState::Working)
            }
        }

        fn labels(snapshot: &Snapshot, note: u32) -> Vec<(PaneId, String)> {
            (send_targets(snapshot, id(note)).into_iter())
                .map(|t| (t.pane, t.label.to_string()))
                .collect()
        }

        #[test]
        fn should_offer_each_live_agent_in_the_notes_workspace_by_task_or_tab() {
            let (a, b) = (workspace(1, "a"), workspace(2, "b"));
            let linked = Task {
                title: "Tabs: arrow-key navigation".into(),
                ..task(10, TaskKind::Plain, Some(a.id))
            };
            let exited = PaneState {
                state: AgentState::Exited,
                ..agent(33, a.id, Agent::Claude, None)
            };
            let snapshot = Snapshot {
                notes: vec![workspace_note(20, a.id)],
                panes: vec![
                    agent(30, a.id, Agent::Claude, Some(10)),
                    pane(31, a.id, AgentState::Idle),
                    agent(32, a.id, Agent::Codex, None),
                    exited,
                    agent(34, b.id, Agent::Claude, None),
                ],
                tasks: vec![linked],
                workspaces: vec![a, b],
                ..Snapshot::default()
            };

            assert_eq!(
                labels(&snapshot, 20),
                [
                    (
                        id(30),
                        "Claude Code · Tabs: arrow-key navigation".to_owned()
                    ),
                    (id(32), "Codex · tab 32".to_owned()),
                ]
            );
        }

        #[test]
        fn should_look_in_the_tasks_workspace_for_a_task_note() {
            let (a, b) = (workspace(1, "a"), workspace(2, "b"));
            let on_b = task(10, TaskKind::Plain, Some(b.id));
            let snapshot = Snapshot {
                notes: vec![task_note(20, &on_b)],
                panes: vec![
                    agent(30, a.id, Agent::Claude, None),
                    agent(31, b.id, Agent::Claude, None),
                ],
                tasks: vec![on_b],
                workspaces: vec![a, b],
                ..Snapshot::default()
            };

            assert_eq!(
                labels(&snapshot, 20),
                [(id(31), "Claude Code · tab 31".to_owned())]
            );
        }

        #[test]
        fn should_offer_none_for_an_inbox_tasks_note() {
            let a = workspace(1, "a");
            let inbox = task(10, TaskKind::Plain, None);
            let snapshot = Snapshot {
                notes: vec![task_note(20, &inbox)],
                panes: vec![agent(30, a.id, Agent::Claude, None)],
                tasks: vec![inbox],
                workspaces: vec![a],
                ..Snapshot::default()
            };

            assert_eq!(labels(&snapshot, 20), []);
        }
    }
}
