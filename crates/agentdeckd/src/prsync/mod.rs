//! PR sync (spec §8): the viewer's open GitHub PRs become PR tasks, every 120 s and on demand.
//!
//! The first sync, and one every [`FULL_SYNC_VAR`] seconds (an hour by default), is full: it
//! searches every open PR and fetches the stored ones that left the search. The rest are
//! incremental: they search only the PRs updated since the last good sync began, in any
//! state, then refresh pending checks (a finished check run doesn't update a PR) and match
//! inbox PRs to workspaces again from what's stored.
//!
//! - [`token`]: the token, from `gh auth token`.
//! - [`github`]: the GraphQL search and the fetch by number.
//! - [`matching`]: which workspace holds a clone of a PR's repo with its branch.
//! - `db::pull_requests`: storing the PRs and moving their tasks.
//!
//! One sync runs at a time: the timer and `POST /v1/prs/sync` queue behind each other.

pub mod github;
pub mod matching;
pub mod token;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use agentdeck_core::ws::Entity;
use agentdeck_core::{TaskId, WorkspaceId};
use chrono::{DateTime, SecondsFormat, Utc};
use tokio::time::MissedTickBehavior;

use crate::db::{self, Db};
use crate::events::Events;
use crate::model::{GhLogin, PrSyncStatus};
use github::{Github, GithubError, RemotePr};
use matching::Clones;

/// How often the timer syncs.
const INTERVAL: Duration = Duration::from_secs(120);

/// The environment variable that points PR sync at another GraphQL endpoint (tests use it).
pub const ENDPOINT_VAR: &str = "AGENTDECK_GITHUB_API";

/// The environment variable holding how many seconds pass between full syncs; `0` makes every
/// sync full (tests use it).
pub const FULL_SYNC_VAR: &str = "AGENTDECK_PR_FULL_SYNC_SECS";

/// How often a sync is full, unless [`FULL_SYNC_VAR`] says otherwise.
pub const FULL_SYNC_EVERY: Duration = Duration::from_secs(60 * 60);

/// How far before the last good sync's start an incremental search reaches, for clock skew
/// and GitHub's search index lagging behind its writes.
const OVERLAP: chrono::TimeDelta = chrono::TimeDelta::minutes(10);

/// A handle to PR sync; clones share it.
#[derive(Clone)]
pub struct PrSync {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    /// Gets a `changed` event for each task a sync creates or changes.
    events: Events,
    github: Github,
    /// How often a sync is full.
    full_every: Duration,
    state: Mutex<State>,
    /// Held for the length of a sync, so syncs never overlap.
    running: tokio::sync::Mutex<()>,
}

struct State {
    /// The token `gh` last printed; `None` until it prints one GitHub hasn't rejected.
    token: Option<String>,
    status: PrSyncStatus,
    /// When the last good sync began; `None` until one has finished.
    last_good: Option<DateTime<Utc>>,
    /// When the last good full sync began.
    last_full: Option<DateTime<Utc>>,
}

/// What a good sync did.
struct Synced {
    /// The tasks it created or changed.
    changed: Vec<TaskId>,
    /// When it began.
    began: DateTime<Utc>,
    /// Whether it was full.
    full: bool,
}

/// Why a sync failed.
#[derive(Debug, thiserror::Error)]
enum SyncError {
    #[error(transparent)]
    NoToken(#[from] token::NoToken),
    #[error(transparent)]
    Github(#[from] GithubError),
    #[error("{0}")]
    Db(db::DbError),
    #[error("matching PRs to workspaces: {0}")]
    Matching(#[from] tokio::task::JoinError),
}

impl From<db::DbError> for SyncError {
    fn from(err: db::DbError) -> Self {
        SyncError::Db(err)
    }
}

impl PrSync {
    /// Starts the timer, whose first sync runs now and reads the token with `gh auth token`
    /// (on the timer's task, so a slow `gh` never holds up the daemon's start). `endpoint` is
    /// the GraphQL URL; a sync is full every `full_every`. Must be called inside a tokio
    /// runtime.
    pub fn start(
        db: Db,
        events: Events,
        endpoint: String,
        full_every: Duration,
    ) -> anyhow::Result<PrSync> {
        let sync = PrSync {
            inner: Arc::new(Inner {
                db,
                events,
                github: Github::new(endpoint)?,
                full_every,
                state: Mutex::new(State {
                    token: None,
                    status: PrSyncStatus {
                        login: GhLogin::LoggedOut,
                        last_synced_at: None,
                        last_error: None,
                    },
                    last_good: None,
                    last_full: None,
                }),
                running: tokio::sync::Mutex::new(()),
            }),
        };
        let timer = sync.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(INTERVAL);
            interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                timer.sync().await;
            }
        });
        Ok(sync)
    }

