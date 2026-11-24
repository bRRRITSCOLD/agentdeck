//! `pull_requests` queries: storing what PR sync saw, with each PR's Repository (spec §8);
//! listing the PRs, and finding them by reference (spec §4).
//!
//! A PR row is keyed by its Repository's GitHub id and its number ([`PrKey`]), so a renamed
//! Repository's PRs stay the same PRs, and another Repository taking its old name never
//! collides with them. A PR's name is its Repository's name now.

use agentdeck_core::{PrRef, PrState, RepositoryId, Yours};
use chrono::{DateTime, Utc};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use super::{DbError, now, opt_text, repositories, text};
use crate::model::{PrFilter, PullRequest, Repository};
use crate::prsync::github::{PrKey, RemotePr, RemoteRepository};

/// Every column a stored PR is read back from, its Repository's included, for [`remote`].
const STORED: &str = "p.number, p.title, p.url, p.state, p.draft, p.head_ref, p.base_ref,
    p.author, p.authored, p.review_requested, p.assigned, p.checks, p.review, p.updated_at,
    p.unreachable_at IS NOT NULL AS unreachable,
    p.finished_at IS NOT NULL AS finished, r.id AS repository_id, r.full_name,
    r.url AS repository_url, r.private, r.archived";

/// Every PR stored as open, by key.
pub fn open(conn: &Connection) -> Result<Vec<PrKey>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT repository_id, number FROM pull_requests WHERE state = ?1
         ORDER BY repository_id, number",
    )?;
    let rows = stmt.query_map([PrState::Open.as_str()], key)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The kept PRs, by Repository name, ignoring case, then number; only those of
/// `filter.repository`, by the name GitHub gives it now, when it's set, and only those of the
/// Repositories `filter.workspace` links, when that's set. A workspace that doesn't exist is
/// [`DbError::UnknownReference`]. Finished PRs are left out unless `filter.finished` asks for
/// them.
pub fn list(conn: &Connection, filter: &PrFilter) -> Result<Vec<PullRequest>, DbError> {
    let mut sql = format!(
        "SELECT {STORED} FROM pull_requests p JOIN repositories r ON r.id = p.repository_id
         WHERE 1 = 1"
    );
    if !filter.finished.unwrap_or(false) {
        sql.push_str(" AND p.finished_at IS NULL");
    }
    let mut values: Vec<Value> = Vec::new();
    if let Some(repository) = &filter.repository {
        values.push(Value::Text(repository.to_string()));
        sql.push_str(&format!(
            " AND r.full_name = ?{} COLLATE NOCASE",
            values.len()
        ));
    }
    if let Some(workspace) = filter.workspace {
        if !super::exists(conn, "workspaces", workspace)? {
            return Err(DbError::UnknownReference("workspace"));
        }
        values.push(Value::Text(workspace.to_string()));
        sql.push_str(&format!(
            " AND r.id IN (SELECT repository_id FROM links WHERE workspace_id = ?{})",
            values.len()
        ));
    }
    sql.push_str(" ORDER BY r.full_name COLLATE NOCASE, r.id, p.number");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(values), listed)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One row of [`list`].
fn listed(row: &Row<'_>) -> rusqlite::Result<PullRequest> {
    let repository = Repository {
        id: RepositoryId::from(row.get::<_, u64>("repository_id")?),
        name: text(row, "full_name")?,
        url: row.get("repository_url")?,
        private: row.get("private")?,
        archived: row.get("archived")?,
    };
    let number: u32 = row.get("number")?;
    Ok(PullRequest {
        reference: PrRef::new(repository.name.clone(), number),
        number,
        repository,
        title: row.get("title")?,
        url: row.get("url")?,
        state: text(row, "state")?,
        draft: row.get("draft")?,
        head_ref: row.get("head_ref")?,
        base_ref: row.get("base_ref")?,
        author: row.get("author")?,
        yours: yours(row)?,
        checks: opt_text(row, "checks")?,
        review: opt_text(row, "review")?,
        updated_at: row.get("updated_at")?,
        unreachable: row.get("unreachable")?,
        finished: row.get("finished")?,
    })
}

