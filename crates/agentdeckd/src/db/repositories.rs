//! `repositories` queries: the Repository of each pull request PR sync reads (spec §3, §8).
//! A Repository is kept while a kept pull request belongs to it.

use agentdeck_core::RepositoryId;
use rusqlite::{Connection, OptionalExtension, params};

use super::DbError;
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

/// Deletes Repository `id` when no kept pull request names it. Returns whether it was deleted.
pub fn forget_if_unreferenced(conn: &Connection, id: RepositoryId) -> Result<bool, DbError> {
    let deleted = conn.execute(
        "DELETE FROM repositories WHERE id = ?1
           AND NOT EXISTS (SELECT 1 FROM pull_requests WHERE repository_id = ?1)",
        [id.get()],
    )?;
    Ok(deleted > 0)
}
