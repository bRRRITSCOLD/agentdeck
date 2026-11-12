//! Delivering notes (spec §9): a note on a task with a live agent pane is typed into that
//! pane once the agent waits at its prompt, never into a dialog. Delivery marks the note
//! read by the pane.

use agentdeck_core::ws::Entity;
use agentdeck_core::{AgentState, NoteId, PaneId};

use crate::db::{self, Db};
use crate::events::Events;
use crate::model::Note;
use crate::terminal::Terminals;

/// Hands a new `note` to its task's newest live agent pane, if it is on a task that has
/// one. It is typed, with Enter, once the agent is idle or done, and marked read then; it
/// stays unread if the pane exits first.
pub async fn deliver_to_task_pane(db: &Db, terminals: &Terminals, events: &Events, note: &Note) {
    let Some(task) = note.task_id else {
        return;
    };
    let panes = match db.call(move |c| db::panes::agents_on_task(c, task)).await {
        Ok(panes) => panes,
        Err(err) => {
            tracing::warn!("finding task {task}'s panes for note {}: {err}", note.id);
            return;
        }
    };
    let live = panes.into_iter().find_map(|id| {
        terminals
            .get(id)
            .filter(|pane| pane.status().state != AgentState::Exited)
    });
    let Some(pane) = live else {
        return;
    };
    let text = format!("{}\r", note.body.trim_end());
    let Ok(typed) = pane.deliver(text.as_bytes()) else {
        return;
    };
    let (db, events, note, pane) = (db.clone(), events.clone(), note.id, pane.id());
    tokio::spawn(async move {
        if typed.await.is_ok() {
            mark_read(&db, &events, vec![note], pane).await;
        }
    });
}

/// Marks `notes` read by `pane`, which they were delivered to.
pub async fn mark_read(db: &Db, events: &Events, notes: Vec<NoteId>, pane: PaneId) {
    if notes.is_empty() {
        return;
    }
    let marked = db
        .call(move |c| {
            let mut marked = Vec::new();
            for id in notes {
                if db::notes::mark_read(c, id, pane)? {
                    marked.push(id);
                }
            }
            Ok(marked)
        })
        .await;
    match marked {
        Ok(marked) => {
            for id in marked {
                events.changed(Entity::Note, id);
            }
        }
        Err(err) => tracing::warn!("marking notes read by pane {pane}: {err}"),
    }
}