/// Why a PR row is yours, in [`Yours::ALL`]'s order, from its `authored`, `review_requested`
/// and `assigned` columns.
fn yours(row: &Row<'_>) -> rusqlite::Result<Vec<Yours>> {
    let mut yours = Vec::new();
    for (column, why) in [
        ("authored", Yours::Authored),
        ("review_requested", Yours::ReviewRequested),
        ("assigned", Yours::Assigned),
    ] {
        if row.get(column)? {
            yours.push(why);
        }
    }
    Ok(yours)
}

/// Every PR stored as open, as last synced, but for those Finished or unreachable.
pub fn open_current(conn: &Connection) -> Result<Vec<RemotePr>, DbError> {
    stored_where(
        conn,
        "p.state = ?1 AND p.finished_at IS NULL AND p.unreachable_at IS NULL",
    )
}

/// Every PR stored as open, as last synced.
pub fn open_prs(conn: &Connection) -> Result<Vec<RemotePr>, DbError> {
    stored_where(conn, "p.state = ?1")
}

/// Finishes each stored open PR of `prs` (ones GitHub hasn't updated within the cutoff), in
/// one transaction. One already Finished is left as it is, and so is one the GitHub sign-in
/// can't see, which gets no Finished window. Returns the PRs that changed.
pub fn finish_aged(conn: &mut Connection, prs: &[PrKey]) -> Result<Vec<PrKey>, DbError> {
    let tx = conn.transaction()?;
    let finished_at = now();
    let mut changed = Vec::new();
    for pr in prs {
        let marked = tx.execute(
            "UPDATE pull_requests SET finished_at = ?3
             WHERE repository_id = ?1 AND number = ?2 AND state = ?4 AND finished_at IS NULL
               AND unreachable_at IS NULL",
            params![
                pr.repository.get(),
                pr.number,
                finished_at,
                PrState::Open.as_str()
            ],
        )?;
        if marked > 0 {
            changed.push(*pr);
        }
    }
    tx.commit()?;
    Ok(changed)
}

/// What [`prune`] deleted.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pruned {
    /// The PRs.
    pub prs: Vec<PrKey>,
    /// The Repositories nothing references any more.
    pub repositories: Vec<RepositoryId>,
}

/// Deletes each PR Finished at or before `before`, then each of their Repositories nothing
/// references any more, in one transaction (spec §8). A PR the GitHub sign-in can't see gets
/// no window: it is never deleted.
pub fn prune(conn: &mut Connection, before: DateTime<Utc>) -> Result<Pruned, DbError> {
    let tx = conn.transaction()?;
    let prs: Vec<PrKey> = {
        let mut stmt = tx.prepare(
            "SELECT repository_id, number FROM pull_requests
             WHERE finished_at IS NOT NULL AND finished_at <= ?1 AND unreachable_at IS NULL
             ORDER BY repository_id, number",
        )?;
        let rows = stmt.query_map([super::stamp(before)], key)?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut pruned = Pruned::default();
    for pr in prs {
        tx.execute(
            "DELETE FROM pull_requests WHERE repository_id = ?1 AND number = ?2",
            params![pr.repository.get(), pr.number],
        )?;
        pruned.prs.push(pr);
    }
    for pr in &pruned.prs {
        if !pruned.repositories.contains(&pr.repository)
            && repositories::forget_if_unreferenced(&tx, pr.repository)?
        {
            pruned.repositories.push(pr.repository);
        }
    }
    tx.commit()?;
    Ok(pruned)
}

/// A stored PR that can be closed: its key, and `owner/name` and number as last synced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closable {
    /// What names it for good.
    pub key: PrKey,
    /// `owner/name`, as GitHub called its Repository when last synced.
    pub repo: String,
    /// Its number.
    pub number: i64,
}

