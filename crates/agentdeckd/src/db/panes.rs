//! `panes` queries.

use std::collections::HashSet;

use agentdeck_core::{Agent, AgentState, PaneId, TabId, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{DbError, new_id, now, opt_text, text};
use crate::model::{NewPane, Pane};

const COLUMNS: &str = "id, tab_id, kind, agent, hosted_agent, argv, cwd, folder, session_id, \
                       state, title, exit_code, created_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Pane> {
    let argv: String = row.get("argv")?;
    let argv = serde_json::from_str(&argv).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, err.into())
    })?;
    let cwd: String = row.get("cwd")?;
    // NULL until a read differs from where it started: `cwd` then (spec §3).
    let folder: Option<String> = row.get("folder")?;
    Ok(Pane {
        id: text(row, "id")?,
        tab_id: text(row, "tab_id")?,
        kind: text(row, "kind")?,
        agent: opt_text(row, "agent")?,
        hosted_agent: opt_text(row, "hosted_agent")?,
        last_hosted: None,
        argv,
        folder: folder.unwrap_or_else(|| cwd.clone()),
        cwd,
        session_id: row.get("session_id")?,
        state: text(row, "state")?,
        title: row.get("title")?,
        exit_code: row.get("exit_code")?,
        created_at: row.get("created_at")?,
    })
}

/// The workspace a new pane in `new`'s tab belongs to; `unknown_tab` when the tab doesn't
/// exist.
fn workspace_for(conn: &Connection, new: &NewPane) -> Result<WorkspaceId, DbError> {
    let workspace: Option<WorkspaceId> = conn
        .query_row(
            "SELECT workspace_id FROM tabs WHERE id = ?1",
            [new.tab_id.to_string()],
            |row| text(row, "workspace_id"),
        )
        .optional()?;
    workspace.ok_or(DbError::UnknownReference("tab"))
}

/// The workspace pane `id`'s tab is in.
pub fn workspace_of(conn: &Connection, id: PaneId) -> Result<WorkspaceId, DbError> {
    conn.query_row(
        "SELECT t.workspace_id FROM panes p JOIN tabs t ON t.id = p.tab_id WHERE p.id = ?1",
        [id.to_string()],
        |row| text(row, "workspace_id"),
    )
    .optional()?
    .ok_or(DbError::NotFound("pane"))
}

/// The current folder of every pane in Workspace `workspace`'s tabs, exited ones included:
/// its last known `folder`, else the `cwd` it launched in (spec §3), each once.
pub fn folders_in(conn: &Connection, workspace: WorkspaceId) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT COALESCE(p.folder, p.cwd) FROM panes p JOIN tabs t ON t.id = p.tab_id
         WHERE t.workspace_id = ?1 ORDER BY 1",
    )?;
    let rows = stmt.query_map([workspace.to_string()], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The Workspace tab `id` is in; `None` once the tab is gone.
pub fn workspace_of_tab(conn: &Connection, id: TabId) -> Result<Option<WorkspaceId>, DbError> {
    Ok(conn
        .query_row(
            "SELECT workspace_id FROM tabs WHERE id = ?1",
            [id.to_string()],
            |row| text(row, "workspace_id"),
        )
        .optional()?)
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
        "INSERT INTO panes (id, tab_id, kind, agent, argv, cwd, session_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            id.to_string(),
            new.tab_id.to_string(),
            new.kind.as_str(),
            new.agent.map(|agent| agent.as_str()),
            serde_json::to_string(argv).expect("a string list always serializes"),
            new.cwd,
            session_id,
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

/// Records pane `id`'s last known folder (spec §6): where it is now, or `None` for the folder
/// it launched in, `cwd`.
pub fn set_folder(conn: &Connection, id: PaneId, folder: Option<&str>) -> Result<(), DbError> {
    conn.execute(
        "UPDATE panes SET folder = ?2 WHERE id = ?1",
        params![id.to_string(), folder],
    )?;
    Ok(())
}

/// Records the agent a shell pane hosts now, or `None` once it hosts none (spec §7).
pub fn set_hosted_agent(
    conn: &Connection,
    id: PaneId,
    agent: Option<Agent>,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE panes SET hosted_agent = ?2 WHERE id = ?1",
        params![id.to_string(), agent.map(Agent::as_str)],
    )?;
    Ok(())
}

/// Forgets every hosted agent, as the daemon starts: a shell pane comes back as its shell
/// alone (spec §6).
pub fn forget_hosted_agents(conn: &Connection) -> Result<(), DbError> {
    conn.execute(
        "UPDATE panes SET hosted_agent = NULL WHERE hosted_agent IS NOT NULL",
        [],
    )?;
    Ok(())
}

/// Records that the pane's process exited; it hosts no agent any more.
pub fn set_exited(
    conn: &Connection,
    id: PaneId,
    code: Option<i64>,
    title: &str,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE panes SET state = 'exited', exit_code = ?2, title = ?3, hosted_agent = NULL
         WHERE id = ?1",
        params![id.to_string(), code, title],
    )?;
    Ok(())
}

/// Records stopped pane `id` as exited, with `title`, as the daemon stops before its process
/// has (spec §6); an exit already saved, with its code, stays as it is.
pub fn set_stopped(conn: &Connection, id: PaneId, title: &str) -> Result<(), DbError> {
    conn.execute(
        "UPDATE panes SET state = 'exited', title = ?2, hosted_agent = NULL
         WHERE id = ?1 AND state != 'exited'",
        params![id.to_string(), title],
    )?;
    Ok(())
}

/// Turns pane `id` into a shell pane running `argv` (spec §3): no agent, no session.
pub fn become_shell(conn: &Connection, id: PaneId, argv: &[String]) -> Result<Pane, DbError> {
    conn.execute(
        "UPDATE panes SET kind = 'shell', agent = NULL, session_id = NULL, hosted_agent = NULL,
                          argv = ?2
         WHERE id = ?1",
        params![
            id.to_string(),
            serde_json::to_string(argv).expect("a string list always serializes")
        ],
    )?;
    get(conn, id)
}

/// Starts pane `id`'s state over as it is spawned again in `folder` (spec §4): `unknown`, no
/// exit code, no hosted agent, and `folder` both its launch folder (`cwd`), where a resume of
/// the session it starts there runs, and its last known folder. A fresh launch passes the
/// `argv` and `session_id` it starts with.
pub fn respawned(
    conn: &Connection,
    id: PaneId,
    folder: &str,
    fresh: Option<(&[String], Option<&str>)>,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE panes SET state = 'unknown', exit_code = NULL, hosted_agent = NULL, cwd = ?2,
                          folder = ?2
         WHERE id = ?1",
        params![id.to_string(), folder],
    )?;
    if let Some((argv, session_id)) = fresh {
        conn.execute(
            "UPDATE panes SET argv = ?2, session_id = ?3 WHERE id = ?1",
            params![
                id.to_string(),
                serde_json::to_string(argv).expect("a string list always serializes"),
                session_id
            ],
        )?;
    }
    Ok(())
}

/// Deletes a pane.
pub fn delete(conn: &Connection, id: PaneId) -> Result<(), DbError> {
    match conn.execute("DELETE FROM panes WHERE id = ?1", [id.to_string()])? {
        0 => Err(DbError::NotFound("pane")),
        _ => Ok(()),
    }
}
