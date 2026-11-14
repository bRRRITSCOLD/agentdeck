//! Delivering the user's task comments (spec §9): a comment on a task with a live agent pane
//! is typed into that pane once the agent waits at its prompt, never into a dialog.
//! Delivery records the time and the pane. An agent's comments are never delivered.

use agentdeck_core::ws::Entity;
use agentdeck_core::{AgentState, CommentAuthor, CommentId, PaneId};

use crate::db::{self, Db};
use crate::events::Events;
use crate::model::Comment;
use crate::terminal::Terminals;

/// Hands a new user `comment` to its task's newest live agent pane, if the task has one. It
/// is typed, with Enter, once the agent is idle or done, and marked delivered then; it stays
/// undelivered if the pane exits first. An agent's comment is left alone.
pub async fn deliver_to_task_pane(
    db: &Db,
    terminals: &Terminals,
    events: &Events,
    comment: &Comment,
) {
    if comment.author != CommentAuthor::User {
        return;
    }
    let task = comment.task_id;
    let panes = match db.call(move |c| db::panes::agents_on_task(c, task)).await {
        Ok(panes) => panes,
        Err(err) => {
            tracing::warn!(
                "finding task {task}'s panes for comment {}: {err}",
                comment.id
            );
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
    let text = format!("{}\r", comment.body.trim_end());
    let Ok(typed) = pane.deliver(text.as_bytes()) else {
        return;
    };
    let (db, events, comment, pane) = (db.clone(), events.clone(), comment.id, pane.id());
    tokio::spawn(async move {
        if typed.await.is_ok() {
            mark_delivered(&db, &events, vec![comment], pane).await;
        }
    });
}

/// Records that `comments` were delivered to `pane`.
pub async fn mark_delivered(db: &Db, events: &Events, comments: Vec<CommentId>, pane: PaneId) {
    if comments.is_empty() {
        return;
    }
    let marked = db
        .call(move |c| {
            let mut marked = Vec::new();
            for id in comments {
                if db::comments::mark_delivered(c, id, pane)? {
                    marked.push(id);
                }
            }
            Ok(marked)
        })
        .await;
    match marked {
        Ok(marked) => {
            for id in marked {
                events.changed(Entity::Comment, id);
            }
        }
        Err(err) => tracing::warn!("marking comments delivered to pane {pane}: {err}"),
    }
}