/// Each of `prs`, in order, while it is stored open, Finished or not, and the GitHub sign-in
/// can see it; `None` for a PR stored in another state, or not stored. A reference matches by
/// its Repository's name as last synced, ignoring case, as GitHub does.
pub fn closable(conn: &Connection, prs: &[PrRef]) -> Result<Vec<Option<Closable>>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT p.repository_id, p.number, r.full_name
         FROM pull_requests p JOIN repositories r ON r.id = p.repository_id
         WHERE r.full_name = ?1 COLLATE NOCASE AND p.number = ?2 AND p.state = ?3
           AND p.unreachable_at IS NULL
         ORDER BY r.id LIMIT 1",
    )?;
    prs.iter()
        .map(|pr| {
            let found = stmt.query_row(
                params![
                    pr.repository().to_string(),
                    pr.number(),
                    PrState::Open.as_str()
                ],
                |row| {
                    Ok(Closable {
                        key: key(row)?,
                        repo: row.get("full_name")?,
                        number: row.get("number")?,
                    })
                },
            );
            Ok(found.optional()?)
        })
        .collect()
}

/// Stores PR `pr` as closed, as GitHub closed it, and Finished from now unless it already was,
/// so no sync fetches it by number again. Returns whether anything changed: `false` when it
/// was no longer stored open.
pub fn close(conn: &Connection, pr: PrKey) -> Result<bool, DbError> {
    let closed = conn.execute(
        "UPDATE pull_requests SET state = ?3, synced_at = ?4,
           finished_at = COALESCE(finished_at, ?4), unreachable_at = NULL
         WHERE repository_id = ?1 AND number = ?2 AND state = ?5",
        params![
            pr.repository.get(),
            pr.number,
            PrState::Closed.as_str(),
            now(),
            PrState::Open.as_str()
        ],
    )?;
    Ok(closed > 0)
}

/// The GitHub login the last good PR sync ran as, if one was ever stored.
pub fn synced_as(conn: &Connection) -> Result<Option<String>, DbError> {
    Ok(conn
        .query_row("SELECT synced_as FROM pr_sync WHERE id = 1", [], |row| {
            row.get(0)
        })
        .optional()?)
}

/// Stores `login` as the one the last good PR sync ran as.
pub fn set_synced_as(conn: &Connection, login: &str) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO pr_sync (id, synced_as) VALUES (1, ?1)
         ON CONFLICT (id) DO UPDATE SET synced_as = excluded.synced_as",
        [login],
    )?;
    Ok(())
}

/// A PR row's key.
fn key(row: &Row<'_>) -> rusqlite::Result<PrKey> {
    Ok(PrKey {
        repository: RepositoryId::from(row.get::<_, u64>("repository_id")?),
        number: row.get("number")?,
    })
}

/// The stored PRs matching `condition`, where `?1` is the open state, as last synced.
fn stored_where(conn: &Connection, condition: &str) -> Result<Vec<RemotePr>, DbError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {STORED} FROM pull_requests p JOIN repositories r ON r.id = p.repository_id
         WHERE {condition} ORDER BY r.id, p.number"
    ))?;
    let rows = stmt.query_map([PrState::Open.as_str()], |row| Ok(remote(row)?.pr))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// A stored PR as last synced, with what PR sync marked on it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Kept {
    pr: RemotePr,
    unreachable: bool,
    finished: bool,
}

/// A row of [`STORED`] columns.
fn remote(row: &Row<'_>) -> rusqlite::Result<Kept> {
    Ok(Kept {
        pr: RemotePr {
            repository: RemoteRepository {
                id: RepositoryId::from(row.get::<_, u64>("repository_id")?),
                name: text(row, "full_name")?,
                url: row.get("repository_url")?,
                private: row.get("private")?,
                archived: row.get("archived")?,
            },
            number: row.get("number")?,
            title: row.get("title")?,
            url: row.get("url")?,
            state: text(row, "state")?,
            draft: row.get("draft")?,
            head_ref: row.get("head_ref")?,
            base_ref: row.get("base_ref")?,
            author: row.get("author")?,
            yours: yours(row)?,
            checks: opt_text(row, "checks")?,
            review: opt_text(row, "review")?,
            updated_at: row.get("updated_at")?,
        },
        unreachable: row.get("unreachable")?,
        finished: row.get("finished")?,
    })
}

