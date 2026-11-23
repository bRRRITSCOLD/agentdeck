//! `repositories` and `links` queries: the Repository of each pull request PR sync reads
//! (spec §3, §8), and the Links between Workspaces and Repositories (spec §3, §4).

use agentdeck_core::{RepositoryId, RepositoryName, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, params};

use super::DbError;
use crate::model::RepositoryLinks;
use crate::prsync::github::RemoteRepository;

/// Stores `repository` as GitHub reports it now, by its id: a new one is created; a known one
/// takes its new name, URL, private and archived, so a rename or a transfer keeps it the same
/// Repository. Returns whether anything changed: `false` when it was stored as it is.
pub fn store(conn: &Connection, repository: &RemoteRepository) -> Result<bool, DbError> {
    let id = repository.id.get();
    let stored: Option<(String, String, bool, bool)> = conn
        .query_row(
            "SELECT full_name, url, private, archived FROM repositories WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let now = (
        repository.name.to_string(),
        repository.url.clone(),
        repository.private,
        repository.archived,
    );
    if stored.as_ref() == Some(&now) {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO repositories (id, full_name, url, private, archived)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (id) DO UPDATE SET
           full_name = excluded.full_name, url = excluded.url, private = excluded.private,
           archived = excluded.archived",
        params![id, now.0, now.1, now.2, now.3],
    )?;
    Ok(true)
}

/// What [`link`] and [`store_and_link`] did.
#[derive(Debug)]
pub struct Linked {
    /// The Repository, with every Workspace linking it now.
    pub repository: RepositoryLinks,
    /// Whether anything changed: the Link is new, or the Repository was stored or updated.
    pub changed: bool,
}

/// The id of the Repository GitHub calls `name` now, as last read, ignoring case as GitHub
/// does; `None` when agentdeck doesn't know one by that name.
pub fn by_name(conn: &Connection, name: &RepositoryName) -> Result<Option<RepositoryId>, DbError> {
    Ok(conn
        .query_row(
            "SELECT id FROM repositories WHERE full_name = ?1 COLLATE NOCASE ORDER BY id LIMIT 1",
            [name.to_string()],
            |row| row.get::<_, u64>(0),
        )
        .optional()?
        .map(RepositoryId::from))
}

/// Whether Repository `id` is stored.
pub fn is_known(conn: &Connection, id: RepositoryId) -> Result<bool, DbError> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM repositories WHERE id = ?1)",
        [id.get()],
        |row| row.get(0),
    )?)
}

