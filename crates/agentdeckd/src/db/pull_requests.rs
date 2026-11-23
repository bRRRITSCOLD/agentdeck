//! `pull_requests` queries: storing what PR sync saw, with each PR's Repository, and the PR
//! tasks it moves (spec §8); listing the PRs, and finding them by reference (spec §4).

use agentdeck_core::{
    PrRef, PrState, RepositoryId, TaskId, TaskKind, TaskPlace, TaskStatus, WorkspaceId,
    pr_task_status,
};
use chrono::{DateTime, Utc};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use super::{DbError, now, opt_text, repositories, tasks, text};
use crate::model::{NewTask, PrFilter, PullRequest, Repository, TaskPatch, TaskPr};
use crate::prsync::github::{RemotePr, RemoteRepository};

/// A PR stored as open, by what names it: its Repository's id, once PR sync has read it, and
/// `owner/name` and number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPr {
    /// Its Repository's id; `None` for a PR stored before PR sync read Repositories.
    pub repository: Option<RepositoryId>,
    /// `owner/name`, as last synced.
    pub repo: String,
    /// Its number.
    pub number: i64,
}

impl OpenPr {
    /// Whether `pr`, as GitHub shows it now, is this PR: the same number in the same
    /// Repository, by id, so a renamed Repository's PR still is; or by name, for a PR whose
    /// Repository isn't known yet.
    pub fn is(&self, pr: &RemotePr) -> bool {
        let same_repository = match (self.repository, &pr.repository) {
            (Some(id), Some(repository)) => id == repository.id,
            _ => self.repo.eq_ignore_ascii_case(&pr.repo),
        };
        self.number == pr.number && same_repository
    }
}

