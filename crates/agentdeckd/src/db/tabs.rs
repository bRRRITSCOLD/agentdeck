//! `tabs` queries.

use agentdeck_core::{Layout, TabId, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{DbError, exists, new_id, text};
use crate::model::{Tab, TabPatch};

const COLUMNS: &str = "id, workspace_id, title, position, layout";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Tab> {
    let layout: String = row.get("layout")?;
    let layout = serde_json::from_str(&layout).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, err.into())
    })?;
    Ok(Tab {
        id: text(row, "id")?,
        workspace_id: text(row, "workspace_id")?,
        title: row.get("title")?,
        position: row.get("position")?,
        layout,
    })
}

fn layout_json(layout: Option<&Layout>) -> String {
    serde_json::to_string(&layout).expect("a layout always serializes")
}

/// A workspace's tabs, in position order.
pub fn list(conn: &Connection, workspace_id: WorkspaceId) -> Result<Vec<Tab>, DbError> {
    if !exists(conn, "workspaces", workspace_id)? {
        return Err(DbError::NotFound("workspace"));
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM tabs WHERE workspace_id = ?1 ORDER BY position, id"
    ))?;
    let rows = stmt.query_map([workspace_id.to_string()], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One tab.
pub fn get(conn: &Connection, id: TabId) -> Result<Tab, DbError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM tabs WHERE id = ?1"),
        [id.to_string()],
        from_row,
    )
    .optional()?
    .ok_or(DbError::NotFound("tab"))
}

/// Adds a tab after the workspace's last one.
pub fn create(
    conn: &Connection,
    workspace_id: WorkspaceId,
    title: &str,
    layout: Option<&Layout>,
) -> Result<Tab, DbError> {
    if !exists(conn, "workspaces", workspace_id)? {
        return Err(DbError::NotFound("workspace"));
    }
    let id: TabId = new_id();
    conn.execute(
        "INSERT INTO tabs (id, workspace_id, title, position, layout)
         VALUES (?1, ?2, ?3,
           (SELECT COALESCE(MAX(position) + 1, 0) FROM tabs WHERE workspace_id = ?2), ?4)",
        params![
            id.to_string(),
            workspace_id.to_string(),
            title,
            layout_json(layout)
        ],
    )?;
    get(conn, id)
}

/// Applies the fields `patch` sets.
pub fn update(conn: &Connection, id: TabId, patch: &TabPatch) -> Result<Tab, DbError> {
    get(conn, id)?;
    if let Some(title) = &patch.title {
        conn.execute(
            "UPDATE tabs SET title = ?2 WHERE id = ?1",
            params![id.to_string(), title],
        )?;
    }
    if let Some(position) = patch.position {
        conn.execute(
            "UPDATE tabs SET position = ?2 WHERE id = ?1",
            params![id.to_string(), position],
        )?;
    }
    if let Some(layout) = &patch.layout {
        conn.execute(
            "UPDATE tabs SET layout = ?2 WHERE id = ?1",
            params![id.to_string(), layout_json(Some(layout))],
        )?;
    }
    get(conn, id)
}

/// Deletes a tab and its panes.
pub fn delete(conn: &Connection, id: TabId) -> Result<(), DbError> {
    match conn.execute("DELETE FROM tabs WHERE id = ?1", [id.to_string()])? {
        0 => Err(DbError::NotFound("tab")),
        _ => Ok(()),
    }
}
