//! Delivering the user's task comments (spec §9): a comment on a task with a live agent pane
//! is typed into that pane once the agent waits at its prompt, never into a dialog.
//! Delivery records the time and the pane. An agent's comments are never delivered.
//!
//! A comment reaches an agent at most once. While it waits for a pane's prompt it is held for
//! that one pane in [`Claims`], and it is typed only if the pane still holds it right before
//! typing. Two panes on one task can both be handed a waiting comment (a new comment, then a
//! link), so the hand-off decides which pane holds it: the newest agent pane linked to the
//! task, as for a new comment. A launch prompt that carries it takes it from any pane.
//!
//! A shell pane counts as an agent pane while it hosts an agent (spec §3, §7): it takes
//! comments only then, and when an agent starts in it the task's waiting comments are handed
//! to it as on linking ([`deliver_on_hosting`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use agentdeck_core::ws::Entity;
use agentdeck_core::{AgentState, CommentAuthor, CommentId, PaneId, PaneKind, TaskId};
use rusqlite::Connection;

use crate::db::{self, Db, DbError};
use crate::events::Events;
use crate::model::Comment;
use crate::terminal::Terminals;
use crate::terminal::pane::Pane;
use tokio::sync::broadcast;

/// Which pane each comment on its way to an agent is for, so it reaches only one.
///
/// A claim is taken only in a database job that has just read the comment as undelivered,
/// and dropped only in the job that marks it delivered (or when its pane lets go of it
/// untyped). Jobs run one at a time, so no hand-off can read a comment as undelivered after
/// it was typed and then claim it again.
#[derive(Clone, Default)]
pub struct Claims(Arc<Mutex<HashMap<CommentId, Claim>>>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Claim {
    /// Waiting in this pane's queue for its prompt.
    Held(PaneId),
    /// Typed, or put in a launch prompt; not yet marked delivered.
    Given,
}

impl Claims {
    fn lock(&self) -> MutexGuard<'_, HashMap<CommentId, Claim>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Holds `comment` for `pane` unless another pane wins it: one it was given to already,
    /// or one holding it that is newer than `pane` and still linked to the task. `linked` is
    /// the task's agent panes, newest first. Whether `pane` holds it now.
    fn hold(&self, comment: CommentId, pane: PaneId, linked: &[PaneId]) -> bool {
        let newest_first = |id: PaneId| linked.iter().position(|&p| p == id);
        let mut claims = self.lock();
        let wins = match claims.get(&comment) {
            None => true,
            Some(Claim::Given) => false,
            Some(&Claim::Held(holder)) if holder == pane => false,
            Some(&Claim::Held(holder)) => match (newest_first(pane), newest_first(holder)) {
                (Some(pane), Some(holder)) => pane < holder,
                (_, None) => true,
                (None, Some(_)) => false,
            },
        };
        if wins {
            claims.insert(comment, Claim::Held(pane));
        }
        wins
    }

    /// Gives `comment` to `pane` to type, if `pane` still holds it. Asked right before typing.
    fn give(&self, comment: CommentId, pane: PaneId) -> bool {
        let mut claims = self.lock();
        if claims.get(&comment) != Some(&Claim::Held(pane)) {
            return false;
        }
        claims.insert(comment, Claim::Given);
        true
    }

    /// Gives `comment` to a launch prompt, taking it from any pane holding it, unless it was
    /// given already. Whether the prompt may carry it.
    fn prompt(&self, comment: CommentId) -> bool {
        let mut claims = self.lock();
        if claims.get(&comment) == Some(&Claim::Given) {
            return false;
        }
        claims.insert(comment, Claim::Given);
        true
    }

    /// Lets go of `comment` if `pane` holds it: the pane exited before typing it.
    fn release(&self, comment: CommentId, pane: PaneId) {
        let mut claims = self.lock();
        if claims.get(&comment) == Some(&Claim::Held(pane)) {
            claims.remove(&comment);
        }
    }

    /// Forgets `comment`: it was marked delivered, or the launch meant to carry it failed.
    fn forget(&self, comment: CommentId) {
        self.lock().remove(&comment);
    }
}

/// `task`'s undelivered user comments for a new agent pane's launch prompt, oldest first:
/// those no pane has been given already, taken from any pane holding them. Run in the job
/// that creates the pane; once it has started call [`mark_delivered`], or [`launch_failed`]
/// if it couldn't.
pub fn for_launch_prompt(
    conn: &Connection,
    claims: &Claims,
    task: TaskId,
) -> Result<Vec<Comment>, DbError> {
    let mut comments = db::comments::undelivered(conn, task)?;
    comments.retain(|comment| claims.prompt(comment.id));
    Ok(comments)
}

/// Lets go of the comments a launch prompt was to carry, for a pane that couldn't start.
pub fn launch_failed(claims: &Claims, comments: &[Comment]) {
    for comment in comments {
        claims.forget(comment.id);
    }
}

/// Hands a new user `comment` to its task's newest live agent pane, if the task has one. It
/// is typed, with Enter, once the agent is idle or done, and marked delivered then; it stays
/// undelivered if the pane exits first. An agent's comment is left alone.
pub async fn deliver_to_task_pane(
    db: &Db,
    claims: &Claims,
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
    if let Some(pane) = live {
        let id = comment.id;
        hand_off(db, claims, events, &pane, task, move |c| {
            let mut waiting = db::comments::undelivered(c, task)?;
            waiting.retain(|comment| comment.id == id);
            Ok(waiting)
        })
        .await;
    }
}

/// Hands `task`'s user comments no agent has had yet, oldest first, to `pane`, which was just
/// linked to it or started hosting an agent, if it is a live agent pane or hosts one: each is
/// typed and marked delivered as [`deliver_to_task_pane`] does. A comment another pane holds
/// stays with whichever of the two is newer.
pub async fn deliver_waiting(
    db: &Db,
    claims: &Claims,
    terminals: &Terminals,
    events: &Events,
    task: TaskId,
    pane: PaneId,
) {
    let Some(live) = terminals
        .get(pane)
        .filter(|live| live.status().state != AgentState::Exited)
    else {
        return;
    };
    hand_off(db, claims, events, &live, task, move |c| {
        let row = db::panes::get(c, pane)?;
        if row.kind != PaneKind::Agent && row.hosted_agent.is_none() {
            return Ok(Vec::new());
        }
        db::comments::undelivered(c, task)
    })
    .await;
}

/// Hands each shell pane that starts hosting an agent its task's waiting comments, as linking
/// it would (spec §9). Runs until `hosting` closes, with the daemon.
pub async fn deliver_on_hosting(
    db: Db,
    claims: Claims,
    terminals: Terminals,
    events: Events,
    mut hosting: broadcast::Receiver<PaneId>,
) {
    loop {
        let pane = match hosting.recv().await {
            Ok(pane) => pane,
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                tracing::warn!("comment delivery missed {missed} newly hosted agents");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        let task = match db.call(move |c| db::panes::get(c, pane)).await {
            Ok(row) => row.task_id,
            Err(err) => {
                tracing::warn!("reading pane {pane} for its task: {err}");
                continue;
            }
        };
        if let Some(task) = task {
            deliver_waiting(&db, &claims, &terminals, &events, task, pane).await;
        }
    }
}

/// Holds for `pane` each comment `waiting` reads that it wins (see [`Claims::hold`]), in the
/// same job, then queues them on it.
async fn hand_off(
    db: &Db,
    claims: &Claims,
    events: &Events,
    pane: &Pane,
    task: TaskId,
    waiting: impl FnOnce(&Connection) -> Result<Vec<Comment>, DbError> + Send + 'static,
) {
    let (id, holds) = (pane.id(), claims.clone());
    let held = db
        .call(move |c| {
            let linked = db::panes::agents_on_task(c, task)?;
            let mut comments = waiting(c)?;
            comments.retain(|comment| holds.hold(comment.id, id, &linked));
            Ok(comments)
        })
        .await;
    match held {
        Ok(held) => {
            for comment in &held {
                deliver(db, claims, events, pane, comment);
            }
        }
        Err(err) => tracing::warn!("finding task {task}'s comments for pane {id}: {err}"),
    }
}

/// Submits `comment` (typed, then Enter) into `pane` once the agent is idle or done, if the
/// pane still holds it then, and marks it delivered; it stays undelivered if the pane exits
/// first.
fn deliver(db: &Db, claims: &Claims, events: &Events, pane: &Pane, comment: &Comment) {
    let text = comment.body.trim_end();
    let (id, pane_id, gives) = (comment.id, pane.id(), claims.clone());
    let claim = Box::new(move || gives.give(id, pane_id));
    let Some(typed) = pane.deliver(text.as_bytes(), claim) else {
        claims.release(id, pane_id);
        return;
    };
    let (db, claims, events) = (db.clone(), claims.clone(), events.clone());
    tokio::spawn(async move {
        match typed.await {
            Ok(()) => mark_delivered(&db, &claims, &events, vec![id], pane_id).await,
            Err(_) => claims.release(id, pane_id),
        }
    });
}

/// Records that `comments` were delivered to `pane`.
pub async fn mark_delivered(
    db: &Db,
    claims: &Claims,
    events: &Events,
    comments: Vec<CommentId>,
    pane: PaneId,
) {
    if comments.is_empty() {
        return;
    }
    let claims = claims.clone();
    let marked = db
        .call(move |c| {
            let mut marked = Vec::new();
            for id in comments {
                let changed = db::comments::mark_delivered(c, id, pane);
                // Delivered now, earlier or never (deleted): no pane may have it again.
                claims.forget(id);
                if changed? {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A comment and two panes.
    fn ids() -> (CommentId, PaneId, PaneId) {
        (db::new_id(), db::new_id(), db::new_id())
    }

    mod claims {
        use super::*;

        #[test]
        fn should_give_a_comment_only_to_the_pane_holding_it() {
            let claims = Claims::default();
            let (comment, older, newer) = ids();
            assert!(claims.hold(comment, older, &[newer, older]));
            assert!(claims.hold(comment, newer, &[newer, older]));

            assert!(!claims.give(comment, older));
            assert!(claims.give(comment, newer));
            assert!(!claims.give(comment, newer), "given twice");
        }

        #[test]
        fn should_leave_a_comment_with_the_newer_linked_pane_holding_it() {
            let claims = Claims::default();
            let (comment, older, newer) = ids();
            assert!(claims.hold(comment, newer, &[newer, older]));

            assert!(!claims.hold(comment, older, &[newer, older]));
            assert!(claims.give(comment, newer));
        }

        #[test]
        fn should_take_a_comment_from_a_pane_no_longer_linked() {
            let claims = Claims::default();
            let (comment, unlinked, linked) = ids();
            assert!(claims.hold(comment, unlinked, &[unlinked]));

            assert!(claims.hold(comment, linked, &[linked]));
            assert!(claims.give(comment, linked));
        }

        #[test]
        fn should_hold_a_given_comment_for_no_pane_and_no_launch_prompt() {
            let claims = Claims::default();
            let (comment, pane, other) = ids();
            assert!(claims.hold(comment, pane, &[pane]));
            assert!(claims.give(comment, pane));

            assert!(!claims.hold(comment, other, &[other, pane]));
            assert!(!claims.prompt(comment));
        }

        #[test]
        fn should_let_a_launch_prompt_take_a_held_comment() {
            let claims = Claims::default();
            let (comment, pane, _) = ids();
            assert!(claims.hold(comment, pane, &[pane]));

            assert!(claims.prompt(comment));
            assert!(!claims.give(comment, pane));
        }

        #[test]
        fn should_let_another_pane_hold_a_comment_its_pane_let_go_of() {
            let claims = Claims::default();
            let (comment, exited, other) = ids();
            assert!(claims.hold(comment, exited, &[exited, other]));

            claims.release(comment, exited);

            assert!(claims.hold(comment, other, &[exited, other]));
        }

        #[test]
        fn should_not_let_a_pane_let_go_of_a_comment_another_holds() {
            let claims = Claims::default();
            let (comment, older, newer) = ids();
            assert!(claims.hold(comment, older, &[newer, older]));
            assert!(claims.hold(comment, newer, &[newer, older]));

            claims.release(comment, older);

            assert!(claims.give(comment, newer));
        }

        #[test]
        fn should_let_a_pane_hold_a_comment_again_once_forgotten() {
            let claims = Claims::default();
            let (comment, pane, _) = ids();
            assert!(claims.prompt(comment));

            claims.forget(comment);

            assert!(claims.hold(comment, pane, &[pane]));
        }
    }
}