/// Every PR stored as open.
pub fn open(conn: &Connection) -> Result<Vec<OpenPr>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT repository_id, repo, number FROM pull_requests WHERE state = ?1
         ORDER BY repo, number",
    )?;
    let rows = stmt.query_map([PrState::Open.as_str()], |row| {
        Ok(OpenPr {
            repository: row.get::<_, Option<u64>>(0)?.map(RepositoryId::from),
            repo: row.get(1)?,
            number: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The kept PRs whose Repository PR sync has read, by Repository name, ignoring case, then
/// number; only those of `filter.repository`, by the name GitHub gives it now, when it's set.
/// Finished PRs are left out unless `filter.finished` asks for them.
pub fn list(conn: &Connection, filter: &PrFilter) -> Result<Vec<PullRequest>, DbError> {
    let mut sql = String::from(
        "SELECT p.number, p.url, p.state, p.draft, p.head_ref, p.base_ref, p.author, p.checks,
           p.review, p.updated_at, p.unreachable_at IS NOT NULL AS unreachable,
           p.finished_at IS NOT NULL AS finished, t.title,
           r.id AS repository_id, r.full_name, r.url AS repository_url, r.private, r.archived
         FROM pull_requests p
         JOIN tasks t ON t.id = p.task_id
         JOIN repositories r ON r.id = p.repository_id
         WHERE p.base_ref IS NOT NULL",
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
        checks: opt_text(row, "checks")?,
        review: opt_text(row, "review")?,
        updated_at: row.get("updated_at")?,
        unreachable: row.get("unreachable")?,
        finished: row.get("finished")?,
    })
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

/// Every PR stored as open whose task PR sync may still place ([`sync_places`]), as last
/// synced, so it can be matched to a workspace again without asking GitHub; but for those
/// Finished or unreachable, which storing would bring back.
pub fn inbox_open(conn: &Connection) -> Result<Vec<RemotePr>, DbError> {
    stored_where(
        conn,
        "p.state = ?1 AND t.workspace_id IS NULL AND p.workspace_picked_at IS NULL
         AND p.finished_at IS NULL AND p.unreachable_at IS NULL",
    )
}

/// Whether PR sync may place a PR task in the workspace its clone match found (spec §8):
/// only while the task is in the inbox and the user hasn't picked its workspace. A user's
/// pick, the inbox included, sticks; sync never moves the task again.
fn sync_places(workspace: Option<WorkspaceId>, picked: bool) -> bool {
    workspace.is_none() && !picked
}

/// Whether the user picked task `id`'s workspace.
fn is_picked(conn: &Connection, id: TaskId) -> Result<bool, DbError> {
    Ok(conn.query_row(
        "SELECT workspace_picked_at IS NOT NULL FROM pull_requests WHERE task_id = ?1",
        [id.to_string()],
        |row| row.get(0),
    )?)
}

/// Finishes each stored open PR of `prs` (`(repo, number)`: ones GitHub hasn't updated
/// within the cutoff), in one transaction: its task moves to `done`, once. One already Finished
/// is left as it is, so a status the user picks after sticks, and so is one the GitHub
/// sign-in can't see, which gets no Finished window. Returns the tasks that changed.
pub fn finish_aged(conn: &mut Connection, prs: &[(String, i64)]) -> Result<Vec<TaskId>, DbError> {
    let tx = conn.transaction()?;
    let finished_at = now();
    let mut changed = Vec::new();
    for (repo, number) in prs {
        let marked = tx.execute(
            "UPDATE pull_requests SET finished_at = ?3
             WHERE repo = ?1 AND number = ?2 AND state = ?4 AND finished_at IS NULL
               AND unreachable_at IS NULL",
            params![repo, number, finished_at, PrState::Open.as_str()],
        )?;
        let Some(task_id) = task_for(&tx, repo, *number)?.filter(|_| marked > 0) else {
            continue;
        };
        if tasks::get(&tx, task_id)?.status == TaskStatus::Done {
            continue;
        }
        tasks::update(
            &tx,
            task_id,
            &TaskPatch {
                title: None,
                body: None,
                status: Some(TaskStatus::Done),
                workspace_id: None,
            },
        )?;
        changed.push(task_id);
    }
    tx.commit()?;
    Ok(changed)
}

/// What [`prune`] deleted.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pruned {
    /// The PRs' tasks, deleted with them.
    pub tasks: Vec<TaskId>,
    /// The Repositories nothing references any more.
    pub repositories: Vec<RepositoryId>,
}

/// Deletes each PR Finished at or before `before`, with its task, then each of their
/// Repositories nothing references any more, in one transaction (spec §8). A PR the GitHub
/// sign-in can't see gets no window: it is never deleted.
pub fn prune(conn: &mut Connection, before: DateTime<Utc>) -> Result<Pruned, DbError> {
    let tx = conn.transaction()?;
    let rows: Vec<(TaskId, Option<u64>)> = {
        let mut stmt = tx.prepare(
            "SELECT task_id, repository_id FROM pull_requests
             WHERE finished_at IS NOT NULL AND finished_at <= ?1 AND unreachable_at IS NULL
             ORDER BY task_id",
        )?;
        let rows = stmt.query_map([super::stamp(before)], |row| {
            Ok((text(row, "task_id")?, row.get(1)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut pruned = Pruned::default();
    for (task, repository) in rows {
        tasks::delete(&tx, task)?;
        pruned.tasks.push(task);
        if let Some(repository) = repository.map(RepositoryId::from)
            && !pruned.repositories.contains(&repository)
            && repositories::forget_if_unreferenced(&tx, repository)?
        {
            pruned.repositories.push(repository);
        }
    }
    tx.commit()?;
    Ok(pruned)
}

/// A stored PR that can be closed: its task, and `owner/name` and number as last synced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closable {
    /// Its PR task.
    pub task: TaskId,
    /// `owner/name`, as GitHub called its Repository when last synced.
    pub repo: String,
    /// Its number.
    pub number: i64,
}

/// Each of `prs`, in order, while it is stored open, Finished or not, and the GitHub sign-in
/// can see it; `None` for a PR stored in another state, or not stored. A reference matches by
/// the name its Repository had when last synced, ignoring case, as GitHub does.
pub fn closable(conn: &Connection, prs: &[PrRef]) -> Result<Vec<Option<Closable>>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT task_id, repo, number FROM pull_requests
         WHERE repo = ?1 COLLATE NOCASE AND number = ?2 AND state = ?3
           AND unreachable_at IS NULL",
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
                        task: text(row, "task_id")?,
                        repo: row.get("repo")?,
                        number: row.get("number")?,
                    })
                },
            );
            Ok(found.optional()?)
        })
        .collect()
}

