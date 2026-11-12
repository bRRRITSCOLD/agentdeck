//! `workspaces` queries.

use std::path::{Path, PathBuf};

use agentdeck_core::WorkspaceId;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{DbError, new_id, now, text};
use crate::model::{Workspace, WorkspacePatch};

const COLUMNS: &str = "id, slug, name, dir, dir_owned, muted, archived_at, created_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: text(row, "id")?,
        slug: row.get("slug")?,
        name: row.get("name")?,
        dir: row.get("dir")?,
        dir_owned: row.get("dir_owned")?,
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

/// Creates a workspace named `name`. With `dir`, attaches that dir; without, creates a
/// scratch dir `<scratch_root>/<slug>`. The slug is the name's, made unique among the
/// workspaces and, for a scratch dir, among the dirs already under `scratch_root`.
pub fn create(
    conn: &Connection,
    name: &str,
    dir: Option<&Path>,
    scratch_root: &Path,
) -> Result<Workspace, DbError> {
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
        if !taken && (dir.is_some() || !scratch_root.join(&slug).exists()) {
            break slug;
        }
        n += 1;
    };

    let (dir, dir_owned): (PathBuf, bool) = match dir {
        Some(dir) => (dir.to_path_buf(), false),
        None => {
            let dir = scratch_root.join(&slug);
            std::fs::create_dir_all(&dir)?;
            (dir, true)
        }
    };
    let id: WorkspaceId = new_id();
    conn.execute(
        "INSERT INTO workspaces (id, slug, name, dir, dir_owned, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id.to_string(),
            slug,
            name,
            dir.display().to_string(),
            dir_owned,
            now()
        ],
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

/// Archives (stamping now) or unarchives. Archiving an archived workspace keeps its stamp.
pub fn set_archived(
    conn: &Connection,
    id: WorkspaceId,
    archived: bool,
) -> Result<Workspace, DbError> {
    get(conn, id)?;
    if archived {
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

/// Deletes the row. Its tabs, panes and notes go with it; its tasks move to the inbox.
pub fn delete(conn: &Connection, id: WorkspaceId) -> Result<(), DbError> {
    match conn.execute("DELETE FROM workspaces WHERE id = ?1", [id.to_string()])? {
        0 => Err(DbError::NotFound("workspace")),
        _ => Ok(()),
    }
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