/// Every Repository, each with the Workspaces linking it, by name ignoring case, then id;
/// only those `workspace` links when it's set. A `workspace` that doesn't exist is
/// [`DbError::UnknownReference`].
pub fn list(
    conn: &Connection,
    workspace: Option<WorkspaceId>,
) -> Result<Vec<RepositoryLinks>, DbError> {
    let ids: Vec<u64> = match workspace {
        Some(workspace) => {
            if !super::exists(conn, "workspaces", workspace)? {
                return Err(DbError::UnknownReference("workspace"));
            }
            let mut stmt = conn.prepare(
                "SELECT r.id FROM repositories r JOIN links l ON l.repository_id = r.id
                 WHERE l.workspace_id = ?1 ORDER BY r.full_name COLLATE NOCASE, r.id",
            )?;
            let rows = stmt.query_map([workspace.to_string()], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        }
        None => {
            let mut stmt =
                conn.prepare("SELECT id FROM repositories ORDER BY full_name COLLATE NOCASE, id")?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        }
    };
    ids.into_iter()
        .map(|id| get(conn, RepositoryId::from(id)))
        .collect()
}

/// Repository `id` with the Workspaces linking it, oldest first.
pub fn get(conn: &Connection, id: RepositoryId) -> Result<RepositoryLinks, DbError> {
    let repository = conn
        .query_row(
            "SELECT full_name, url, private, archived, unreachable_at IS NOT NULL
             FROM repositories WHERE id = ?1",
            [id.get()],
            |row| {
                Ok(RepositoryLinks {
                    id,
                    name: super::text(row, "full_name")?,
                    url: row.get(1)?,
                    private: row.get(2)?,
                    archived: row.get(3)?,
                    unreachable: row.get(4)?,
                    workspaces: Vec::new(),
                })
            },
        )
        .optional()?
        .ok_or(DbError::NotFound("repository"))?;
    let mut stmt = conn.prepare(
        "SELECT l.workspace_id FROM links l JOIN workspaces w ON w.id = l.workspace_id
         WHERE l.repository_id = ?1 ORDER BY w.created_at, w.id",
    )?;
    let rows = stmt.query_map([id.get()], |row| super::text(row, "workspace_id"))?;
    Ok(RepositoryLinks {
        workspaces: rows.collect::<rusqlite::Result<_>>()?,
        ..repository
    })
}

/// Links Workspace `workspace` to Repository `id`, which agentdeck knows. Linking a pair
/// already linked changes nothing. A Workspace that doesn't exist is [`DbError::NotFound`];
/// a Repository that doesn't, [`DbError::UnknownReference`].
pub fn link(
    conn: &mut Connection,
    workspace: WorkspaceId,
    id: RepositoryId,
) -> Result<Linked, DbError> {
    let tx = conn.transaction()?;
    if !super::exists(&tx, "workspaces", workspace)? {
        return Err(DbError::NotFound("workspace"));
    }
    if !is_known(&tx, id)? {
        return Err(DbError::UnknownReference("repository"));
    }
    let linked = insert_link(&tx, workspace, id, false)?;
    tx.commit()?;
    Ok(linked)
}

/// Stores `repository` as GitHub reports it now ([`store`]) and links Workspace `workspace`
/// to it, in one transaction. A Workspace that doesn't exist is [`DbError::NotFound`], and
/// nothing is stored.
pub fn store_and_link(
    conn: &mut Connection,
    workspace: WorkspaceId,
    repository: &RemoteRepository,
) -> Result<Linked, DbError> {
    let tx = conn.transaction()?;
    if !super::exists(&tx, "workspaces", workspace)? {
        return Err(DbError::NotFound("workspace"));
    }
    let stored = store(&tx, repository)?;
    let linked = insert_link(&tx, workspace, repository.id, stored)?;
    tx.commit()?;
    Ok(linked)
}

/// Adds the Link, unless it's there; `changed` is whether it was added or `stored` is.
fn insert_link(
    conn: &Connection,
    workspace: WorkspaceId,
    id: RepositoryId,
    stored: bool,
) -> Result<Linked, DbError> {
    let added = add_link(conn, workspace, id)?;
    Ok(Linked {
        repository: get(conn, id)?,
        changed: added || stored,
    })
}

/// Adds the Link between `workspace` and Repository `id`, both stored, unless it's there;
/// whether it was added.
pub fn add_link(
    conn: &Connection,
    workspace: WorkspaceId,
    id: RepositoryId,
) -> Result<bool, DbError> {
    let added = conn.execute(
        "INSERT OR IGNORE INTO links (workspace_id, repository_id) VALUES (?1, ?2)",
        params![workspace.to_string(), id.get()],
    )?;
    Ok(added > 0)
}

/// Removes the Link between `workspace` and Repository `id`, if there is one, keeping the
/// Repository; whether there was one.
pub fn remove_link(
    conn: &Connection,
    workspace: WorkspaceId,
    id: RepositoryId,
) -> Result<bool, DbError> {
    let removed = conn.execute(
        "DELETE FROM links WHERE workspace_id = ?1 AND repository_id = ?2",
        params![workspace.to_string(), id.get()],
    )?;
    Ok(removed > 0)
}

/// Unlinks Workspace `workspace` from Repository `id`, then deletes the Repository when no
/// other Link and no kept pull request names it, in one transaction. Returns whether the
/// Repository was deleted. A Workspace that doesn't exist, or a pair that isn't linked, is
/// [`DbError::NotFound`].
pub fn unlink(
    conn: &mut Connection,
    workspace: WorkspaceId,
    id: RepositoryId,
) -> Result<bool, DbError> {
    let tx = conn.transaction()?;
    if !super::exists(&tx, "workspaces", workspace)? {
        return Err(DbError::NotFound("workspace"));
    }
    if !remove_link(&tx, workspace, id)? {
        return Err(DbError::NotFound("link"));
    }
    let deleted = forget_if_unreferenced(&tx, id)?;
    tx.commit()?;
    Ok(deleted)
}

/// The Repositories Workspace `workspace` links.
pub fn linked_by(conn: &Connection, workspace: WorkspaceId) -> Result<Vec<RepositoryId>, DbError> {
    let mut stmt =
        conn.prepare("SELECT repository_id FROM links WHERE workspace_id = ?1 ORDER BY 1")?;
    let rows = stmt.query_map([workspace.to_string()], |row| row.get::<_, u64>(0))?;
    Ok(rows
        .map(|id| id.map(RepositoryId::from))
        .collect::<rusqlite::Result<_>>()?)
}

/// Deletes Repository `id` when no Link and no kept pull request names it (spec §3), and
/// returns whether it did.
pub fn forget_if_unreferenced(conn: &Connection, id: RepositoryId) -> Result<bool, DbError> {
    let deleted = conn.execute(
        "DELETE FROM repositories WHERE id = ?1
           AND NOT EXISTS (SELECT 1 FROM links WHERE repository_id = ?1)
           AND NOT EXISTS (SELECT 1 FROM pull_requests WHERE repository_id = ?1)",
        [id.get()],
    )?;
    Ok(deleted > 0)
}