/// Stores task `id`'s PR as closed, as GitHub closed it, and Finished from now unless it
/// already was, and moves the task to `done`, in one transaction, so no sync fetches it by
/// number again. Returns whether anything changed: `false` when it was no longer stored open.
pub fn close(conn: &mut Connection, id: TaskId) -> Result<bool, DbError> {
    let tx = conn.transaction()?;
    let closed = tx.execute(
        "UPDATE pull_requests SET state = ?2, synced_at = ?3,
           finished_at = COALESCE(finished_at, ?3), unreachable_at = NULL
         WHERE task_id = ?1 AND state = ?4",
        params![
            id.to_string(),
            PrState::Closed.as_str(),
            now(),
            PrState::Open.as_str()
        ],
    )?;
    if closed == 0 {
        return Ok(false);
    }
    // A task already done keeps its `done_at`.
    tasks::update(
        &tx,
        id,
        &TaskPatch {
            title: None,
            body: None,
            status: Some(TaskStatus::Done),
            workspace_id: None,
        },
    )?;
    tx.commit()?;
    Ok(true)
}

/// Whether task `id`'s PR is Finished.
fn is_finished(conn: &Connection, id: TaskId) -> Result<bool, DbError> {
    Ok(conn.query_row(
        "SELECT finished_at IS NOT NULL FROM pull_requests WHERE task_id = ?1",
        [id.to_string()],
        |row| row.get(0),
    )?)
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

/// The stored PRs matching `condition`, where `?1` is the open state, as last synced.
fn stored_where(conn: &Connection, condition: &str) -> Result<Vec<RemotePr>, DbError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT p.task_id FROM pull_requests p JOIN tasks t ON t.id = p.task_id
         WHERE {condition} ORDER BY p.repo, p.number"
    ))?;
    let ids = stmt
        .query_map([PrState::Open.as_str()], |row| text(row, "task_id"))?
        .collect::<rusqlite::Result<Vec<TaskId>>>()?;
    let mut prs = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(pr) = stored(conn, id)? {
            prs.push(pr);
        }
    }
    Ok(prs)
}

/// Task `id`'s PR as last synced; `None` for a plain task.
fn stored(conn: &Connection, id: TaskId) -> Result<Option<RemotePr>, DbError> {
    let task = tasks::get(conn, id)?;
    let Some(pr) = task.pr else {
        return Ok(None);
    };
    let more = more_of(conn, id)?;
    Ok(Some(RemotePr {
        repo: pr.repo,
        repository: more.repository,
        number: pr.number,
        title: task.title,
        url: pr.url,
        state: pr.state,
        draft: pr.draft,
        head_ref: pr.head_ref,
        base_ref: more.base_ref,
        author: more.author,
        checks: pr.checks,
        review: pr.review,
        updated_at: pr.updated_at,
    }))
}

/// What a PR row holds beyond a task's [`TaskPr`]: its Repository, base branch and author.
#[derive(Debug, Clone, PartialEq, Eq)]
struct More {
    repository: Option<RemoteRepository>,
    base_ref: Option<String>,
    author: Option<String>,
}

/// [`More`] of task `id`'s PR.
fn more_of(conn: &Connection, id: TaskId) -> Result<More, DbError> {
    Ok(conn.query_row(
        "SELECT p.base_ref, p.author, r.id, r.full_name, r.url, r.private, r.archived
         FROM pull_requests p LEFT JOIN repositories r ON r.id = p.repository_id
         WHERE p.task_id = ?1",
        [id.to_string()],
        |row| {
            let repository = match row.get::<_, Option<u64>>(2)? {
                Some(repository) => Some(RemoteRepository {
                    id: RepositoryId::from(repository),
                    name: text(row, "full_name")?,
                    url: row.get(4)?,
                    private: row.get(5)?,
                    archived: row.get(6)?,
                }),
                None => None,
            };
            Ok(More {
                repository,
                base_ref: row.get(0)?,
                author: row.get(1)?,
            })
        },
    )?)
}

/// What [`store`] changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Stored {
    /// The tasks whose task or PR changed.
    pub tasks: Vec<TaskId>,
    /// The Repositories created, renamed or otherwise changed, each once.
    pub repositories: Vec<RepositoryId>,
}

