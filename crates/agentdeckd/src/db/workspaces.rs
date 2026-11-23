//! `workspaces` queries.

use agentdeck_core::{RepositoryId, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{DbError, new_id, now, repositories, text};
use crate::model::{Workspace, WorkspacePatch};

const COLUMNS: &str = "id, slug, name, muted, archived_at, created_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: text(row, "id")?,
        slug: row.get("slug")?,
        name: row.get("name")?,
        muted: row.get("muted")?,
        archived_at: row.get("archived_at")?,
        created_at: row.get("created_at")?,
    })
}

/// Every workspace, oldest first.
pub fn list(conn: &Connection) -> Result<Vec<Workspace>, DbError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM workspaces ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One workspace.
pub fn get(conn: &Connection, id: WorkspaceId) -> Result<Workspace, DbError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM workspaces WHERE id = ?1"),
        [id.to_string()],
        from_row,
    )
    .optional()?
    .ok_or(DbError::NotFound("workspace"))
}

/// Creates a workspace named `name`, slugged from its name and made unique among the
/// workspaces.
pub fn create(conn: &Connection, name: &str) -> Result<Workspace, DbError> {
    let base = slugify(name);
    let mut n = 1;
    let slug = loop {
        let slug = if n == 1 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        let taken: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM workspaces WHERE slug = ?1)",
            [&slug],
            |row| row.get(0),
        )?;
        if !taken {
            break slug;
        }
        n += 1;
    };
    let id: WorkspaceId = new_id();
    conn.execute(
        "INSERT INTO workspaces (id, slug, name, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![id.to_string(), slug, name, now()],
    )?;
    get(conn, id)
}

/// Applies the fields `patch` sets.
pub fn update(
    conn: &Connection,
    id: WorkspaceId,
    patch: &WorkspacePatch,
) -> Result<Workspace, DbError> {
    get(conn, id)?;
    if let Some(name) = &patch.name {
        conn.execute(
            "UPDATE workspaces SET name = ?2 WHERE id = ?1",
            params![id.to_string(), name],
        )?;
    }
    if let Some(muted) = patch.muted {
        conn.execute(
            "UPDATE workspaces SET muted = ?2 WHERE id = ?1",
            params![id.to_string(), muted],
        )?;
    }
    get(conn, id)
}

/// Archives (stamping now) or unarchives. Archiving an archived workspace keeps its stamp, and
/// archiving is refused (`panes_running`) while any of its panes hasn't exited.
pub fn set_archived(
    conn: &Connection,
    id: WorkspaceId,
    archived: bool,
) -> Result<Workspace, DbError> {
    get(conn, id)?;
    if archived {
        let running: i64 = conn.query_row(
            "SELECT COUNT(*) FROM panes p JOIN tabs t ON t.id = p.tab_id
             WHERE t.workspace_id = ?1 AND p.state <> 'exited'",
            [id.to_string()],
            |row| row.get(0),
        )?;
        if running > 0 {
            let panes = if running == 1 { "pane" } else { "panes" };
            return Err(DbError::Conflict {
                code: "panes_running",
                message: format!("Close its {running} running {panes} first"),
            });
        }
        conn.execute(
            "UPDATE workspaces SET archived_at = COALESCE(archived_at, ?2) WHERE id = ?1",
            params![id.to_string(), now()],
        )?;
    } else {
        conn.execute(
            "UPDATE workspaces SET archived_at = NULL WHERE id = ?1",
            [id.to_string()],
        )?;
    }
    get(conn, id)
}

/// Deletes the row. Its tabs, panes and Links go with it, and so does each
/// Repository it linked that nothing else names now (spec §3).
/// Returns the Repositories it linked.
pub fn delete(conn: &mut Connection, id: WorkspaceId) -> Result<Vec<RepositoryId>, DbError> {
    let tx = conn.transaction()?;
    let linked = repositories::linked_by(&tx, id)?;
    if tx.execute("DELETE FROM workspaces WHERE id = ?1", [id.to_string()])? == 0 {
        return Err(DbError::NotFound("workspace"));
    }
    for repository in &linked {
        repositories::forget_if_unreferenced(&tx, *repository)?;
    }
    tx.commit()?;
    Ok(linked)
}

/// A slug from a name: lowercase ASCII letters and digits, with every other run of
/// characters turned into one `-`. A name with neither gives `workspace`.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "workspace".to_owned()
    } else {
        slug.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod slugify {
        use super::*;

        #[test]
        fn should_lowercase_and_hyphenate() {
            assert_eq!(slugify("My Big Project"), "my-big-project");
        }

        #[test]
        fn should_collapse_and_trim_punctuation() {
            assert_eq!(slugify("  fix: the -- bug!! "), "fix-the-bug");
        }

        #[test]
        fn should_drop_non_ascii_letters() {
            assert_eq!(slugify("café ✻ 2"), "caf-2");
        }

        #[test]
        fn should_fall_back_when_nothing_is_left() {
            assert_eq!(slugify("✻✻"), "workspace");
        }
    }
}
