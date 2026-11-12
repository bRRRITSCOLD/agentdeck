//! The SQLite database: one owned connection behind a `spawn_blocking` actor (spec §2).
//!
//! Each entity's queries are plain functions over `&Connection` in its own module; handlers
//! run them on the actor with [`Db::call`].

pub mod notes;
pub mod tabs;
pub mod tasks;
pub mod workspaces;

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use anyhow::Context;
use rusqlite::Connection;
use rusqlite_migration::{M, Migrations};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// The migrations, in order. Embedded, and run to the latest at start.
const MIGRATION_LIST: &[M<'static>] = &[M::up(include_str!("migrations/0001_init.sql"))];
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
    /// SQLite failed.
    Sql(rusqlite::Error),
    /// Something outside SQLite failed, such as creating a scratch dir.
    Io(std::io::Error),
    /// The actor has stopped.
    Closed,
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::NotFound(entity) => write!(f, "{entity} not found"),
            DbError::UnknownReference(entity) => write!(f, "no such {entity}"),
            DbError::Invalid { message, .. } => f.write_str(message),
            DbError::Sql(err) => write!(f, "database error: {err}"),
            DbError::Io(err) => write!(f, "io error: {err}"),
            DbError::Closed => f.write_str("the database actor has stopped"),
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(err: rusqlite::Error) -> Self {
        DbError::Sql(err)
    }
}

impl From<std::io::Error> for DbError {
    fn from(err: std::io::Error) -> Self {
        DbError::Io(err)
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

/// Now, as the RFC 3339 UTC string every time column holds.
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
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

    mod migrations {
        use super::*;

        #[test]
        fn should_be_valid() {
            MIGRATIONS.validate().unwrap();
        }
    }
}
