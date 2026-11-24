//! The SQLite database: one owned connection behind a `spawn_blocking` actor (spec §2).
//!
//! Each entity's queries are plain functions over `&Connection` in its own module; handlers
//! run them on the actor with [`Db::call`].

pub mod panes;
pub mod pull_requests;
pub mod repositories;
pub mod tabs;
pub mod workspaces;

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::Context;
use rusqlite::{Connection, OpenFlags};
use rusqlite_migration::{M, Migrations};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// The migrations, in order. Embedded, and run to the latest at start. The domain reset
/// squashed every earlier one into the first; a database from before it is moved aside
/// ([`move_aside_pre_reset`]) rather than migrated.
const MIGRATION_LIST: &[M<'static>] = &[
    M::up(include_str!("migrations/0001_init.sql")),
    M::up(include_str!(
        "migrations/0002_workspaces_lose_their_folder.sql"
    )),
    M::up(include_str!(
        "migrations/0003_why_a_pull_request_is_yours.sql"
    )),
];
const MIGRATIONS: Migrations<'static> = Migrations::from_slice(MIGRATION_LIST);

/// A failed query, as a handler needs to tell them apart.
#[derive(Debug)]
pub enum DbError {
    /// The entity named in the path doesn't exist.
    NotFound(&'static str),
    /// The request refers to an entity that doesn't exist.
    UnknownReference(&'static str),
    /// The request is well-formed but not allowed, with a machine-readable code.
    Invalid {
        /// The `error_code` the response carries.
        code: &'static str,
        /// What was wrong.
        message: String,
    },
    /// The request is allowed, but not in the entity's current state (409).
    Conflict {
        /// The `error_code` the response carries.
        code: &'static str,
        /// What stands in the way.
        message: String,
    },
    /// SQLite failed.
    Sql(rusqlite::Error),
    /// The actor has stopped.
    Closed,
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::NotFound(entity) => write!(f, "{entity} not found"),
            DbError::UnknownReference(entity) => write!(f, "no such {entity}"),
            DbError::Invalid { message, .. } | DbError::Conflict { message, .. } => {
                f.write_str(message)
            }
            DbError::Sql(err) => write!(f, "database error: {err}"),
            DbError::Closed => f.write_str("the database actor has stopped"),
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(err: rusqlite::Error) -> Self {
        DbError::Sql(err)
    }
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// A handle to the actor that owns the connection. Cloning it shares the actor; the actor
/// stops when the last handle is dropped.
#[derive(Clone)]
pub struct Db {
    jobs: mpsc::UnboundedSender<Job>,
}

impl Db {
    /// Opens (creating) the database at `path`, migrates it to the latest version and starts
    /// the actor. Must be called inside a tokio runtime.
    pub fn open(path: &Path) -> anyhow::Result<Db> {
        if let Some(aside) = move_aside_pre_reset(path)? {
            tracing::warn!(
                "{} is from before the domain reset (it has a tasks table): moved it, with its \
                 WAL and SHM files, to {} and starting with an empty database",
                path.display(),
                aside.display()
            );
        }
        let mut conn =
            Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        MIGRATIONS
            .to_latest(&mut conn)
            .context("migrating the database")?;

        let (jobs, mut queue) = mpsc::unbounded_channel::<Job>();
        tokio::task::spawn_blocking(move || {
            while let Some(job) = queue.blocking_recv() {
                job(&mut conn);
            }
        });
        Ok(Db { jobs })
    }

    /// Runs `query` on the actor's connection and returns its result.
    pub async fn call<T, F>(&self, query: F) -> Result<T, DbError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, DbError> + Send + 'static,
    {
        let (reply, result) = oneshot::channel();
        let job: Job = Box::new(move |conn| {
            // The caller may have gone away (its request was dropped); nothing to do then.
            let _ = reply.send(query(conn));
        });
        self.jobs.send(job).map_err(|_| DbError::Closed)?;
        result.await.map_err(|_| DbError::Closed)?
    }
}

/// What a database from before the domain reset is renamed to, beside it (spec §2).
pub const PRE_RESET_SUFFIX: &str = ".pre-domain-reset";

/// The SQLite sidecar files that go with a database file, by suffix.
const SIDECARS: [&str; 2] = ["-wal", "-shm"];

/// Moves the database at `path` aside when it is from before the domain reset, one with a
/// `tasks` table (spec §2): renames it, with its WAL and SHM files, to
/// `<name>.pre-domain-reset` beside it, so the daemon starts with an empty database and the
/// old one stays on disk. A name already taken gets `.2`, `.3`, … after it, so nothing is
/// overwritten. Returns where it went; `None` when there is no database or it is a current
/// one. Nothing else (old scratch folders) is touched.
pub fn move_aside_pre_reset(path: &Path) -> anyhow::Result<Option<PathBuf>> {
    if !path.exists() || !has_tasks_table(path)? {
        return Ok(None);
    }
    let aside = (1..)
        .map(|n| {
            let mut name = path.as_os_str().to_owned();
            name.push(PRE_RESET_SUFFIX);
            if n > 1 {
                name.push(format!(".{n}"));
            }
            PathBuf::from(name)
        })
        .find(|aside| !aside.exists() && SIDECARS.iter().all(|s| !sidecar(aside, s).exists()))
        .expect("an unused name");
    std::fs::rename(path, &aside)
        .with_context(|| format!("moving {} to {}", path.display(), aside.display()))?;
    for suffix in SIDECARS {
        let from = sidecar(path, suffix);
        if from.exists() {
            let to = sidecar(&aside, suffix);
            std::fs::rename(&from, &to)
                .with_context(|| format!("moving {} to {}", from.display(), to.display()))?;
        }
    }
    Ok(Some(aside))
}

/// `path` with `suffix` after its file name: SQLite's name for a sidecar file.
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Whether the database at `path`, which exists, has a `tasks` table. Read-only, so it
/// neither checkpoints nor deletes the WAL; the connection closes before this returns, so its
/// files can be moved.
fn has_tasks_table(path: &Path) -> anyhow::Result<bool> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {}", path.display()))?;
    let found = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'tasks')",
            [],
            |row| row.get(0),
        )
        .with_context(|| format!("reading {}'s schema", path.display()))?;
    Ok(found)
}

