//! PR sync (spec §8): the viewer's open GitHub PRs are kept, every 120 s and on demand. Each
//! PR's Repository is stored by GitHub's id, and each PR by that id and its number, so a rename
//! or a transfer keeps both.
//!
//! Your PRs are the ones you authored, the ones where your review is requested and the ones
//! assigned to you ([`Whose`]); each stores why it is yours ([`agentdeck_core::Yours`]), read
//! from its author, review requests and assignees against the login the token belongs to.
//!
//! The first sync, and one every [`FULL_SYNC_VAR`] seconds (an hour by default), is full: it
//! lists the open PRs you authored, then searches the open ones where your review is
//! requested and those assigned to you, each most recently updated first, down to the age
//! cutoff (`[pr_sync] max_age_days`, see [`crate::config`]), and fetches the stored ones that
//! none showed; a stored one older than the cutoff is Finished instead. The rest are
//! incremental: they search only the PRs updated since the last good sync began, in any
//! state, one search per way a PR is yours, then refresh the ones that may have changed
//! unseen ([`recheck`]: a finished check run doesn't update a PR, and a submitted review or
//! an unassignment takes a PR out of every search).
//!
//! A PR is **Finished** from when a sync sees it merged or closed, or open but yours no way at
//! all, a close from agentdeck closes it, or a full sync finds it open but older than the
//! cutoff; GitHub showing it open, yours and within the cutoff again ends that. Each good sync then deletes the PRs Finished
//! [`FINISHED_VAR`] seconds ago or longer (7 days by default), and the Repositories nothing
//! references any more. A PR the GitHub sign-in can't see is never Finished by the cutoff,
//! and is never deleted.
//!
//! - [`crate::github_auth`]: the token, from the first source that gives one.
//! - [`github`]: the GraphQL search and the fetch by number.
//! - `db::pull_requests`: storing the PRs; `db::repositories`: storing their Repositories.
//!
//! `POST /v1/prs/sync?full=true` (Sync now) is full whatever sync is due, and so is the first
//! sync with another token than the last good one ran with, or after one found none.
//!
//! A stored PR GitHub doesn't show to the token, fetched by number, is marked unreachable: it
//! keeps its state, and the next full sync fetches it again.
//!
//! A full sync also reads Repositories by GitHub's id (REST `GET /repositories/{id}`, as
//! GraphQL has no lookup by that id): each one with a PR that a fetch by its last known name
//! didn't find, so a rename whose old name another Repository took is followed, and every
//! linked one no PR showed, so a Link follows a rename too. One GitHub doesn't show is marked
//! Unreachable, keeping its Links; GitHub showing it again, by id or with a PR, clears that.
//!
//! One sync runs at a time: the timer and `POST /v1/prs/sync` queue behind each other.
//! Closing PRs (`POST /v1/prs/close`) queues behind them too, so a sync that read a PR as
//! open before the close never stores it open again after.

pub mod github;

use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use agentdeck_core::ws::Entity;
use agentdeck_core::{ChecksState, PrRef, PrState, RepositoryId, ReviewState, Yours};
use chrono::{DateTime, SecondsFormat, Utc};
use tokio::time::MissedTickBehavior;

use crate::config::Config;
use crate::db::{self, Db};
use crate::events::Events;
use crate::github_auth::GithubAuth;
use crate::model::{GhLogin, PrCloseResult, PrSyncProgress, PrSyncStage, PrSyncStatus};
use github::{Github, GithubError, PrKey, RemotePr, Whose};

/// How often the timer syncs.
const INTERVAL: Duration = Duration::from_secs(120);

/// The environment variable that points PR sync at another GraphQL endpoint (tests use it).
pub const ENDPOINT_VAR: &str = "AGENTDECK_GITHUB_API";

/// The environment variable that points GitHub's REST calls at another base URL (tests use it).
pub const REST_ENDPOINT_VAR: &str = "AGENTDECK_GITHUB_REST_API";