    /// The `gh` login and the last sync.
    pub fn status(&self) -> PrSyncStatus {
        self.state().status.clone()
    }

    /// Syncs now, after any sync already running, and returns the status it leaves.
    pub async fn sync(&self) -> PrSyncStatus {
        let _running = self.inner.running.lock().await;
        let result = self.sync_once().await;
        let changed = {
            let mut state = self.state();
            match result {
                Ok(synced) => {
                    state.status.last_synced_at = Some(db::now());
                    state.status.last_error = None;
                    state.last_good = Some(synced.began);
                    if synced.full {
                        state.last_full = Some(synced.began);
                    }
                    synced.changed
                }
                Err(err) => {
                    tracing::warn!("PR sync failed: {err}");
                    state.status.last_error = Some(err.to_string());
                    Vec::new()
                }
            }
        };
        for task in changed {
            self.inner.events.changed(Entity::Task, task);
        }
        self.status()
    }

    /// When this sync's incremental search should reach back to; `None` when it is full.
    fn since(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let state = self.state();
        since(state.last_good, state.last_full, now, self.inner.full_every)
    }

    async fn sync_once(&self) -> Result<Synced, SyncError> {
        let began = Utc::now();
        let since = self.since(began);
        let cached = self.state().token.clone();
        let token = match cached {
            Some(token) => token,
            None => self.refresh_token().await?,
        };
        let stored = self
            .inner
            .db
            .call(|conn| db::pull_requests::open(conn))
            .await?;
        let pending = match since {
            Some(_) => {
                self.inner
                    .db
                    .call(|conn| db::pull_requests::open_pending(conn))
                    .await?
            }
            None => Vec::new(),
        };
        let (mut prs, gone) = match self.fetch(&token, &stored, &pending, since).await {
            Err(GithubError::Unauthorized) => {
                // The token may have been refreshed or revoked: ask `gh` again, once.
                let token = self.refresh_token().await?;
                self.fetch(&token, &stored, &pending, since)
                    .await
                    .inspect_err(|err| {
                        if matches!(err, GithubError::Unauthorized) {
                            self.log_out();
                        }
                    })?
            }
            result => result?,
        };
        if since.is_some() {
            // An unchanged inbox PR isn't in an incremental search; match it again as stored.
            let inbox = self
                .inner
                .db
                .call(|conn| db::pull_requests::inbox_open(conn))
                .await?;
            for pr in inbox {
                if !prs
                    .iter()
                    .any(|seen| seen.repo == pr.repo && seen.number == pr.number)
                {
                    prs.push(pr);
                }
            }
        }
        let workspaces = self.workspace_dirs().await?;
        let matched = tokio::task::spawn_blocking(move || {
            let clones = Clones::scan(&workspaces);
            prs.into_iter()
                .map(|pr| {
                    let workspace = clones.workspace_for(&pr.repo, &pr.head_ref);
                    (pr, workspace)
                })
                .collect::<Vec<_>>()
        })
        .await?;
        let changed = self
            .inner
            .db
            .call(move |conn| db::pull_requests::store(conn, &matched, &gone))
            .await?;
        Ok(Synced {
            changed,
            began,
            full: since.is_none(),
        })
    }

