//! `tasks` queries.

use agentdeck_core::{PaneId, TaskId, TaskKind, TaskPlace, TaskStatus, WorkspaceId};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use super::{DbError, exists, new_id, now, opt_text, text};
use crate::model::{NewTask, PullRequest, Task, TaskFilter, TaskPatch};

/// A task with its PR row, if any: `FROM {TABLES}` reads both.
const COLUMNS: &str = "t.id, t.title, t.body, t.status, t.kind, t.workspace_id, t.created_at,
    t.updated_at, t.done_at, p.repo, p.number, p.url, p.state AS pr_state, p.draft, p.head_ref,
    p.checks, p.review, p.updated_at AS pr_updated_at, p.synced_at,
    (SELECT COUNT(*) FROM comments c
     WHERE c.task_id = t.id AND c.author = 'agent' AND c.seen_at IS NULL) AS unseen_agent_comments";
const TABLES: &str = "tasks t LEFT JOIN pull_requests p ON p.task_id = t.id";

fn pr_from_row(row: &Row<'_>) -> rusqlite::Result<Option<PullRequest>> {
    let Some(repo) = row.get::<_, Option<String>>("repo")? else {
        return Ok(None);
    };
    Ok(Some(PullRequest {
        repo,
        number: row.get("number")?,
        url: row.get("url")?,
        state: text(row, "pr_state")?,
        draft: row.get("draft")?,
        head_ref: row.get("head_ref")?,
        checks: opt_text(row, "checks")?,
        review: opt_text(row, "review")?,
        updated_at: row.get("pr_updated_at")?,
        synced_at: row.get("synced_at")?,
    }))
}

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
        pr: pr_from_row(row)?,
        unseen_agent_comments: row.get("unseen_agent_comments")?,
    })
}

/// The tasks matching every filter set, oldest first.
pub fn list(conn: &Connection, filter: &TaskFilter) -> Result<Vec<Task>, DbError> {
    let mut sql = format!("SELECT {COLUMNS} FROM {TABLES} WHERE 1 = 1");
    let mut values: Vec<Value> = Vec::new();
    if let Some(status) = filter.status {
        values.push(Value::Text(status.as_str().into()));
        sql.push_str(&format!(" AND t.status = ?{}", values.len()));
    }
    if let Some(kind) = filter.kind {
        values.push(Value::Text(kind.as_str().into()));
        sql.push_str(&format!(" AND t.kind = ?{}", values.len()));
    }
    if let Some(workspace) = filter.workspace {
        values.push(Value::Text(workspace.to_string()));
        sql.push_str(&format!(" AND t.workspace_id = ?{}", values.len()));
    }
    match filter.inbox {
        Some(true) => sql.push_str(" AND t.workspace_id IS NULL"),
        Some(false) => sql.push_str(" AND t.workspace_id IS NOT NULL"),
        None => {}
    }
    sql.push_str(" ORDER BY t.created_at, t.id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(values), from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One task.
pub fn get(conn: &Connection, id: TaskId) -> Result<Task, DbError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM {TABLES} WHERE t.id = ?1"),
        [id.to_string()],
        from_row,
    )
    .optional()?
    .ok_or(DbError::NotFound("task"))
}

/// Creates a plain task. PR tasks come from PR sync, through [`create_kind`].
pub fn create(conn: &Connection, new: &NewTask) -> Result<Task, DbError> {
    create_kind(conn, new, TaskKind::Plain)
}

/// Creates a task of `kind`.
pub fn create_kind(conn: &Connection, new: &NewTask, kind: TaskKind) -> Result<Task, DbError> {
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
            kind.as_str(),
            new.workspace_id.map(|id| id.to_string()),
            now,
            done_at,
        ],
    )?;
    get(conn, id)
}

/// Applies the fields `patch` sets and stamps `updated_at`. Moving to done stamps `done_at`;
/// moving out of done clears it. A workspace must exist and not be archived
/// (`unknown_workspace`). PR sync changes tasks through it; the user's edits go through
/// [`edit`], which adds the user's rules.
pub fn update(conn: &Connection, id: TaskId, patch: &TaskPatch) -> Result<Task, DbError> {
    let task = get(conn, id)?;
    if let Some(TaskPlace::Workspace(workspace_id)) = patch.workspace_id {
        check_open_workspace(conn, workspace_id)?;
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
                .map_or(task.workspace_id, TaskPlace::workspace)
                .map(|id| id.to_string()),
            now,
            done_at,
        ],
    )?;
    get(conn, id)
}

/// A task the user edited, and the panes the edit unlinked from it.
#[derive(Debug)]
pub struct Edited {
    /// The task as it is now.
    pub task: Task,
    /// The panes that were linked to it in another workspace than its new one.
    pub unlinked: Vec<PaneId>,
}

/// The user's edit of task `id` (`PATCH /v1/tasks/{id}`), in one transaction: [`update`],
/// plus the user's rules (spec §8, §9).
/// - A PR task's title is GitHub's: another title is `invalid_title`.
/// - A PR task's workspace the user sets, the inbox included, is picked: PR sync never moves
///   the task again.
/// - A task moved out of a pane's workspace unlinks the pane, since a pane links only to a
///   task in its own workspace.
pub fn edit(conn: &mut Connection, id: TaskId, patch: &TaskPatch) -> Result<Edited, DbError> {
    let tx = conn.transaction()?;
    let before = get(&tx, id)?;
    if before.kind == TaskKind::Pr
        && let Some(title) = &patch.title
        && *title != before.title
    {
        return Err(DbError::Invalid {
            code: "invalid_title",
            message: "a PR task's title is its PR's: rename the PR on GitHub".into(),
        });
    }
    let task = update(&tx, id, patch)?;
    let mut unlinked = Vec::new();
    if let Some(place) = patch.workspace_id {
        if task.kind == TaskKind::Pr {
            tx.execute(
                "UPDATE pull_requests SET workspace_picked_at = ?2 WHERE task_id = ?1",
                params![id.to_string(), now()],
            )?;
        }
        let mut stmt = tx.prepare(
            "UPDATE panes SET task_id = NULL
             WHERE task_id = ?1
               AND tab_id NOT IN (SELECT id FROM tabs WHERE workspace_id IS ?2)
             RETURNING id",
        )?;
        let rows = stmt.query_map(
            params![id.to_string(), place.workspace().map(|ws| ws.to_string())],
            |row| text(row, "id"),
        )?;
        unlinked = rows.collect::<rusqlite::Result<_>>()?;
    }
    tx.commit()?;
    Ok(Edited { task, unlinked })
}

/// Deletes a task; its comments and PR row go with it, and its panes are unlinked.
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

/// A workspace a task can move to: one that exists and isn't archived (`unknown_workspace`).
fn check_open_workspace(conn: &Connection, workspace_id: WorkspaceId) -> Result<(), DbError> {
    check_workspace(conn, workspace_id)?;
    let archived: Option<String> = conn.query_row(
        "SELECT archived_at FROM workspaces WHERE id = ?1",
        [workspace_id.to_string()],
        |row| row.get(0),
    )?;
    match archived {
        None => Ok(()),
        Some(_) => Err(DbError::Invalid {
            code: "unknown_workspace",
            message: format!("workspace {workspace_id} is archived"),
        }),
    }
}
