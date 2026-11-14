//! `instructions` queries: each workspace's standing context for its agents (spec §9).

use agentdeck_core::{InstructionId, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{DbError, exists, new_id, now, text};
use crate::model::{Instruction, InstructionPatch, NewInstruction};

const COLUMNS: &str = "id, workspace_id, body, created_at, updated_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Instruction> {
    Ok(Instruction {
        id: text(row, "id")?,
        workspace_id: text(row, "workspace_id")?,
        body: row.get("body")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

/// `workspace`'s instructions, oldest first.
pub fn list(conn: &Connection, workspace: WorkspaceId) -> Result<Vec<Instruction>, DbError> {
    if !exists(conn, "workspaces", workspace)? {
        return Err(DbError::NotFound("workspace"));
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM instructions WHERE workspace_id = ?1 ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([workspace.to_string()], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One instruction.
pub fn get(conn: &Connection, id: InstructionId) -> Result<Instruction, DbError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM instructions WHERE id = ?1"),
        [id.to_string()],
        from_row,
    )
    .optional()?
    .ok_or(DbError::NotFound("instruction"))
}

/// Adds an instruction to `workspace`.
pub fn create(
    conn: &Connection,
    workspace: WorkspaceId,
    new: &NewInstruction,
) -> Result<Instruction, DbError> {
    if !exists(conn, "workspaces", workspace)? {
        return Err(DbError::NotFound("workspace"));
    }
    let id: InstructionId = new_id();
    let created = now();
    conn.execute(
        "INSERT INTO instructions (id, workspace_id, body, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?4)",
        params![id.to_string(), workspace.to_string(), new.body, created],
    )?;
    get(conn, id)
}

/// Applies the fields `patch` sets.
pub fn update(
    conn: &Connection,
    id: InstructionId,
    patch: &InstructionPatch,
) -> Result<Instruction, DbError> {
    get(conn, id)?;
    if let Some(body) = &patch.body {
        conn.execute(
            "UPDATE instructions SET body = ?2, updated_at = ?3 WHERE id = ?1",
            params![id.to_string(), body, now()],
        )?;
    }
    get(conn, id)
}

/// Deletes an instruction.
pub fn delete(conn: &Connection, id: InstructionId) -> Result<(), DbError> {
    match conn.execute("DELETE FROM instructions WHERE id = ?1", [id.to_string()])? {
        0 => Err(DbError::NotFound("instruction")),
        _ => Ok(()),
    }
}