    /// The PRs to store, and the stored PRs GitHub no longer shows at all. `stored` is every
    /// PR stored as open, and `pending` those of them whose checks are pending.
    ///
    /// - Full (`since` is `None`): every open PR, then the `stored` ones that left the search,
    ///   fetched by number so a merge or close reaches its task.
    /// - Incremental: the PRs updated at or after `since`, in any state, keeping a merged or
    ///   closed one only if it's stored as open; then the stored open PRs with pending checks
    ///   the search left out, fetched by number. Nothing is gone: only a full sync says so.
    async fn fetch(
        &self,
        token: &str,
        stored: &[(String, i64)],
        pending: &[(String, i64)],
        since: Option<DateTime<Utc>>,
    ) -> Result<(Vec<RemotePr>, Vec<(String, i64)>), GithubError> {
        let github = &self.inner.github;
        let Some(since) = since else {
            let mut prs = github.search(token, github::OPEN_SEARCH).await?;
            let left = missing(stored, &prs);
            let found = github.prs(token, &left).await?;
            let mut gone = Vec::new();
            for (wanted, found) in left.into_iter().zip(found) {
                match found {
                    Some(pr) => prs.push(pr),
                    None => gone.push(wanted),
                }
            }
            return Ok((prs, gone));
        };

        let since = since.to_rfc3339_opts(SecondsFormat::Secs, true);
        let mut prs: Vec<RemotePr> = github
            .search(token, &github::updated_search(&since))
            .await?
            .into_iter()
            .filter(|pr| {
                pr.state == agentdeck_core::PrState::Open
                    || stored
                        .iter()
                        .any(|(repo, number)| pr.repo == *repo && pr.number == *number)
            })
            .collect();
        let pending = missing(pending, &prs);
        prs.extend(github.prs(token, &pending).await?.into_iter().flatten());
        Ok((prs, Vec::new()))
    }

    /// `(id, dir)` of each workspace that isn't archived, oldest first.
    async fn workspace_dirs(&self) -> Result<Vec<(WorkspaceId, PathBuf)>, db::DbError> {
        let workspaces = self
            .inner
            .db
            .call(|conn| db::workspaces::list(conn))
            .await?;
        Ok(workspaces
            .into_iter()
            .filter(|ws| ws.archived_at.is_none())
            .map(|ws| (ws.id, PathBuf::from(ws.dir)))
            .collect())
    }

    /// Runs `gh auth token` and records the outcome as the login state.
    async fn refresh_token(&self) -> Result<String, token::NoToken> {
        let result = token::gh_auth_token().await;
        match &result {
            Ok(token) => {
                let mut state = self.state();
                state.token = Some(token.clone());
                state.status.login = GhLogin::LoggedIn;
            }
            Err(err) => {
                tracing::info!("no GitHub token: {err}");
                self.log_out();
            }
        }
        result
    }

    fn log_out(&self) {
        let mut state = self.state();
        state.token = None;
        state.status.login = GhLogin::LoggedOut;
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // Nothing panics while holding the lock, so a poisoned one still holds a whole state.
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Each of `wanted` (`(repo, number)`) that isn't among `prs`.
fn missing(wanted: &[(String, i64)], prs: &[RemotePr]) -> Vec<(String, i64)> {
    wanted
        .iter()
        .filter(|(repo, number)| {
            !prs.iter()
                .any(|pr| pr.repo == *repo && pr.number == *number)
        })
        .cloned()
        .collect()
}

/// When an incremental search beginning `now` reaches back to, `OVERLAP` before the last good
/// sync began; `None` when the sync must be full: none has been yet, or the last full one
/// began `full_every` or longer ago.
fn since(
    last_good: Option<DateTime<Utc>>,
    last_full: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    full_every: Duration,
) -> Option<DateTime<Utc>> {
    let full_due =
        last_full.is_none_or(|full| (now - full).to_std().unwrap_or_default() >= full_every);
    if full_due {
        return None;
    }
    last_good.map(|good| good - OVERLAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    mod since {
        use super::*;

        const HOUR: Duration = Duration::from_secs(3600);

        fn at(minute: u32) -> DateTime<Utc> {
            DateTime::parse_from_rfc3339(&format!("2026-11-14T10:{minute:02}:00Z"))
                .unwrap()
                .to_utc()
        }

        #[test]
        fn should_be_full_before_any_sync() {
            assert_eq!(since(None, None, at(0), HOUR), None);
        }

        #[test]
        fn should_reach_back_ten_minutes_before_the_last_good_sync() {
            assert_eq!(since(Some(at(30)), Some(at(0)), at(32), HOUR), Some(at(20)));
        }

        #[test]
        fn should_be_full_once_the_last_full_sync_is_old_enough() {
            let last_full = at(0) - chrono::TimeDelta::hours(1);
            assert_eq!(since(Some(at(30)), Some(last_full), at(0), HOUR), None);
        }

        #[test]
        fn should_always_be_full_when_full_every_is_zero() {
            assert_eq!(
                since(Some(at(30)), Some(at(30)), at(30), Duration::ZERO),
                None
            );
        }
    }
}