/// Stores each PR with the workspace it matched, and its Repository, and marks each
/// `unreachable` PR (`(repo, number)`: one GitHub doesn't show to the token) as such, in one
/// transaction. Returns what changed.
///
/// A new PR becomes a PR task; a stored one, found by its Repository's id so a renamed
/// Repository's PR is still the same one, updates its row and its task. A PR task only gains
/// a workspace while [`sync_places`] it, so a workspace never changes under it.
pub fn store(
    conn: &mut Connection,
    prs: &[(RemotePr, Option<WorkspaceId>)],
    unreachable: &[(String, i64)],
) -> Result<Stored, DbError> {
    let tx = conn.transaction()?;
    let synced_at = now();
    let mut changed = Stored::default();
    for (pr, workspace) in prs {
        if let Some(repository) = &pr.repository
            && repositories::store(&tx, repository)?
            && !changed.repositories.contains(&repository.id)
        {
            changed.repositories.push(repository.id);
        }
        if let Some(task) = store_one(&tx, pr, *workspace, &synced_at)? {
            changed.tasks.push(task);
        }
    }
    for (repo, number) in unreachable {
        if let Some(task) = mark_unreachable(&tx, repo, *number, &synced_at)? {
            changed.tasks.push(task);
        }
    }
    tx.commit()?;
    Ok(changed)
}

/// Marks a PR GitHub doesn't show to the token as unreachable, once: it keeps its state, so
/// the next full sync fetches it again, and its task keeps its status. `Some(task)` when it
/// wasn't unreachable before.
fn mark_unreachable(
    conn: &Connection,
    repo: &str,
    number: i64,
    synced_at: &str,
) -> Result<Option<TaskId>, DbError> {
    let marked = conn.execute(
        "UPDATE pull_requests SET unreachable_at = ?3
         WHERE repo = ?1 AND number = ?2 AND unreachable_at IS NULL",
        params![repo, number, synced_at],
    )?;
    if marked == 0 {
        return Ok(None);
    }
    task_for(conn, repo, number)
}

/// Stores one PR; `Some(task)` when the task or its PR changed.
fn store_one(
    conn: &Connection,
    pr: &RemotePr,
    workspace: Option<WorkspaceId>,
    synced_at: &str,
) -> Result<Option<TaskId>, DbError> {
    let Some(task_id) = stored_task(conn, pr)? else {
        let status = pr_task_status(None, pr.state);
        let task = tasks::create_kind(
            conn,
            &NewTask {
                title: pr.title.clone(),
                body: None,
                status,
                workspace_id: workspace,
            },
            TaskKind::Pr,
        )?;
        write_row(conn, task.id, pr, synced_at)?;
        return Ok(Some(task.id));
    };

    let task = tasks::get(conn, task_id)?;
    let before = task.pr.as_ref().ok_or(DbError::NotFound("pull request"))?;
    let after = synced(pr, synced_at);
    // A Finished PR's task is done as a closed one's is, so GitHub showing it open again
    // within the cutoff moves it back to review, as a reopen does. An unreachable PR's state
    // wasn't seen, so it moves as a new PR's would: back to review while open, to done once
    // merged or closed.
    let before_state = if before.unreachable {
        None
    } else if is_finished(conn, task_id)? {
        Some(PrState::Closed)
    } else {
        Some(before.state)
    };
    let status = pr_task_status(before_state, pr.state).filter(|s| *s != task.status);
    let title = Some(pr.title.clone()).filter(|title| *title != task.title);
    let picked = is_picked(conn, task_id)?;
    let workspace = workspace.filter(|_| sync_places(task.workspace_id, picked));
    // A PR re-stored as last synced, with no Repository, leaves these as they are.
    let more = more_of(conn, task_id)?;
    let more_changed = pr.repository.is_some()
        && (more.repository.as_ref().map(|r| r.id) != pr.repository.as_ref().map(|r| r.id)
            || more.base_ref != pr.base_ref
            || more.author != pr.author);
    let pr_changed = more_changed
        || TaskPr {
            synced_at: before.synced_at.clone(),
            ..after
        } != *before;

    write_row(conn, task_id, pr, synced_at)?;
    if status.is_none() && title.is_none() && workspace.is_none() && !pr_changed {
        return Ok(None);
    }
    tasks::update(
        conn,
        task_id,
        &TaskPatch {
            title,
            body: None,
            status,
            workspace_id: workspace.map(TaskPlace::Workspace),
        },
    )?;
    Ok(Some(task_id))
}

