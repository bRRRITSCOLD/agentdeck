//! `pull_requests` queries: storing what PR sync saw, and the PR tasks it moves (spec §8).

use agentdeck_core::{ChecksState, PrState, TaskId, TaskKind, WorkspaceId, pr_task_status};
use rusqlite::{Connection, OptionalExtension, params};

use super::{DbError, now, tasks, text};
use crate::model::{NewTask, PullRequest, TaskPatch};
use crate::prsync::github::RemotePr;

/// `(repo, number)` of every PR stored as open.
pub fn open(conn: &Connection) -> Result<Vec<(String, i64)>, DbError> {
    let mut stmt = conn
        .prepare("SELECT repo, number FROM pull_requests WHERE state = ?1 ORDER BY repo, number")?;
    let rows = stmt.query_map([PrState::Open.as_str()], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `(repo, number)` of every PR stored as open whose checks are pending.
pub fn open_pending(conn: &Connection) -> Result<Vec<(String, i64)>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT repo, number FROM pull_requests WHERE state = ?1 AND checks = ?2
         ORDER BY repo, number",
    )?;
    let rows = stmt.query_map(
        [PrState::Open.as_str(), ChecksState::Pending.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Every PR stored as open, as last synced.
pub fn open_prs(conn: &Connection) -> Result<Vec<RemotePr>, DbError> {
    stored_where(conn, "p.state = ?1")
}

/// Every PR stored as open whose task is in the inbox, as last synced, so it can be matched
/// to a workspace again without asking GitHub.
pub fn inbox_open(conn: &Connection) -> Result<Vec<RemotePr>, DbError> {
    stored_where(conn, "p.state = ?1 AND t.workspace_id IS NULL")
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
    Ok(task.pr.map(|pr| RemotePr {
        repo: pr.repo,
        number: pr.number,
        title: task.title,
        url: pr.url,
        state: pr.state,
        draft: pr.draft,
        head_ref: pr.head_ref,
        checks: pr.checks,
        review: pr.review,
        updated_at: pr.updated_at,
    }))
}

/// Stores each PR with the workspace it matched, and closes each `gone` PR (`(repo, number)`:
/// one GitHub no longer shows), in one transaction. Returns the tasks that changed.
///
/// A new PR becomes a PR task; a stored one updates its row and its task. A PR task only
/// gains a workspace while it's in the inbox, so a workspace never changes under it.
pub fn store(
    conn: &mut Connection,
    prs: &[(RemotePr, Option<WorkspaceId>)],
    gone: &[(String, i64)],
) -> Result<Vec<TaskId>, DbError> {
    let tx = conn.transaction()?;
    let synced_at = now();
    let mut changed = Vec::new();
    for (pr, workspace) in prs {
        if let Some(task) = store_one(&tx, pr, *workspace, &synced_at)? {
            changed.push(task);
        }
    }
    for (repo, number) in gone {
        if let Some(task) = close_gone(&tx, repo, *number, &synced_at)? {
            changed.push(task);
        }
    }
    tx.commit()?;
    Ok(changed)
}

/// Stores a PR GitHub no longer shows as closed, so it isn't fetched again.
fn close_gone(
    conn: &Connection,
    repo: &str,
    number: i64,
    synced_at: &str,
) -> Result<Option<TaskId>, DbError> {
    let Some(task_id) = task_for(conn, repo, number)? else {
        return Ok(None);
    };
    let Some(pr) = stored(conn, task_id)? else {
        return Ok(None);
    };
    let closed = RemotePr {
        state: PrState::Closed,
        ..pr
    };
    store_one(conn, &closed, None, synced_at)
}

/// Stores one PR; `Some(task)` when the task or its PR changed.
fn store_one(
    conn: &Connection,
    pr: &RemotePr,
    workspace: Option<WorkspaceId>,
    synced_at: &str,
) -> Result<Option<TaskId>, DbError> {
    let Some(task_id) = task_for(conn, &pr.repo, pr.number)? else {
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
    let status = pr_task_status(Some(before.state), pr.state).filter(|s| *s != task.status);
    let title = Some(pr.title.clone()).filter(|title| *title != task.title);
    let workspace = workspace.filter(|_| task.workspace_id.is_none());
    let pr_changed = PullRequest {
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
            workspace_id: workspace,
        },
    )?;
    Ok(Some(task_id))
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

fn synced(pr: &RemotePr, synced_at: &str) -> PullRequest {
    PullRequest {
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
    }
}

/// Inserts or replaces `task`'s PR row.
fn write_row(
    conn: &Connection,
    task: TaskId,
    pr: &RemotePr,
    synced_at: &str,
) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO pull_requests
           (task_id, repo, number, url, state, draft, head_ref, checks, review, updated_at, synced_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT (task_id) DO UPDATE SET
           url = excluded.url, state = excluded.state, draft = excluded.draft,
           head_ref = excluded.head_ref, checks = excluded.checks, review = excluded.review,
           updated_at = excluded.updated_at, synced_at = excluded.synced_at",
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
        ],
    )?;
    Ok(())
}
