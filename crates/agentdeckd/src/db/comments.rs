//! `comments` queries: each task's thread (spec §9).

use agentdeck_core::{Agent, CommentAuthor, CommentId, PaneId, TaskId};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{DbError, exists, new_id, now, opt_text, text};
use crate::model::{Comment, NewComment};

const COLUMNS: &str = "id, task_id, body, author, author_agent, author_pane, created_at, \
                       delivered_at, delivered_to_pane";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Comment> {
    Ok(Comment {
        id: text(row, "id")?,
        task_id: text(row, "task_id")?,
        body: row.get("body")?,
        author: text(row, "author")?,
        author_agent: opt_text(row, "author_agent")?,
        author_pane: opt_text(row, "author_pane")?,
        created_at: row.get("created_at")?,
        delivered_at: row.get("delivered_at")?,
        delivered_to_pane: opt_text(row, "delivered_to_pane")?,
    })
}

/// `task`'s comments, oldest first.
pub fn list(conn: &Connection, task: TaskId) -> Result<Vec<Comment>, DbError> {
    if !exists(conn, "tasks", task)? {
        return Err(DbError::NotFound("task"));
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM comments WHERE task_id = ?1 ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([task.to_string()], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The user's comments on `task` no agent has had yet, oldest first: what a new agent pane's
/// launch prompt carries.
pub fn undelivered(conn: &Connection, task: TaskId) -> Result<Vec<Comment>, DbError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM comments
         WHERE task_id = ?1 AND author = 'user' AND delivered_at IS NULL
         ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([task.to_string()], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One comment.
pub fn get(conn: &Connection, id: CommentId) -> Result<Comment, DbError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM comments WHERE id = ?1"),
        [id.to_string()],
        from_row,
    )
    .optional()?
    .ok_or(DbError::NotFound("comment"))
}

/// Adds a comment to `task`: the user's, or with `pane` that pane's agent's.
pub fn create(conn: &Connection, task: TaskId, new: &NewComment) -> Result<Comment, DbError> {
    if !exists(conn, "tasks", task)? {
        return Err(DbError::NotFound("task"));
    }
    let (author, agent) = match new.pane {
        None => (CommentAuthor::User, None),
        Some(pane) => {
            let agent: Option<Option<String>> = conn
                .query_row(
                    "SELECT agent FROM panes WHERE id = ?1",
                    [pane.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(agent) = agent else {
                return Err(DbError::UnknownReference("pane"));
            };
            let agent = agent
                .and_then(|agent| agent.parse::<Agent>().ok())
                .filter(|agent| matches!(agent, Agent::Claude | Agent::Codex));
            (CommentAuthor::Agent, agent)
        }
    };
    let id: CommentId = new_id();
    conn.execute(
        "INSERT INTO comments (id, task_id, body, author, author_agent, author_pane, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id.to_string(),
            task.to_string(),
            new.body,
            author.as_str(),
            agent.map(Agent::as_str),
            new.pane.map(|pane| pane.to_string()),
            now(),
        ],
    )?;
    get(conn, id)
}

/// Records that `id` was delivered to `pane`, unless it was delivered already. Returns
/// whether it changed.
pub fn mark_delivered(conn: &Connection, id: CommentId, pane: PaneId) -> Result<bool, DbError> {
    let changed = conn.execute(
        "UPDATE comments SET delivered_at = ?2, delivered_to_pane = ?3
         WHERE id = ?1 AND delivered_at IS NULL",
        params![id.to_string(), now(), pane.to_string()],
    )?;
    Ok(changed > 0)
}

/// Deletes a comment.
pub fn delete(conn: &Connection, id: CommentId) -> Result<(), DbError> {
    match conn.execute("DELETE FROM comments WHERE id = ?1", [id.to_string()])? {
        0 => Err(DbError::NotFound("comment")),
        _ => Ok(()),
    }
}
