//! The notes the right panel lists (spec §9, §11), from the sidebar's [`Snapshot`]: those on
//! a workspace and on its tasks, newest first, each with the line Penpot's `NoteItem` shows
//! under it.

use agentdeck_client::types::Note;
use agentdeck_core::{NoteId, WorkspaceId};
use chrono::{DateTime, Utc};
use gpui::SharedString;

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
    use agentdeck_core::{Agent, AgentState, TaskKind};

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
}