/// The environment variable holding how many milliseconds one GitHub request may take
/// (tests use it); [`github::TIMEOUT`] when unset.
pub const TIMEOUT_VAR: &str = "AGENTDECK_GITHUB_TIMEOUT_MS";

/// The environment variable holding how many seconds pass between full syncs; `0` makes every
/// sync full (tests use it).
pub const FULL_SYNC_VAR: &str = "AGENTDECK_PR_FULL_SYNC_SECS";

/// How often a sync is full, unless [`FULL_SYNC_VAR`] says otherwise.
pub const FULL_SYNC_EVERY: Duration = Duration::from_secs(60 * 60);

/// The environment variable holding how many seconds a PR stays Finished before a sync
/// deletes it; `0` deletes it at the sync that finishes it (tests use it).
pub const FINISHED_VAR: &str = "AGENTDECK_PR_FINISHED_SECS";

/// How long a PR stays Finished, unless [`FINISHED_VAR`] says otherwise.
pub const KEEP_FINISHED: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Why a PR asked to close isn't closed: it isn't stored open, or the sign-in can't see it.
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
    /// Gets a `changed` event for each PR and Repository a sync creates, changes or deletes.
    events: Events,
    github: Github,
    /// The token, and whether GitHub accepts it.
    auth: GithubAuth,
    /// How often a sync is full.
    full_every: Duration,
    /// How long a PR stays Finished before a sync deletes it.
    keep_finished: Duration,
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
    /// days that is now. A good sync deletes the PRs Finished `keep_finished` ago or longer.
    /// Must be called inside a tokio runtime.
    pub fn start(
        db: Db,
        events: Events,
        github: Github,
        auth: GithubAuth,
        full_every: Duration,
        keep_finished: Duration,
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
                keep_finished,
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
        let mut result = self.sync_once(began).await;
        if result.is_ok()
            && let Err(err) = self.prune().await
        {
            result = Err(err);
        }
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

    /// Closes the pull requests `prs` names on GitHub, after any sync running, and answers one
    /// result per reference in the order asked (spec §8). A PR that isn't stored open, or that
    /// the GitHub sign-in can't see, fails without reaching GitHub; with no token, every one
    /// fails. The node ids are looked up 25 to a request, then each PR is closed on its own,
    /// one at a time; one GitHub closes is stored closed, and so Finished, at once, with a
    /// `changed` event. A failure is that PR's alone.
    pub async fn close(&self, prs: Vec<PrRef>) -> Vec<PrCloseResult> {
        let _running = self.inner.running.lock().await;
        let asked = prs.clone();
        let found = match self
            .inner
            .db
            .call(move |conn| db::pull_requests::closable(conn, &asked))
            .await
        {
            Ok(found) => found,
            Err(err) => {
                return prs
                    .into_iter()
                    .map(|pr| PrCloseResult::failed(pr, err.to_string()))
                    .collect();
            }
        };
        let mut results: Vec<PrCloseResult> = prs
            .iter()
            .map(|pr| PrCloseResult::failed(pr.clone(), NOT_AN_OPEN_PR))
            .collect();
        let open: Vec<(usize, db::pull_requests::Closable)> = found
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
                results[*i] = PrCloseResult::failed(prs[*i].clone(), NOT_SIGNED_IN);
            }
            return results;
        };
        let wanted: Vec<(String, i64)> = open
            .iter()
            .map(|(_, pr)| (pr.repo.clone(), pr.number))
            .collect();
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
        for ((i, stored), node_id) in open.into_iter().zip(node_ids) {
            let pr = prs[i].clone();
            results[i] = match node_id {
                Err(error) => PrCloseResult::failed(pr, error),
                Ok(node_id) => match github.close(&token, &node_id).await {
                    Err(GithubError::GraphQl(message)) => PrCloseResult::failed(pr, message),
                    Err(err) => PrCloseResult::failed(pr, err.to_string()),
                    Ok(()) => {
                        self.closed(&stored).await;
                        PrCloseResult::closed(pr)
                    }
                },
            };
        }
        results
    }

    /// Stores `pr`, which GitHub just closed, as closed and Finished, and tells clients it
    /// changed. A failure only logs: GitHub closed it, and the next full sync reads it closed.
    async fn closed(&self, pr: &db::pull_requests::Closable) {
        let key = pr.key;
        match self
            .inner
            .db
            .call(move |conn| db::pull_requests::close(conn, key))
            .await
        {
            Ok(true) => self.inner.events.changed(Entity::PullRequest, key),
            Ok(false) => {}
            Err(err) => tracing::warn!("storing {}#{} as closed: {err}", pr.repo, pr.number),
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
        let mut read = Read {
            since,
            // A cutoff before the earliest time chrono holds is no cutoff.
            cutoff: self
                .inner
                .config
                .max_age()
                .and_then(|max_age| began.checked_sub_signed(max_age)),
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
            None => self.read_full(token, read.cutoff).await,
            Some(since) => self.read_since(token, since, read.cutoff).await,
        }
    }

    /// A full read, in two phases.
    ///
    /// 1. The list of open PRs you authored, light fields only, then the searches for open
    ///    PRs where your review is requested and that are assigned to you, every field, each
    ///    stored page by page as it arrives. A stored PR keeps its checks and review from the
    ///    list for now. Each is most recently updated first, so it ends at the first PR older
    ///    than `cutoff`, which isn't stored.
    /// 2. Every field, fetched by number, for the listed PRs that need it and no search read
    ///    whole: new ones, ones GitHub updated since they were stored, and [`unsettled`] ones.
    ///
    /// Then the PRs stored as open that no list or search showed are fetched by number, so a
    /// merge, a close or a PR no longer yours is seen ([`PrSync::fetch_left`]); one GitHub
    /// doesn't show to the token is marked unreachable, keeping its state. One last updated
    /// before `cutoff` isn't fetched: it is Finished, unless it is unreachable. Last, every
    /// linked Repository GitHub showed nothing of is read by id ([`PrSync::read_linked`]).
    async fn read_full(&self, token: &str, cutoff: Option<DateTime<Utc>>) -> Result<(), SyncError> {
        let github = &self.inner.github;
        let stored = self
            .inner
            .db
            .call(|conn| db::pull_requests::open_prs(conn))
            .await?;
        let mut tally = Tally::default();
        let mut stale: Vec<RemotePr> = Vec::new();
        let mut after: Option<String> = None;
        let mut listed: u32 = 0;
        for _ in 0..github::MAX_LIST_PAGES {
            let page = github.list_page(token, after.as_deref()).await?;
            let mut prs = Vec::with_capacity(page.prs.len());
            let mut past_cutoff = false;
            for pr in page.prs {
                if older_than(&pr.updated_at, cutoff) {
                    past_cutoff = true;
                    break;
                }
                tally.see(pr.key());
                listed += 1;
                match stored.iter().find(|old| old.key() == pr.key()) {
                    Some(old) => {
                        if old.updated_at != pr.updated_at || unsettled(old) {
                            stale.push(pr.clone());
                        }
                        prs.push(RemotePr {
                            checks: old.checks,
                            review: old.review,
                            ..pr
                        });
                    }
                    None => {
                        stale.push(pr.clone());
                        prs.push(pr);
                    }
                }
            }
            self.store(prs, Vec::new()).await?;
            // The list's count takes in the PRs past the cutoff, which this sync skips.
            let total = if past_cutoff { listed } else { page.total };
            self.set_progress(Some(tally.progress(total)));
            match page.next {
                Some(next) if !past_cutoff => after = Some(next),
                _ => {
                    tally.counted(total.max(listed));
                    break;
                }
            }
        }
        for whose in [Whose::ReviewRequested, Whose::Assigned] {
            let q = github::open_search(whose);
            let read = self
                .search(token, &q, cutoff, true, &mut tally, |_| true)
                .await?;
            // Read whole: nothing to fetch for it.
            stale.retain(|pr| !read.contains(&pr.key()));
        }

        self.details_stage();
        let details = self.fetch(token, &stale).await?;
        self.store(details.found, Vec::new()).await?;

        // Read again, now the list is stored, with each Repository's name GitHub gives it now.
        let stored = self
            .inner
            .db
            .call(|conn| db::pull_requests::open_prs(conn))
            .await?;
        let (aged, left): (Vec<RemotePr>, Vec<RemotePr>) = stored
            .into_iter()
            .filter(|pr| !tally.seen.contains(&pr.key()))
            .partition(|pr| older_than(&pr.updated_at, cutoff));
        self.finish_aged(aged.iter().map(RemotePr::key).collect())
            .await?;
        // The Repositories GitHub showed this sync, with a PR in the list or a search (the
        // details fetched are theirs), then with a PR fetched or by id.
        let mut read: BTreeSet<RepositoryId> =
            (tally.seen.iter()).map(|pr| pr.repository).collect();
        let left = self.fetch_left(token, &left, &mut read).await?;
        self.store(left.found, left.missing).await?;
        self.read_linked(token, &read).await
    }

    /// Fetches `left`, stored open PRs that no list or search showed, by number under the
    /// name each one's Repository last went by. Then reads by GitHub's id each Repository
    /// that GitHub showed no PR of, here or in `read`, and that has a PR it didn't show as
    /// asked (none, or another Repository's: its name was taken after a rename agentdeck
    /// hadn't seen). One GitHub shows under a new name is stored so, and those PRs are fetched
    /// again under it; one it doesn't show is marked Unreachable, and they are missing. Adds
    /// every Repository GitHub showed to `read`.
    async fn fetch_left(
        &self,
        token: &str,
        left: &[RemotePr],
        read: &mut BTreeSet<RepositoryId>,
    ) -> Result<Fetched, SyncError> {
        let mut fetched = self.fetch(token, left).await?;
        read.extend(fetched.found.iter().map(|pr| pr.repository.id));
        let unread: BTreeSet<RepositoryId> = (fetched.missing.iter())
            .map(|pr| pr.repository)
            .filter(|id| !read.contains(id))
            .collect();
        if unread.is_empty() {
            return Ok(fetched);
        }
        let now = self.read_by_id(token, unread, read).await?;
        let again: Vec<RemotePr> = (left.iter())
            .filter(|pr| fetched.missing.contains(&pr.key()))
            .filter_map(|pr| {
                let now = now.iter().find(|r| r.id == pr.repository.id)?;
                let renamed =
                    !(now.name.to_string()).eq_ignore_ascii_case(&pr.repository.name.to_string());
                renamed.then(|| RemotePr {
                    repository: now.clone(),
                    ..pr.clone()
                })
            })
            .collect();
        if again.is_empty() {
            return Ok(fetched);
        }
        let refetched = self.fetch(token, &again).await?;
        fetched
            .missing
            .retain(|key| !again.iter().any(|pr| pr.key() == *key));
        fetched.found.extend(refetched.found);
        fetched.missing.extend(refetched.missing);
        Ok(fetched)
    }

    /// Reads by GitHub's id every Repository a Workspace links that this full sync didn't
    /// find in `read`, so a linked Repository with no PR GitHub showed still follows a rename,
    /// a lost access (Unreachable, its Links kept) and a regained one (spec §8).
    async fn read_linked(
        &self,
        token: &str,
        read: &BTreeSet<RepositoryId>,
    ) -> Result<(), SyncError> {
        let linked = (self.inner.db)
            .call(|conn| db::repositories::linked(conn))
            .await?;
        let unread: BTreeSet<RepositoryId> = (linked.into_iter())
            .filter(|id| !read.contains(id))
            .collect();
        if !unread.is_empty() {
            self.read_by_id(token, unread, &mut BTreeSet::new()).await?;
        }
        Ok(())
    }

    /// Reads each of `ids` by GitHub's id and stores what GitHub says: its name, URL, private
    /// and archived now, or, for one it doesn't show, the Unreachable mark; tells clients which
    /// changed. Adds those GitHub showed to `read`, and returns them.
    async fn read_by_id(
        &self,
        token: &str,
        ids: BTreeSet<RepositoryId>,
        read: &mut BTreeSet<RepositoryId>,
    ) -> Result<Vec<github::RemoteRepository>, SyncError> {
        let ids: Vec<RepositoryId> = ids.into_iter().collect();
        let answers = self.inner.github.repositories_by_id(token, &ids).await?;
        let mut found = Vec::new();
        let mut unreachable = Vec::new();
        for (id, answer) in ids.into_iter().zip(answers) {
            match answer {
                Some(repository) => {
                    read.insert(id);
                    found.push(repository);
                }
                None => unreachable.push(id),
            }
        }
        let stored = found.clone();
        let changed = (self.inner.db)
            .call(move |conn| db::repositories::refresh(conn, &stored, &unreachable))
            .await?;
        for repository in changed {
            self.inner.events.changed(Entity::Repository, repository);
        }
        Ok(found)
    }

    /// An incremental read: a search for each way a PR is yours ([`Whose`]) for the PRs
    /// updated at or after `since`, in any state, keeping a merged or closed one only if it's
    /// stored as open, and none last updated before `cutoff`; then the stored open PRs the
    /// searches left out that GitHub updated within `cutoff` and that may have changed unseen
    /// ([`recheck`]), fetched by number. Nothing is gone: only a full read says so.
    async fn read_since(
        &self,
        token: &str,
        since: DateTime<Utc>,
        cutoff: Option<DateTime<Utc>>,
    ) -> Result<(), SyncError> {
        let stored = self
            .inner
            .db
            .call(|conn| db::pull_requests::open(conn))
            .await?;
        let since = since.to_rfc3339_opts(SecondsFormat::Secs, true);
        let mut tally = Tally::default();
        for whose in Whose::ALL {
            let q = github::updated_search(whose, &since);
            self.search(token, &q, cutoff, false, &mut tally, |pr| {
                pr.state == PrState::Open || stored.contains(&pr.key())
            })
            .await?;
        }

        let recheck: Vec<RemotePr> = self
            .inner
            .db
            .call(|conn| db::pull_requests::open_current(conn))
            .await?
            .into_iter()
            .filter(|pr| {
                recheck(pr)
                    && !older_than(&pr.updated_at, cutoff)
                    && !tally.seen.contains(&pr.key())
            })
            .collect();
        if !recheck.is_empty() {
            self.details_stage();
        }
        let fetched = self.fetch(token, &recheck).await?;
        self.store(fetched.found, Vec::new()).await
    }

    /// Reads search `q` page by page, at most [`github::MAX_PAGES`], storing each page's PRs
    /// that `keep` lets through and that GitHub updated within `cutoff` as it arrives, and
    /// counting every PR read in `tally`. A `sorted` search, most recently updated first, ends
    /// at the first PR older than `cutoff`, which isn't counted. Returns the keys of the PRs
    /// it stored.
    async fn search(
        &self,
        token: &str,
        q: &str,
        cutoff: Option<DateTime<Utc>>,
        sorted: bool,
        tally: &mut Tally,
        keep: impl Fn(&RemotePr) -> bool,
    ) -> Result<Vec<PrKey>, SyncError> {
        let github = &self.inner.github;
        let mut kept = Vec::new();
        let mut read: u32 = 0;
        let mut after: Option<String> = None;
        for _ in 0..github::MAX_PAGES {
            let page = github.search_page(token, q, after.as_deref()).await?;
            let mut prs = Vec::with_capacity(page.prs.len());
            let mut past_cutoff = false;
            for pr in page.prs {
                let old = older_than(&pr.updated_at, cutoff);
                if old && sorted {
                    past_cutoff = true;
                    break;
                }
                tally.see(pr.key());
                read += 1;
                if !old && keep(&pr) {
                    kept.push(pr.key());
                    prs.push(pr);
                }
            }
            self.store(prs, Vec::new()).await?;
            let total = if past_cutoff { read } else { page.total };
            self.set_progress(Some(tally.progress(total)));
            match page.next {
                Some(next) if !past_cutoff => after = Some(next),
                _ => {
                    tally.counted(total.max(read));
                    break;
                }
            }
        }
        Ok(kept)
    }

    /// Fetches each of `asked` by number, by the name its Repository goes by as last read.
    /// A PR GitHub doesn't show the token, or one it shows under another Repository (the name
    /// was taken by another after a rename agentdeck hasn't seen), is missing.
    async fn fetch(&self, token: &str, asked: &[RemotePr]) -> Result<Fetched, SyncError> {
        let wanted: Vec<(String, i64)> = asked.iter().map(RemotePr::wanted).collect();
        let found = self.inner.github.prs(token, &wanted).await?;
        let mut fetched = Fetched::default();
        for (asked, found) in asked.iter().zip(found) {
            match found {
                Some(pr) if pr.key() == asked.key() => fetched.found.push(pr),
                _ => fetched.missing.push(asked.key()),
            }
        }
        Ok(fetched)
    }

    /// Stores `prs` with their Repositories, marks the `unreachable` ones, and tells clients
    /// which PRs and Repositories changed.
    async fn store(&self, prs: Vec<RemotePr>, unreachable: Vec<PrKey>) -> Result<(), SyncError> {
        if prs.is_empty() && unreachable.is_empty() {
            return Ok(());
        }
        let changed = self
            .inner
            .db
            .call(move |conn| db::pull_requests::store(conn, &prs, &unreachable))
            .await?;
        for repository in changed.repositories {
            self.inner.events.changed(Entity::Repository, repository);
        }
        for pr in changed.prs {
            self.inner.events.changed(Entity::PullRequest, pr);
        }
        Ok(())
    }

    /// Finishes the stored open PRs `prs`, which GitHub hasn't updated within the cutoff, and
    /// tells clients which changed.
    async fn finish_aged(&self, prs: Vec<PrKey>) -> Result<(), SyncError> {
        if prs.is_empty() {
            return Ok(());
        }
        let changed = self
            .inner
            .db
            .call(move |conn| db::pull_requests::finish_aged(conn, &prs))
            .await?;
        for pr in changed {
            self.inner.events.changed(Entity::PullRequest, pr);
        }
        Ok(())
    }

    /// Deletes the PRs Finished `keep_finished` ago or longer, then the Repositories nothing
    /// references any more, and tells clients which PRs and Repositories went.
    async fn prune(&self) -> Result<(), SyncError> {
        let keep =
            chrono::TimeDelta::from_std(self.inner.keep_finished).unwrap_or(chrono::TimeDelta::MAX);
        // A window longer than chrono holds deletes nothing.
        let Some(before) = Utc::now().checked_sub_signed(keep) else {
            return Ok(());
        };
        let pruned = self
            .inner
            .db
            .call(move |conn| db::pull_requests::prune(conn, before))
            .await?;
        for pr in pruned.prs {
            self.inner.events.changed(Entity::PullRequest, pr);
        }
        for repository in pruned.repositories {
            self.inner.events.changed(Entity::Repository, repository);
        }
        Ok(())
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
}

/// What fetching PRs by number found.
#[derive(Default)]
struct Fetched {
    /// The PRs GitHub showed, each as asked for.
    found: Vec<RemotePr>,
    /// The PRs it didn't show the token, by key.
    missing: Vec<PrKey>,
}

/// How far a sync's reading has got, across the list and the searches it reads in turn.
#[derive(Default)]
struct Tally {
    /// Every PR read so far, by key, each once.
    seen: Vec<PrKey>,
    /// How many PRs the lists and searches read to their end held.
    counted: u32,
}

impl Tally {
    /// Counts PR `key` as read.
    fn see(&mut self, key: PrKey) {
        if !self.seen.contains(&key) {
            self.seen.push(key);
        }
    }

    /// Adds `total`, how many PRs a list or search held, once it is read to its end.
    fn counted(&mut self, total: u32) {
        self.counted = self.counted.saturating_add(total);
    }

    /// The progress while reading a list or search holding `total` PRs.
    fn progress(&self, total: u32) -> PrSyncProgress {
        let prs_done = u32::try_from(self.seen.len()).unwrap_or(u32::MAX);
        PrSyncProgress {
            prs_done,
            prs: self.counted.saturating_add(total).max(prs_done),
            stage: PrSyncStage::Listing,
        }
    }
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

/// Whether an incremental sync fetches stored open PR `pr` by number when no search showed
/// it: it is [`unsettled`], or yours only as a reviewer or an assignee. Submitting your review
/// or being unassigned leaves every search, and is seen only by fetching the PR (spec §8).
fn recheck(pr: &RemotePr) -> bool {
    unsettled(pr) || !pr.yours.contains(&Yours::Authored)
}

/// Whether a PR last updated at `updated_at` (RFC 3339) is older than `cutoff`. One whose time
/// doesn't parse isn't: better synced than dropped.
fn older_than(updated_at: &str, cutoff: Option<DateTime<Utc>>) -> bool {
    let Some(cutoff) = cutoff else {
        return false;
    };
    DateTime::parse_from_rfc3339(updated_at).is_ok_and(|updated| updated < cutoff)
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

        pub(super) fn pr(checks: Option<ChecksState>, review: Option<ReviewState>) -> RemotePr {
            RemotePr {
                repository: github::RemoteRepository {
                    id: agentdeck_core::RepositoryId::from(1_296_269),
                    name: "acme/widgets".parse().unwrap(),
                    url: "https://github.com/acme/widgets".into(),
                    private: false,
                    archived: false,
                },
                number: 7,
                title: "Add PR sync".into(),
                url: "https://github.com/acme/widgets/pull/7".into(),
                state: PrState::Open,
                draft: false,
                head_ref: "feat/sync".into(),
                base_ref: "main".into(),
                author: None,
                yours: vec![agentdeck_core::Yours::Authored],
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

    mod recheck {
        use super::*;

        fn pr(yours: Vec<Yours>, checks: Option<ChecksState>) -> RemotePr {
            RemotePr {
                yours,
                ..unsettled::pr(checks, None)
            }
        }

        #[test]
        fn should_recheck_a_settled_pr_yours_only_as_a_reviewer_or_an_assignee() {
            for yours in [
                vec![Yours::ReviewRequested],
                vec![Yours::Assigned],
                vec![Yours::ReviewRequested, Yours::Assigned],
            ] {
                assert!(recheck(&pr(yours, Some(ChecksState::Success))));
            }
        }

        #[test]
        fn should_not_recheck_a_settled_pr_you_authored() {
            assert!(!recheck(&pr(
                vec![Yours::Authored, Yours::ReviewRequested],
                Some(ChecksState::Success)
            )));
        }

        #[test]
        fn should_recheck_an_unsettled_pr_you_authored() {
            assert!(recheck(&pr(vec![Yours::Authored], None)));
        }
    }

    mod tally {
        use super::*;

        fn key(number: i64) -> PrKey {
            PrKey {
                repository: agentdeck_core::RepositoryId::from(1),
                number,
            }
        }

        #[test]
        fn should_count_a_pr_two_searches_read_once() {
            let mut tally = Tally::default();
            tally.see(key(7));
            tally.see(key(7));

            assert_eq!(tally.progress(0).prs_done, 1);
        }

        #[test]
        fn should_add_the_total_of_the_one_reading_to_those_read_to_their_end() {
            let mut tally = Tally::default();
            tally.see(key(1));
            tally.counted(1);
            tally.see(key(2));

            let progress = tally.progress(30);

            assert_eq!((progress.prs_done, progress.prs), (2, 31));
            assert_eq!(progress.stage, PrSyncStage::Listing);
        }

        #[test]
        fn should_never_say_fewer_prs_than_it_has_read() {
            let mut tally = Tally::default();
            for number in 1..=3 {
                tally.see(key(number));
            }

            assert_eq!(tally.progress(0).prs, 3);
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
