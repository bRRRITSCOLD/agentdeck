//! `panes` queries.

use std::collections::HashSet;

use agentdeck_core::{AgentState, PaneId, TaskId, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{DbError, exists, new_id, now, opt_text, text};
use crate::model::{NewPane, Pane};

const COLUMNS: &str = "id, tab_id, kind, agent, argv, cwd, session_id, task_id, state, title, \
                       exit_code, created_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Pane> {
    let argv: String = row.get("argv")?;
    let argv = serde_json::from_str(&argv).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, err.into())
    })?;
    Ok(Pane {
        id: text(row, "id")?,
        tab_id: text(row, "tab_id")?,
        kind: text(row, "kind")?,
        agent: opt_text(row, "agent")?,
        argv,
        cwd: row.get("cwd")?,
        session_id: row.get("session_id")?,
        task_id: opt_text(row, "task_id")?,
        state: text(row, "state")?,
        title: row.get("title")?,
        exit_code: row.get("exit_code")?,
        created_at: row.get("created_at")?,
    })
}

/// The workspace a new pane in `new`'s tab belongs to; `unknown_tab` or `unknown_task` when
/// the pane refers to an entity that doesn't exist.
pub fn workspace_for(conn: &Connection, new: &NewPane) -> Result<WorkspaceId, DbError> {
    let workspace: Option<WorkspaceId> = conn
        .query_row(
            "SELECT workspace_id FROM tabs WHERE id = ?1",
            [new.tab_id.to_string()],
            |row| text(row, "workspace_id"),
        )
        .optional()?;
    let workspace = workspace.ok_or(DbError::UnknownReference("tab"))?;
    if let Some(task) = new.task_id
        && !exists(conn, "tasks", task)?
    {
        return Err(DbError::UnknownReference("task"));
    }
    Ok(workspace)
}

/// Adds a pane to its tab, before its process starts: `argv` is what will be launched and
/// `session_id` the agent session it starts on. Returns it with the workspace its tab is in.
pub fn create(
    conn: &Connection,
    new: &NewPane,
    argv: &[String],
    session_id: Option<&str>,
) -> Result<(Pane, WorkspaceId), DbError> {
    let workspace = workspace_for(conn, new)?;
    let id: PaneId = new_id();
    conn.execute(
        "INSERT INTO panes (id, tab_id, kind, agent, argv, cwd, session_id, task_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id.to_string(),
            new.tab_id.to_string(),
            new.kind.as_str(),
            new.agent.map(|agent| agent.as_str()),
            serde_json::to_string(argv).expect("a string list always serializes"),
            new.cwd,
            session_id,
            new.task_id.map(|task| task.to_string()),
            now(),
        ],
    )?;
    Ok((get(conn, id)?, workspace))
}

/// One pane.
pub fn get(conn: &Connection, id: PaneId) -> Result<Pane, DbError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM panes WHERE id = ?1"),
        [id.to_string()],
        from_row,
    )
    .optional()?
    .ok_or(DbError::NotFound("pane"))
}

/// Every pane's id.
pub fn ids(conn: &Connection) -> Result<Vec<PaneId>, DbError> {
    let mut stmt = conn.prepare("SELECT id FROM panes")?;
    let rows = stmt.query_map([], |row| text(row, "id"))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Every pane whose process hadn't exited when the daemon last stopped, with its
/// workspace, oldest first: the panes to resume (spec §6).
pub fn resumable(conn: &Connection) -> Result<Vec<(Pane, WorkspaceId)>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT p.*, t.workspace_id FROM panes p JOIN tabs t ON t.id = p.tab_id
         WHERE p.state <> 'exited' ORDER BY p.created_at, p.id",
    )?;
    let rows = stmt.query_map([], |row| Ok((from_row(row)?, text(row, "workspace_id")?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The agent panes linked to `task`, newest first.
pub fn agents_on_task(conn: &Connection, task: TaskId) -> Result<Vec<PaneId>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id FROM panes WHERE task_id = ?1 AND kind = 'agent'
         ORDER BY created_at DESC, id DESC",
    )?;
    let rows = stmt.query_map([task.to_string()], |row| text(row, "id"))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Every session id a pane has.
pub fn session_ids(conn: &Connection) -> Result<HashSet<String>, DbError> {
    let mut stmt = conn.prepare("SELECT session_id FROM panes WHERE session_id IS NOT NULL")?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Records the agent session a pane runs.
pub fn set_session_id(conn: &Connection, id: PaneId, session_id: &str) -> Result<(), DbError> {
    conn.execute(
        "UPDATE panes SET session_id = ?2 WHERE id = ?1",
        params![id.to_string(), session_id],
    )?;
    Ok(())
}

/// Records a published state and the title it was seen under. An exited pane stays exited.
pub fn set_state(
    conn: &Connection,
    id: PaneId,
    state: AgentState,
    title: &str,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE panes SET state = ?2, title = ?3 WHERE id = ?1 AND state <> 'exited'",
        params![id.to_string(), state.as_str(), title],
    )?;
    Ok(())
}

/// Records that the pane's process exited.
pub fn set_exited(
    conn: &Connection,
    id: PaneId,
    code: Option<i64>,
    title: &str,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE panes SET state = 'exited', exit_code = ?2, title = ?3 WHERE id = ?1",
        params![id.to_string(), code, title],
    )?;
    Ok(())
}

/// Deletes a pane.
pub fn delete(conn: &Connection, id: PaneId) -> Result<(), DbError> {
    match conn.execute("DELETE FROM panes WHERE id = ?1", [id.to_string()])? {
        0 => Err(DbError::NotFound("pane")),
        _ => Ok(()),
    }
}