/// PR `pr` as stored now; `None` when it isn't.
fn kept(conn: &Connection, pr: PrKey) -> Result<Option<Kept>, DbError> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {STORED} FROM pull_requests p
                 JOIN repositories r ON r.id = p.repository_id
                 WHERE p.repository_id = ?1 AND p.number = ?2"
            ),
            params![pr.repository.get(), pr.number],
            remote,
        )
        .optional()?)
}

/// What [`store`] changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Stored {
    /// The PRs stored for the first time, or changed.
    pub prs: Vec<PrKey>,
    /// The Repositories created, renamed or otherwise changed, each once.
    pub repositories: Vec<RepositoryId>,
}

/// Stores each PR with its Repository, and marks each `unreachable` PR (one GitHub doesn't show
/// to the token) as such, in one transaction. Returns what changed.
///
/// A PR is found by its Repository's id and number, so a renamed Repository's PR is still the
/// same one, and another Repository taking the old name is another PR.
pub fn store(
    conn: &mut Connection,
    prs: &[RemotePr],
    unreachable: &[PrKey],
) -> Result<Stored, DbError> {
    let tx = conn.transaction()?;
    let synced_at = now();
    let mut changed = Stored::default();
    for pr in prs {
        let repository = &pr.repository;
        if repositories::store(&tx, repository)? && !changed.repositories.contains(&repository.id) {
            changed.repositories.push(repository.id);
        }
        let before = kept(&tx, pr.key())?;
        write_row(&tx, pr, &synced_at)?;
        if kept(&tx, pr.key())? != before && !changed.prs.contains(&pr.key()) {
            changed.prs.push(pr.key());
        }
    }
    for pr in unreachable {
        let marked = tx.execute(
            "UPDATE pull_requests SET unreachable_at = ?3
             WHERE repository_id = ?1 AND number = ?2 AND unreachable_at IS NULL",
            params![pr.repository.get(), pr.number, synced_at],
        )?;
        if marked > 0 && !changed.prs.contains(pr) {
            changed.prs.push(*pr);
        }
    }
    tx.commit()?;
    Ok(changed)
}

/// Inserts or replaces `pr`'s row, which is no longer unreachable. GitHub showed it within
/// the cutoff, so it is Finished while merged or closed, or open but yours no way (spec §8),
/// from the first time it was stored so; not while open and yours.
fn write_row(conn: &Connection, pr: &RemotePr, synced_at: &str) -> Result<(), DbError> {
    let has = |why: Yours| pr.yours.contains(&why);
    let kept = pr.state == PrState::Open && !pr.yours.is_empty();
    conn.execute(
        "INSERT INTO pull_requests
           (repository_id, number, title, url, state, draft, head_ref, base_ref, author, checks,
            review, updated_at, synced_at, finished_at, authored, review_requested, assigned)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 CASE WHEN ?14 THEN NULL ELSE ?13 END, ?15, ?16, ?17)
         ON CONFLICT (repository_id, number) DO UPDATE SET
           title = excluded.title, url = excluded.url, state = excluded.state,
           draft = excluded.draft, head_ref = excluded.head_ref, base_ref = excluded.base_ref,
           author = excluded.author, checks = excluded.checks, review = excluded.review,
           updated_at = excluded.updated_at, synced_at = excluded.synced_at,
           authored = excluded.authored, review_requested = excluded.review_requested,
           assigned = excluded.assigned,
           unreachable_at = NULL,
           finished_at = CASE WHEN ?14 THEN NULL
                              ELSE COALESCE(finished_at, excluded.finished_at) END",
        params![
            pr.repository.id.get(),
            pr.number,
            pr.title,
            pr.url,
            pr.state.as_str(),
            pr.draft,
            pr.head_ref,
            pr.base_ref,
            pr.author,
            pr.checks.map(|checks| checks.as_str()),
            pr.review.map(|review| review.as_str()),
            pr.updated_at,
            synced_at,
            kept,
            has(Yours::Authored),
            has(Yours::ReviewRequested),
            has(Yours::Assigned),
        ],
    )?;
    Ok(())
}
