//! PR sync (spec §8): the viewer's open GitHub PRs become PR tasks, every 120 s and on demand.
//!
//! The first sync, and one every [`FULL_SYNC_VAR`] seconds (an hour by default), is full: it
//! lists the open PRs, most recently updated first, down to the age cutoff (`[pr_sync]
//! max_age_days`, see [`crate::config`]), and fetches the stored ones that left the list; a
//! stored one older than the cutoff is aged out instead. The rest are incremental: they
//! search only the PRs updated since the last good sync began, in any state, then refresh the
//! [`unsettled`] ones (a finished check run doesn't update a PR) and match inbox PRs to
//! workspaces again from what's stored.
//!
//! - [`crate::github_auth`]: the token, from the first source that gives one.
//! - [`github`]: the GraphQL search and the fetch by number.
//! - [`matching`]: which workspace holds a clone of a PR's repo with its branch.
//! - `db::pull_requests`: storing the PRs and moving their tasks.
//!
//! `POST /v1/prs/sync?full=true` (Sync now) is full whatever sync is due, and so is the first
//! sync with another token than the last good one ran with, or after one found none.
//!
//! A stored PR GitHub doesn't show to the token, fetched by number, is marked unreachable: it
//! keeps its state and its task its status, and the next full sync fetches it again.
//!
//! One sync runs at a time: the timer and `POST /v1/prs/sync` queue behind each other.
//! Closing PRs (`POST /v1/prs/close`) queues behind them too, so a sync that read a PR as
//! open before the close never stores it open again after.

pub mod github;
pub mod matching;

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use agentdeck_core::ws::Entity;
use agentdeck_core::{ChecksState, PrState, ReviewState, TaskId, WorkspaceId};
use chrono::{DateTime, SecondsFormat, Utc};
use tokio::time::MissedTickBehavior;

use crate::config::Config;
use crate::db::{self, Db};
use crate::events::Events;
use crate::github_auth::GithubAuth;
use crate::model::{GhLogin, PrCloseResult, PrSyncProgress, PrSyncStage, PrSyncStatus};
use github::{Github, GithubError, RemotePr};
use matching::Clones;

/// How often the timer syncs.
const INTERVAL: Duration = Duration::from_secs(120);

/// The environment variable that points PR sync at another GraphQL endpoint (tests use it).
pub const ENDPOINT_VAR: &str = "AGENTDECK_GITHUB_API";

/// The environment variable holding how many milliseconds one GitHub request may take
/// (tests use it); [`github::TIMEOUT`] when unset.
pub const TIMEOUT_VAR: &str = "AGENTDECK_GITHUB_TIMEOUT_MS";

/// The environment variable holding how many seconds pass between full syncs; `0` makes every
/// sync full (tests use it).
pub const FULL_SYNC_VAR: &str = "AGENTDECK_PR_FULL_SYNC_SECS";

/// How often a sync is full, unless [`FULL_SYNC_VAR`] says otherwise.
pub const FULL_SYNC_EVERY: Duration = Duration::from_secs(60 * 60);

/// Why a task asked to close isn't closed: it isn't a PR task whose PR is stored open.
const NOT_AN_OPEN_PR: &str = "Not an open PR";

/// Why no PR closes: there is no token.
const NOT_SIGNED_IN: &str = "Not signed in to GitHub";

/// The id a [`Entity::PrSync`] event names: there is one sync status.
const STATUS_ID: &str = "status";

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
    /// The token, and whether GitHub accepts it.
    auth: GithubAuth,
    /// How often a sync is full.
    full_every: Duration,
    /// The settings, read again at each sync: `pr_sync.max_age_days` says which open PRs
    /// sync.
    config: Config,
    state: Mutex<State>,
    /// Held for the length of a sync, so syncs never overlap.
    running: tokio::sync::Mutex<()>,
    /// Wakes the timer to sync at once: the age cutoff widened.
    widened: tokio::sync::Notify,
}

struct State {
    /// The status, but for `login` and `max_age_days`, read as it is asked for.
    status: PrSyncStatus,
    /// When the last good sync began; `None` until one has finished.
    last_good: Option<DateTime<Utc>>,
    /// When the last good full sync began.
    last_full: Option<DateTime<Utc>>,
    /// Whether the next sync must be full whenever the last full one was: the age cutoff
    /// widened, so open PRs it now lets in that no incremental search would find are listed,
    /// a client asked for a full sync, or the token changed or went.
    full_wanted: bool,
    /// A hash of the token the last good sync ran with, so a sync with another one is full;
    /// the token itself is never kept here.
    synced_with: Option<u64>,
}

