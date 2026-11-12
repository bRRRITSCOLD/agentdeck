//! `notes` queries.

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use uuid::Uuid;

use super::{DbError, exists, id, new_id, now, opt_id};
use crate::model::{NewNote, Note, NoteFilter, NotePatch};

const COLUMNS: &str = "id, body, workspace_id, task_id, created_at, read_at, read_by_pane";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Note> {
    Ok(Note {
        id: id(row, "id")?,
        body: row.get("body")?,
        workspace_id: opt_id(row, "workspace_id")?,
        task_id: opt_id(row, "task_id")?,
        created_at: row.get("created_at")?,
        read_at: row.get("read_at")?,
        read_by_pane: opt_id(row, "read_by_pane")?,
    })
}

/// The notes matching every filter set, oldest first.
pub fn list(conn: &Connection, filter: &NoteFilter) -> Result<Vec<Note>, DbError> {
    let mut sql = format!("SELECT {COLUMNS} FROM notes WHERE 1 = 1");
    let mut values: Vec<Value> = Vec::new();
    if let Some(workspace) = filter.workspace {
        values.push(Value::Text(workspace.to_string()));
        sql.push_str(&format!(" AND workspace_id = ?{}", values.len()));
    }
    if let Some(task) = filter.task {
        values.push(Value::Text(task.to_string()));
        sql.push_str(&format!(" AND task_id = ?{}", values.len()));
    }
    match filter.unread {
        Some(true) => sql.push_str(" AND read_at IS NULL"),
        Some(false) => sql.push_str(" AND read_at IS NOT NULL"),
        None => {}
    }
    sql.push_str(" ORDER BY created_at, id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(values), from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One note.
pub fn get(conn: &Connection, id: Uuid) -> Result<Note, DbError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM notes WHERE id = ?1"),
        [id.to_string()],
        from_row,
    )
    .optional()?
    .ok_or(DbError::NotFound("note"))
}

/// Leaves a note on exactly one of a workspace and a task.
pub fn create(conn: &Connection, new: &NewNote) -> Result<Note, DbError> {
    match (new.workspace_id, new.task_id) {
        (Some(workspace_id), None) => {
            if !exists(conn, "workspaces", workspace_id)? {
                return Err(DbError::UnknownReference("workspace"));
            }
        }
        (None, Some(task_id)) => {
            if !exists(conn, "tasks", task_id)? {
                return Err(DbError::UnknownReference("task"));
            }
        }
        _ => {
            return Err(DbError::Invalid {
                code: "invalid_target",
                message: "a note is on exactly one of a workspace and a task".into(),
            });
        }
    }
    let id = new_id();
    conn.execute(
        "INSERT INTO notes (id, body, workspace_id, task_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            id.to_string(),
            new.body,
            new.workspace_id.map(|id| id.to_string()),
            new.task_id.map(|id| id.to_string()),
            now(),
        ],
    )?;
    get(conn, id)
}

/// Applies the fields `patch` sets. `read: true` stamps `read_at` (keeping an earlier
/// stamp) and records `read_by_pane`; `read: false` clears both.
pub fn update(conn: &Connection, id: Uuid, patch: &NotePatch) -> Result<Note, DbError> {
    get(conn, id)?;
    if let Some(body) = &patch.body {
        conn.execute(
            "UPDATE notes SET body = ?2 WHERE id = ?1",
            params![id.to_string(), body],
        )?;
    }
    match patch.read {
        Some(true) => {
            conn.execute(
                "UPDATE notes SET read_at = COALESCE(read_at, ?2),
                   read_by_pane = COALESCE(?3, read_by_pane)
                 WHERE id = ?1",
                params![
                    id.to_string(),
                    now(),
                    patch.read_by_pane.map(|id| id.to_string())
                ],
            )?;
        }
        Some(false) => {
            conn.execute(
                "UPDATE notes SET read_at = NULL, read_by_pane = NULL WHERE id = ?1",
                [id.to_string()],
            )?;
        }
        None => {}
    }
    get(conn, id)
}

/// Deletes a note.
pub fn delete(conn: &Connection, id: Uuid) -> Result<(), DbError> {
    match conn.execute("DELETE FROM notes WHERE id = ?1", [id.to_string()])? {
        0 => Err(DbError::NotFound("note")),
        _ => Ok(()),
    }
}
