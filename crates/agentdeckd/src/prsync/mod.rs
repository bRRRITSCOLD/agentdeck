//! PR sync (spec §8): the viewer's open GitHub PRs become PR tasks, every 120 s and on demand.
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
    state: Mutex<State>,
    /// Held for the length of a sync, so syncs never overlap.
    running: tokio::sync::Mutex<()>,
}

struct State {
    /// The token `gh` last printed; `None` until it prints one GitHub hasn't rejected.
    token: Option<String>,
    status: PrSyncStatus,
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
    /// the GraphQL URL. Must be called inside a tokio runtime.
    pub fn start(db: Db, events: Events, endpoint: String) -> anyhow::Result<PrSync> {
        let sync = PrSync {
            inner: Arc::new(Inner {
                db,
                events,
                github: Github::new(endpoint)?,
                state: Mutex::new(State {
                    token: None,
                    status: PrSyncStatus {
                        login: GhLogin::LoggedOut,
                        last_synced_at: None,
                        last_error: None,
                    },
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
                Ok(changed) => {
                    state.status.last_synced_at = Some(db::now());
                    state.status.last_error = None;
                    changed
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

    async fn sync_once(&self) -> Result<Vec<TaskId>, SyncError> {
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
        let (prs, gone) = match self.fetch(&token, &stored).await {
            Err(GithubError::Unauthorized) => {
                // The token may have been refreshed or revoked: ask `gh` again, once.
                let token = self.refresh_token().await?;
                self.fetch(&token, &stored).await.inspect_err(|err| {
                    if matches!(err, GithubError::Unauthorized) {
                        self.log_out();
                    }
                })?
            }
            result => result?,
        };
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
        Ok(self
            .inner
            .db
            .call(move |conn| db::pull_requests::store(conn, &matched, &gone))
            .await?)
    }

    /// The open PRs the search finds, then the `stored` open PRs that left the search, fetched
    /// by number in batches so a merge or close reaches its task. Also returns the stored PRs
    /// GitHub no longer shows at all.
    async fn fetch(
        &self,
        token: &str,
        stored: &[(String, i64)],
    ) -> Result<(Vec<RemotePr>, Vec<(String, i64)>), GithubError> {
        let github = &self.inner.github;
        let mut prs = github.open_prs(token).await?;
        let left: Vec<(String, i64)> = stored
            .iter()
            .filter(|(repo, number)| {
                !prs.iter()
                    .any(|pr| pr.repo == *repo && pr.number == *number)
            })
            .cloned()
            .collect();
        let found = github.prs(token, &left).await?;
        let mut gone = Vec::new();
        for (wanted, found) in left.into_iter().zip(found) {
            match found {
                Some(pr) => prs.push(pr),
                None => gone.push(wanted),
            }
        }
        Ok((prs, gone))
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
