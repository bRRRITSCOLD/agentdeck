//! `tasks` queries.

use agentdeck_core::{TaskId, TaskKind, TaskStatus, WorkspaceId};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use super::{DbError, exists, new_id, now, opt_text, text};
use crate::model::{NewTask, Task, TaskFilter, TaskPatch};

const COLUMNS: &str =
    "id, title, body, status, kind, workspace_id, created_at, updated_at, done_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: text(row, "id")?,
        title: row.get("title")?,
        body: row.get("body")?,
        status: text(row, "status")?,
        kind: text(row, "kind")?,
        workspace_id: opt_text(row, "workspace_id")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        done_at: row.get("done_at")?,
    })
}

/// The tasks matching every filter set, oldest first.
pub fn list(conn: &Connection, filter: &TaskFilter) -> Result<Vec<Task>, DbError> {
    let mut sql = format!("SELECT {COLUMNS} FROM tasks WHERE 1 = 1");
    let mut values: Vec<Value> = Vec::new();
    if let Some(status) = filter.status {
        values.push(Value::Text(status.as_str().into()));
        sql.push_str(&format!(" AND status = ?{}", values.len()));
    }
    if let Some(kind) = filter.kind {
        values.push(Value::Text(kind.as_str().into()));
        sql.push_str(&format!(" AND kind = ?{}", values.len()));
    }
    if let Some(workspace) = filter.workspace {
        values.push(Value::Text(workspace.to_string()));
        sql.push_str(&format!(" AND workspace_id = ?{}", values.len()));
    }
    match filter.inbox {
        Some(true) => sql.push_str(" AND workspace_id IS NULL"),
        Some(false) => sql.push_str(" AND workspace_id IS NOT NULL"),
        None => {}
    }
    sql.push_str(" ORDER BY created_at, id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(values), from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One task.
pub fn get(conn: &Connection, id: TaskId) -> Result<Task, DbError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM tasks WHERE id = ?1"),
        [id.to_string()],
        from_row,
    )
    .optional()?
    .ok_or(DbError::NotFound("task"))
}

/// Creates a plain task. PR tasks come from PR sync.
pub fn create(conn: &Connection, new: &NewTask) -> Result<Task, DbError> {
    if let Some(workspace_id) = new.workspace_id {
        check_workspace(conn, workspace_id)?;
    }
    let id: TaskId = new_id();
    let status = new.status.unwrap_or(TaskStatus::Todo);
    let now = now();
    let done_at = (status == TaskStatus::Done).then(|| now.clone());
    conn.execute(
        "INSERT INTO tasks (id, title, body, status, kind, workspace_id, created_at, updated_at, done_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)",
        params![
            id.to_string(),
            new.title,
            new.body.as_deref().unwrap_or(""),
            status.as_str(),
            TaskKind::Plain.as_str(),
            new.workspace_id.map(|id| id.to_string()),
            now,
            done_at,
        ],
    )?;
    get(conn, id)
}

/// Applies the fields `patch` sets and stamps `updated_at`. Moving to done stamps `done_at`;
/// moving out of done clears it.
pub fn update(conn: &Connection, id: TaskId, patch: &TaskPatch) -> Result<Task, DbError> {
    let task = get(conn, id)?;
    if let Some(workspace_id) = patch.workspace_id {
        check_workspace(conn, workspace_id)?;
    }
    let now = now();
    let status = patch.status.unwrap_or(task.status);
    let done_at = match (task.status, status) {
        (TaskStatus::Done, TaskStatus::Done) => task.done_at,
        (_, TaskStatus::Done) => Some(now.clone()),
        _ => None,
    };
    conn.execute(
        "UPDATE tasks SET title = ?2, body = ?3, status = ?4, workspace_id = ?5,
           updated_at = ?6, done_at = ?7
         WHERE id = ?1",
        params![
            id.to_string(),
            patch.title.as_ref().unwrap_or(&task.title),
            patch.body.as_ref().unwrap_or(&task.body),
            status.as_str(),
            patch
                .workspace_id
                .or(task.workspace_id)
                .map(|id| id.to_string()),
            now,
            done_at,
        ],
    )?;
    get(conn, id)
}

/// Deletes a task; its notes and PR row go with it, and its panes are unlinked.
pub fn delete(conn: &Connection, id: TaskId) -> Result<(), DbError> {
    match conn.execute("DELETE FROM tasks WHERE id = ?1", [id.to_string()])? {
        0 => Err(DbError::NotFound("task")),
        _ => Ok(()),
    }
}

fn check_workspace(conn: &Connection, workspace_id: WorkspaceId) -> Result<(), DbError> {
    if exists(conn, "workspaces", workspace_id)? {
        Ok(())
    } else {
        Err(DbError::UnknownReference("workspace"))
    }
}