/// Now, as the RFC 3339 UTC string every time column holds.
pub fn now() -> String {
    stamp(chrono::Utc::now())
}

/// `at` as the RFC 3339 UTC string every time column holds: to the microsecond, so a create
/// and an edit in the same millisecond still stamp in order. Every stamp has six fraction digits
/// and a `Z`, so stamps sort as text.
pub fn stamp(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// A new id: a UUIDv7, so ids sort by creation time.
pub fn new_id<T: From<Uuid>>() -> T {
    Uuid::now_v7().into()
}

/// Reads a TEXT column through its type's `FromStr`: an id or a column enum.
pub fn text<T>(row: &rusqlite::Row<'_>, column: &str) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let text: String = row.get(column)?;
    parse(&text)
}

/// Reads a nullable TEXT column through its type's `FromStr`.
pub fn opt_text<T>(row: &rusqlite::Row<'_>, column: &str) -> rusqlite::Result<Option<T>>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let text: Option<String> = row.get(column)?;
    text.as_deref().map(parse).transpose()
}

fn parse<T>(text: &str) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    text.parse().map_err(|err: T::Err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, err.into())
    })
}

/// Whether a row with `id` exists in `table`. `table` is always a literal from this crate.
pub fn exists(conn: &Connection, table: &str, id: impl fmt::Display) -> rusqlite::Result<bool> {
    conn.query_row(
        &format!("SELECT EXISTS (SELECT 1 FROM {table} WHERE id = ?1)"),
        [id.to_string()],
        |row| row.get(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    mod stamp {
        use super::*;

        fn at(rfc3339: &str) -> chrono::DateTime<chrono::Utc> {
            chrono::DateTime::parse_from_rfc3339(rfc3339)
                .expect("an RFC 3339 time")
                .to_utc()
        }

        #[test]
        fn should_order_two_instants_in_the_same_millisecond() {
            let first = stamp(at("2026-11-14T10:00:00.123400Z"));
            let second = stamp(at("2026-11-14T10:00:00.123401Z"));

            assert!(second > first, "{first} then {second}");
        }

        #[test]
        fn should_parse_back_to_the_same_instant() {
            let instant = at("2026-11-14T10:00:00.123456Z");

            assert_eq!(at(&stamp(instant)), instant);
        }
    }

    mod migrations {
        use super::*;

        #[test]
        fn should_be_valid() {
            MIGRATIONS.validate().unwrap();
        }
    }
}