/// The task of `pr`, if it's stored: by its Repository's id and number, so a PR whose
/// Repository GitHub renamed or transferred is still found; else, for a row stored before PR
/// sync read Repositories, by `owner/name` and number.
fn stored_task(conn: &Connection, pr: &RemotePr) -> Result<Option<TaskId>, DbError> {
    if let Some(repository) = &pr.repository {
        let by_id = conn
            .query_row(
                "SELECT task_id FROM pull_requests WHERE repository_id = ?1 AND number = ?2",
                params![repository.id.get(), pr.number],
                |row| text(row, "task_id"),
            )
            .optional()?;
        if by_id.is_some() {
            return Ok(by_id);
        }
    }
    Ok(conn
        .query_row(
            "SELECT task_id FROM pull_requests
             WHERE repository_id IS NULL AND repo = ?1 AND number = ?2",
            params![pr.repo, pr.number],
            |row| text(row, "task_id"),
        )
        .optional()?)
}

/// The task of the stored PR `repo#number`, if it's stored.
fn task_for(conn: &Connection, repo: &str, number: i64) -> Result<Option<TaskId>, DbError> {
    Ok(conn
        .query_row(
            "SELECT task_id FROM pull_requests WHERE repo = ?1 AND number = ?2",
            params![repo, number],
            |row| text(row, "task_id"),
        )
        .optional()?)
}

fn synced(pr: &RemotePr, synced_at: &str) -> TaskPr {
    TaskPr {
        repo: pr.repo.clone(),
        number: pr.number,
        url: pr.url.clone(),
        state: pr.state,
        draft: pr.draft,
        head_ref: pr.head_ref.clone(),
        checks: pr.checks,
        review: pr.review,
        updated_at: pr.updated_at.clone(),
        synced_at: synced_at.to_owned(),
        unreachable: false,
    }
}

/// Inserts or replaces `task`'s PR row, which is no longer unreachable. GitHub showed it within
/// the cutoff, so it is Finished while merged or closed, from the first time it was stored so,
/// and not while open. A PR GitHub showed with its Repository takes that Repository's name,
/// its base branch and its author; one re-stored as last synced keeps what its row holds.
fn write_row(
    conn: &Connection,
    task: TaskId,
    pr: &RemotePr,
    synced_at: &str,
) -> Result<(), DbError> {
    let read = pr.repository.is_some();
    conn.execute(
        "INSERT INTO pull_requests
           (task_id, repo, number, url, state, draft, head_ref, checks, review, updated_at,
            synced_at, repository_id, base_ref, author, finished_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 CASE WHEN ?5 = ?16 THEN NULL ELSE ?11 END)
         ON CONFLICT (task_id) DO UPDATE SET
           repo = excluded.repo, url = excluded.url, state = excluded.state,
           draft = excluded.draft, head_ref = excluded.head_ref, checks = excluded.checks,
           review = excluded.review, updated_at = excluded.updated_at,
           synced_at = excluded.synced_at, unreachable_at = NULL,
           finished_at = CASE WHEN excluded.state = ?16 THEN NULL
                              ELSE COALESCE(finished_at, excluded.finished_at) END,
           repository_id = CASE WHEN ?15 THEN excluded.repository_id ELSE repository_id END,
           base_ref = CASE WHEN ?15 THEN excluded.base_ref ELSE base_ref END,
           author = CASE WHEN ?15 THEN excluded.author ELSE author END",
        params![
            task.to_string(),
            pr.repo,
            pr.number,
            pr.url,
            pr.state.as_str(),
            pr.draft,
            pr.head_ref,
            pr.checks.map(|checks| checks.as_str()),
            pr.review.map(|review| review.as_str()),
            pr.updated_at,
            synced_at,
            pr.repository.as_ref().map(|repository| repository.id.get()),
            pr.base_ref,
            pr.author,
            read,
            PrState::Open.as_str(),
        ],
    )?;
    Ok(())
}