/// What a good sync did.
struct Synced {
    /// When it began.
    began: DateTime<Utc>,
    /// Whether it was full.
    full: bool,
    /// [`token_key`] of the token it ran with.
    token: u64,
}

/// Why a sync failed.
#[derive(Debug, thiserror::Error)]
enum SyncError {
    #[error(transparent)]
    NoToken(#[from] crate::github_auth::NoToken),
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
    /// Starts the timer, whose first sync runs now and reads the token's sources (on the
    /// timer's task, so a slow `gh` never holds up the daemon's start); a token the routes or
    /// a sign-in change syncs at once too. `github` is the GraphQL client; a sync is full
    /// every `full_every`; open PRs GitHub last updated more than `pr_sync.max_age_days` ago,
    /// as `config` says when the sync begins, aren't synced, and the status says how many
    /// days that is now. Must be called inside a tokio runtime.
    pub fn start(
        db: Db,
        events: Events,
        github: Github,
        auth: GithubAuth,
        full_every: Duration,
        config: Config,
    ) -> PrSync {
        let max_age_days = config.max_age_days();
        let mut token_changes = auth.token_changes();
        let sync = PrSync {
            inner: Arc::new(Inner {
                db,
                events,
                github,
                auth,
                full_every,
                config,
                state: Mutex::new(State {
                    status: PrSyncStatus {
                        login: GhLogin::LoggedOut,
                        last_synced_at: None,
                        last_error: None,
                        progress: None,
                        max_age_days,
                        synced_as: None,
                    },
                    last_good: None,
                    last_full: None,
                    full_wanted: false,
                    synced_with: None,
                }),
                running: tokio::sync::Mutex::new(()),
                widened: tokio::sync::Notify::new(),
            }),
        };
        let timer = sync.clone();
        tokio::spawn(async move {
            match timer
                .inner
                .db
                .call(|conn| db::pull_requests::synced_as(conn))
                .await
            {
                Ok(login) => timer.state().status.synced_as = login,
                Err(err) => tracing::warn!("reading the login PRs last synced as: {err}"),
            }
            let mut interval = tokio::time::interval(INTERVAL);
            interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    changed = token_changes.changed() => {
                        if changed.is_err() {
                            // The credentials are gone: the daemon is stopping.
                            return;
                        }
                        // A sign-in or sign-out: every PR is checked again with the new token.
                        timer.state().full_wanted = true;
                    }
                    () = timer.inner.widened.notified() => {}
                }
                timer.sync(false).await;
            }
        });
        sync
    }

    /// The GitHub login, the last sync and the age cutoff as it is now.
    pub fn status(&self) -> PrSyncStatus {
        PrSyncStatus {
            login: if self.inner.auth.token().is_some() {
                GhLogin::LoggedIn
            } else {
                GhLogin::LoggedOut
            },
            max_age_days: self.inner.config.max_age_days(),
            ..self.state().status.clone()
        }
    }

    /// Tells clients the status changed with the age cutoff, `before` days then `after` now
    /// (`0` for none). When it widened, a full sync starts at once, after any sync running, so
    /// the older open PRs it now lets in arrive without waiting for the hourly one; a narrower
    /// one waits for the next sync.
    pub fn cutoff_changed(&self, before: u32, after: u32) {
        self.inner.events.changed(Entity::PrSync, STATUS_ID);
        if widens(before, after) {
            self.state().full_wanted = true;
            self.inner.widened.notify_one();
        }
    }

    /// Syncs now, after any sync already running, and returns the status it leaves. A `full`
    /// sync is full whatever sync is due (Sync now asks for one); else it is the sync due.
    pub async fn sync(&self, full: bool) -> PrSyncStatus {
        let _running = self.inner.running.lock().await;
        self.set_progress(Some(PrSyncProgress {
            prs_done: 0,
            prs: 0,
            stage: PrSyncStage::Listing,
        }));
        let began = Utc::now();
        if full {
            self.state().full_wanted = true;
        }
        let result = self.sync_once(began).await;
        let synced_as = match &result {
            Ok(_) => self.store_synced_as().await,
            Err(_) => None,
        };
        {
            let mut state = self.state();
            state.status.progress = None;
            match result {
                Ok(synced) => {
                    state.status.last_synced_at = Some(db::now());
                    state.status.last_error = None;
                    state.last_good = Some(synced.began);
                    state.synced_with = Some(synced.token);
                    if synced.full {
                        state.last_full = Some(synced.began);
                    }
                    if synced_as.is_some() {
                        state.status.synced_as = synced_as;
                    }
                }
                Err(err) => {
                    tracing::warn!("PR sync failed: {err}");
                    state.status.last_error = Some(err.to_string());
                }
            }
        }
        self.inner.events.changed(Entity::PrSync, STATUS_ID);
        self.status()
    }

    /// Stores the login GitHub gave for the token as the one PRs last synced as, and returns
    /// it; `None` when GitHub gave none (it couldn't be asked) or storing it failed.
    async fn store_synced_as(&self) -> Option<String> {
        let login = self.inner.auth.info().login?;
        if self.state().status.synced_as.as_ref() == Some(&login) {
            return Some(login);
        }
        let stored = login.clone();
        match (self.inner.db)
            .call(move |conn| db::pull_requests::set_synced_as(conn, &stored))
            .await
        {
            Ok(()) => Some(login),
            Err(err) => {
                tracing::warn!("storing the login PRs synced as: {err}");
                None
            }
        }
    }

    /// Closes the PRs of `task_ids` on GitHub, after any sync running, and answers one result
    /// per task in the order asked (spec §8). A task that isn't a PR task whose PR is stored
    /// open fails without reaching GitHub; with no token, every one fails. The node ids are
    /// looked up 25 to a request, then each PR is closed on its own, one at a time; one
    /// GitHub closes is stored closed and its task moves to done at once, with a `changed`
    /// event. A failure is that PR's alone.
    pub async fn close(&self, task_ids: Vec<TaskId>) -> Vec<PrCloseResult> {
        let _running = self.inner.running.lock().await;
        let ids = task_ids.clone();
        let prs = match self
            .inner
            .db
            .call(move |conn| db::pull_requests::closable(conn, &ids))
            .await
        {
            Ok(prs) => prs,
            Err(err) => {
                return task_ids
                    .into_iter()
                    .map(|id| PrCloseResult::failed(id, err.to_string()))
                    .collect();
            }
        };
        let mut results: Vec<PrCloseResult> = task_ids
            .iter()
            .map(|id| PrCloseResult::failed(*id, NOT_AN_OPEN_PR))
            .collect();
        let open: Vec<(usize, (String, i64))> = prs
            .into_iter()
            .enumerate()
            .filter_map(|(i, pr)| Some((i, pr?)))
            .collect();
        if open.is_empty() {
            return results;
        }
        let auth = &self.inner.auth;
        let Ok(mut token) = auth.current_token().await else {
            for (i, _) in &open {
                results[*i] = PrCloseResult::failed(task_ids[*i], NOT_SIGNED_IN);
            }
            return results;
        };
        let wanted: Vec<(String, i64)> = open.iter().map(|(_, pr)| pr.clone()).collect();
        let github = &self.inner.github;
        let node_ids = match github.node_ids(&token, &wanted).await {
            Ok(node_ids) => node_ids,
            // The token may have been refreshed or revoked: read the sources again, once.
            Err(GithubError::Unauthorized) => match auth.reread().await {
                Ok(fresh) => {
                    token = fresh;
                    github
                        .node_ids(&token, &wanted)
                        .await
                        .unwrap_or_else(|err| {
                            auth.rejected();
                            vec![Err(err.to_string()); wanted.len()]
                        })
                }
                Err(_) => vec![Err(NOT_SIGNED_IN.to_owned()); wanted.len()],
            },
            Err(err) => vec![Err(err.to_string()); wanted.len()],
        };
        for ((i, (repo, number)), node_id) in open.into_iter().zip(node_ids) {
            let task_id = task_ids[i];
            results[i] = match node_id {
                Err(error) => PrCloseResult::failed(task_id, error),
                Ok(node_id) => match github.close(&token, &node_id).await {
                    Err(GithubError::GraphQl(message)) => PrCloseResult::failed(task_id, message),
                    Err(err) => PrCloseResult::failed(task_id, err.to_string()),
                    Ok(()) => {
                        self.closed(task_id, &repo, number).await;
                        PrCloseResult::closed(task_id)
                    }
                },
            };
        }
        results
    }

    /// Stores task `task_id`'s PR, `repo#number`, which GitHub just closed, as closed, and
    /// tells clients its task changed. A failure only logs: GitHub closed it, and the next
    /// full sync reads it closed.
    async fn closed(&self, task_id: TaskId, repo: &str, number: i64) {
        match self
            .inner
            .db
            .call(move |conn| db::pull_requests::close(conn, task_id))
            .await
        {
            Ok(true) => self.inner.events.changed(Entity::Task, task_id),
            Ok(false) => {}
            Err(err) => tracing::warn!("storing {repo}#{number} as closed: {err}"),
        }
    }

    /// Records how far the running sync is, and tells clients.
    fn set_progress(&self, progress: Option<PrSyncProgress>) {
        self.state().status.progress = progress;
        self.inner.events.changed(Entity::PrSync, STATUS_ID);
    }

    /// Records that the running sync has read its last page and is fetching details.
    fn details_stage(&self) {
        let progress = self.state().status.progress;
        self.set_progress(progress.map(|progress| PrSyncProgress {
            stage: PrSyncStage::Details,
            ..progress
        }));
    }

    /// When the sync beginning `now` with `token` should reach back to; `None` when it is
    /// full. A full sync the widened cutoff, a client or a change of token asked for is this
    /// one, and so is the first with another token than the last good sync's.
    fn since(&self, now: DateTime<Utc>, token: &str) -> Option<DateTime<Utc>> {
        let mut state = self.state();
        let wanted = std::mem::take(&mut state.full_wanted);
        if wanted || state.synced_with != Some(token_key(token)) {
            return None;
        }
        since(state.last_good, state.last_full, now, self.inner.full_every)
    }

    /// One sync, which began at `began`: the sync due with the token there is now. With no
    /// token, the next sync with one is full; a full one that fails is still owed.
    async fn sync_once(&self, began: DateTime<Utc>) -> Result<Synced, SyncError> {
        let auth = &self.inner.auth;
        let token = auth.current_token().await.inspect_err(|_| {
            self.state().full_wanted = true;
        })?;
        let since = self.since(began, &token);
        self.sync_with(began, token, since).await.inspect_err(|_| {
            // A full sync the widened cutoff, a client or a new token asked for is still owed.
            if since.is_none() {
                self.state().full_wanted = true;
            }
        })
    }

    /// One sync with `token`, which began at `began` and reaches back to `since` (`None` for a
    /// full one).
    async fn sync_with(
        &self,
        began: DateTime<Utc>,
        token: String,
        since: Option<DateTime<Utc>>,
    ) -> Result<Synced, SyncError> {
        let auth = &self.inner.auth;
        let workspaces = self.workspace_dirs().await?;
        let clones =
            Arc::new(tokio::task::spawn_blocking(move || Clones::scan(&workspaces)).await?);
        let mut read = Read {
            since,
            // A cutoff before the earliest time chrono holds is no cutoff.
            cutoff: self
                .inner
                .config
                .max_age()
                .and_then(|max_age| began.checked_sub_signed(max_age)),
            clones,
        };
        let token = match self.read(&token, &read).await {
            Err(SyncError::Github(GithubError::Unauthorized)) => {
                // The token may have been refreshed or revoked: read the sources again, once.
                // Another token sees other PRs, so its read is full.
                let fresh = auth.reread().await.inspect_err(|_| {
                    self.state().full_wanted = true;
                })?;
                if fresh != token {
                    read.since = None;
                }
                self.read(&fresh, &read).await.inspect_err(|err| {
                    if matches!(err, SyncError::Github(GithubError::Unauthorized)) {
                        auth.rejected();
                    }
                    if read.since.is_none() {
                        self.state().full_wanted = true;
                    }
                })?;
                fresh
            }
            result => {
                result?;
                token
            }
        };
        Ok(Synced {
            began,
            full: read.since.is_none(),
            token: token_key(&token),
        })
    }

    /// Reads GitHub and stores what it shows: a full read when `since` is `None`, else an
    /// incremental one.
    async fn read(&self, token: &str, read: &Read) -> Result<(), SyncError> {
        match read.since {
            None => self.read_full(token, read.cutoff, &read.clones).await,
            Some(since) => {
                self.read_since(token, since, read.cutoff, &read.clones)
                    .await
            }
        }
    }

    /// A full read, in two phases.
    ///
    /// 1. The list of open PRs, light fields only, stored page by page as it arrives. A
    ///    stored PR keeps its checks and review for now. The list is most recently updated
    ///    first, so it ends at the first PR older than `cutoff`, which isn't stored.
    /// 2. Every field, fetched by number, for the PRs that need it: new ones, ones GitHub
    ///    updated since they were stored, and [`unsettled`] ones.
    ///
    /// Then the PRs stored as open that left the list are fetched by number, so a merge or
    /// close reaches its task; one GitHub doesn't show to the token is marked unreachable,
    /// keeping its state and its task's status. One last updated before `cutoff` isn't
    /// fetched: it is aged out, its task moving to done once.
    async fn read_full(
        &self,
        token: &str,
        cutoff: Option<DateTime<Utc>>,
        clones: &Arc<Clones>,
    ) -> Result<(), SyncError> {
        let github = &self.inner.github;
        let stored = self
            .inner
            .db
            .call(|conn| db::pull_requests::open_prs(conn))
            .await?;
        let mut seen: Vec<(String, i64)> = Vec::new();
        let mut stale: Vec<(String, i64)> = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..github::MAX_LIST_PAGES {
            let page = github.list_page(token, after.as_deref()).await?;
            let mut prs = Vec::with_capacity(page.prs.len());
            let mut past_cutoff = false;
            for pr in page.prs {
                if older_than(&pr.updated_at, cutoff) {
                    past_cutoff = true;
                    break;
                }
                let key = (pr.repo.clone(), pr.number);
                match stored
                    .iter()
                    .find(|old| old.repo == pr.repo && old.number == pr.number)
                {
                    Some(old) => {
                        if old.updated_at != pr.updated_at || unsettled(old) {
                            stale.push(key.clone());
                        }
                        prs.push(RemotePr {
                            checks: old.checks,
                            review: old.review,
                            ..pr
                        });
                    }
                    None => {
                        stale.push(key.clone());
                        prs.push(pr);
                    }
                }
                seen.push(key);
            }
            self.store(prs, Vec::new(), clones).await?;
            let prs_done = u32::try_from(seen.len()).unwrap_or(u32::MAX);
            self.set_progress(Some(PrSyncProgress {
                prs_done,
                // The list's count takes in the PRs past the cutoff, which this sync skips.
                prs: if past_cutoff {
                    prs_done
                } else {
                    page.total.max(prs_done)
                },
                stage: PrSyncStage::Listing,
            }));
            match page.next {
                Some(next) if !past_cutoff => after = Some(next),
                _ => break,
            }
        }

        self.details_stage();
        let details: Vec<RemotePr> = github
            .prs(token, &stale)
            .await?
            .into_iter()
            .flatten()
            .collect();
        self.store(details, Vec::new(), clones).await?;

        let (aged, stored): (Vec<RemotePr>, Vec<RemotePr>) = stored
            .into_iter()
            .partition(|pr| older_than(&pr.updated_at, cutoff));
        let aged: Vec<(String, i64)> = aged.into_iter().map(|pr| (pr.repo, pr.number)).collect();
        self.age_out(missing(&aged, &seen)).await?;
        let stored: Vec<(String, i64)> =
            stored.into_iter().map(|pr| (pr.repo, pr.number)).collect();
        let left = missing(&stored, &seen);
        let found = github.prs(token, &left).await?;
        let mut prs = Vec::new();
        let mut unreachable = Vec::new();
        for (wanted, found) in left.into_iter().zip(found) {
            match found {
                Some(pr) => prs.push(pr),
                None => unreachable.push(wanted),
            }
        }
        self.store(prs, unreachable, clones).await
    }

    /// An incremental read: the PRs updated at or after `since`, in any state, keeping a
    /// merged or closed one only if it's stored as open, and none last updated before
    /// `cutoff`; then the stored open PRs the search left out that are [`unsettled`] and
    /// GitHub updated within `cutoff`, fetched by number, and the inbox PRs it left out,
    /// matched to a workspace again as stored. Nothing is gone: only a full read says so.
    async fn read_since(
        &self,
        token: &str,
        since: DateTime<Utc>,
        cutoff: Option<DateTime<Utc>>,
        clones: &Arc<Clones>,
    ) -> Result<(), SyncError> {
        let github = &self.inner.github;
        let stored = self
            .inner
            .db
            .call(|conn| db::pull_requests::open(conn))
            .await?;
        let q = github::updated_search(&since.to_rfc3339_opts(SecondsFormat::Secs, true));
        let mut seen: Vec<(String, i64)> = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..github::MAX_PAGES {
            let page = github.search_page(token, &q, after.as_deref()).await?;
            seen.extend(page.prs.iter().map(|pr| (pr.repo.clone(), pr.number)));
            let prs: Vec<RemotePr> = page
                .prs
                .into_iter()
                .filter(|pr| {
                    pr.state == PrState::Open || stored.contains(&(pr.repo.clone(), pr.number))
                })
                .filter(|pr| !older_than(&pr.updated_at, cutoff))
                .collect();
            self.store(prs, Vec::new(), clones).await?;
            let prs_done = u32::try_from(seen.len()).unwrap_or(u32::MAX);
            self.set_progress(Some(PrSyncProgress {
                prs_done,
                prs: page.total.max(prs_done),
                stage: PrSyncStage::Listing,
            }));
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }

        let unsettled: Vec<(String, i64)> = self
            .inner
            .db
            .call(|conn| db::pull_requests::open_current(conn))
            .await?
            .into_iter()
            .filter(|pr| unsettled(pr) && !older_than(&pr.updated_at, cutoff))
            .map(|pr| (pr.repo, pr.number))
            .collect();
        let unsettled = missing(&unsettled, &seen);
        if !unsettled.is_empty() {
            self.details_stage();
        }
        let mut prs: Vec<RemotePr> = github
            .prs(token, &unsettled)
            .await?
            .into_iter()
            .flatten()
            .collect();
        seen.extend(prs.iter().map(|pr| (pr.repo.clone(), pr.number)));
        let inbox = self
            .inner
            .db
            .call(|conn| db::pull_requests::inbox_open(conn))
            .await?;
        prs.extend(
            inbox
                .into_iter()
                .filter(|pr| !seen.contains(&(pr.repo.clone(), pr.number))),
        );
        self.store(prs, Vec::new(), clones).await
    }

    /// Matches `prs` to workspaces, stores them, marks the `unreachable` ones, and tells
    /// clients which tasks changed.
    async fn store(
        &self,
        prs: Vec<RemotePr>,
        unreachable: Vec<(String, i64)>,
        clones: &Arc<Clones>,
    ) -> Result<(), SyncError> {
        if prs.is_empty() && unreachable.is_empty() {
            return Ok(());
        }
        let clones = Arc::clone(clones);
        let matched = tokio::task::spawn_blocking(move || {
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
            .call(move |conn| db::pull_requests::store(conn, &matched, &unreachable))
            .await?;
        for task in changed {
            self.inner.events.changed(Entity::Task, task);
        }
        Ok(())
    }

    /// Ages out the stored open PRs `prs`, and tells clients which tasks changed.
    async fn age_out(&self, prs: Vec<(String, i64)>) -> Result<(), SyncError> {
        if prs.is_empty() {
            return Ok(());
        }
        let changed = self
            .inner
            .db
            .call(move |conn| db::pull_requests::age_out(conn, &prs))
            .await?;
        for task in changed {
            self.inner.events.changed(Entity::Task, task);
        }
        Ok(())
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

    fn state(&self) -> MutexGuard<'_, State> {
        // Nothing panics while holding the lock, so a poisoned one still holds a whole state.
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// What one sync reads, decided as it begins.
struct Read {
    /// When the incremental search reaches back to; `None` for a full read.
    since: Option<DateTime<Utc>>,
    /// Open PRs last updated before this aren't synced; `None` for no cutoff.
    cutoff: Option<DateTime<Utc>>,
    /// The clones in the workspaces, for matching.
    clones: Arc<Clones>,
}

/// A hash of `token`, to tell whether a sync runs with the same token as the last good one
/// without keeping the token.
fn token_key(token: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    token.hash(&mut hasher);
    hasher.finish()
}

/// Whether stored open PR `pr` may change in a way GitHub doesn't count as an update, so no
/// search or list shows it (spec §8): its checks are pending or haven't started (a check run
/// finishing doesn't update a PR), or a review is still to come, required or asked for again
/// after changes were requested (a submitted review does update a PR, #247, so this arm is a
/// hedge; #271 weighs dropping it). No
/// review decision means the repo requires none, and an approved PR waits on nobody.
fn unsettled(pr: &RemotePr) -> bool {
    matches!(pr.checks, None | Some(ChecksState::Pending))
        || matches!(
            pr.review,
            Some(ReviewState::ReviewRequired | ReviewState::ChangesRequested)
        )
}

/// Whether a PR last updated at `updated_at` (RFC 3339) is older than `cutoff`. One whose time
/// doesn't parse isn't: better synced than dropped.
fn older_than(updated_at: &str, cutoff: Option<DateTime<Utc>>) -> bool {
    let Some(cutoff) = cutoff else {
        return false;
    };
    DateTime::parse_from_rfc3339(updated_at).is_ok_and(|updated| updated < cutoff)
}

/// Each of `wanted` (`(repo, number)`) that isn't among `seen`.
fn missing(wanted: &[(String, i64)], seen: &[(String, i64)]) -> Vec<(String, i64)> {
    wanted
        .iter()
        .filter(|pr| !seen.contains(pr))
        .cloned()
        .collect()
}

/// Whether an age cutoff of `after` days lets in open PRs one of `before` days left out: it is
/// longer, or `0` (none) where there was one.
fn widens(before: u32, after: u32) -> bool {
    before != 0 && (after == 0 || after > before)
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

    mod older_than {
        use super::*;

        fn cutoff() -> Option<DateTime<Utc>> {
            Some(
                DateTime::parse_from_rfc3339("2026-08-16T10:00:00Z")
                    .unwrap()
                    .to_utc(),
            )
        }

        #[test]
        fn should_be_older_when_updated_before_the_cutoff() {
            assert!(older_than("2017-03-01T12:00:00Z", cutoff()));
            assert!(older_than("2026-08-16T09:59:59Z", cutoff()));
        }

        #[test]
        fn should_not_be_older_when_updated_at_or_after_the_cutoff() {
            assert!(!older_than("2026-08-16T10:00:00Z", cutoff()));
            assert!(!older_than("2026-11-14T10:00:00Z", cutoff()));
        }

        #[test]
        fn should_never_be_older_without_a_cutoff() {
            assert!(!older_than("2017-03-01T12:00:00Z", None));
        }

        #[test]
        fn should_not_be_older_when_the_time_does_not_parse() {
            assert!(!older_than("yesterday", cutoff()));
        }
    }

    mod unsettled {
        use super::*;

        fn pr(checks: Option<ChecksState>, review: Option<ReviewState>) -> RemotePr {
            RemotePr {
                repo: "acme/widgets".into(),
                number: 7,
                title: "Add PR sync".into(),
                url: "https://github.com/acme/widgets/pull/7".into(),
                state: PrState::Open,
                draft: false,
                head_ref: "feat/sync".into(),
                checks,
                review,
                updated_at: "2026-11-01T10:00:00Z".into(),
            }
        }

        #[test]
        fn should_be_unsettled_while_checks_are_pending_or_not_started() {
            assert!(unsettled(&pr(None, Some(ReviewState::Approved))));
            assert!(unsettled(&pr(
                Some(ChecksState::Pending),
                Some(ReviewState::Approved)
            )));
        }

        #[test]
        fn should_be_unsettled_while_a_review_is_still_to_come() {
            for review in [ReviewState::ReviewRequired, ReviewState::ChangesRequested] {
                assert!(unsettled(&pr(Some(ChecksState::Failure), Some(review))));
            }
        }

        #[test]
        fn should_be_settled_once_checks_finished_and_no_review_is_awaited() {
            for review in [None, Some(ReviewState::Approved)] {
                for checks in [ChecksState::Success, ChecksState::Failure] {
                    assert!(!unsettled(&pr(Some(checks), review)));
                }
            }
        }
    }

    mod widens {
        use super::*;

        #[test]
        fn should_widen_when_the_cutoff_grows() {
            assert!(widens(30, 365));
        }

        #[test]
        fn should_widen_when_the_cutoff_is_lifted() {
            assert!(widens(30, 0));
        }

        #[test]
        fn should_not_widen_when_the_cutoff_shrinks_or_stays() {
            assert!(!widens(365, 30));
            assert!(!widens(30, 30));
        }

        #[test]
        fn should_not_widen_from_no_cutoff() {
            assert!(!widens(0, 30));
            assert!(!widens(0, 0));
        }
    }

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
