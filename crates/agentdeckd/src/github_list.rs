//! The GitHub repository list (spec §8): every repository the sign-in can reach, as the Link
//! picker's All your repositories offers them.
//!
//! Nothing loads until a client asks with [`GithubList::refresh`], which the GUI does each
//! time a picker opens. A load reads `GET /user/repos` 100 a page, by `owner/name`: page 1
//! first, whose `Link` header names the last page, then the last page, so the total is exact
//! early, then the pages between, [`PAGES_IN_FLIGHT`] at once. About 1,100 repositories take
//! 12 requests. Each page is served as it arrives while no list is held; once one is, a
//! refresh runs quietly under it and replaces it whole. One load runs at a time.
//!
//! The list is kept in memory for the run, for the sign-in it was read with: when the token
//! changes, it is dropped and, if a client had asked for it, loaded again. Every change to it
//! or its progress sends `changed github_repositories`. `search/repositories` is never called:
//! it allows 30 requests a minute and misses repositories you only collaborate on.

use std::sync::{Arc, Mutex, MutexGuard};

use agentdeck_core::ws::Entity;
use futures_util::{StreamExt as _, TryStreamExt as _};

use crate::events::Events;
use crate::github_auth::{GithubAuth, NoToken};
use crate::model::{GithubListStatus, GithubRepositoryList, Repository};
use crate::prsync::github::{Github, GithubError, REPOS_PAGE_SIZE, RemoteRepository, ReposPage};

/// How many pages after the first are read at once.
const PAGES_IN_FLIGHT: usize = 4;

/// The id a [`Entity::GithubRepositories`] event names: there is one list.
const LIST_ID: &str = "list";

/// The list, kept in memory for the run; clones share it.
#[derive(Clone)]
pub struct GithubList {
    inner: Arc<Inner>,
}

struct Inner {
    events: Events,
    auth: GithubAuth,
    github: Github,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Bumped when the sign-in changes, so a load from before stores nothing.
    generation: u64,
    /// Whether a load of this generation runs.
    loading: bool,
    /// Whether a client has asked for the list since the sign-in last changed.
    asked: bool,
    /// The last list loaded whole.
    held: Option<Vec<Repository>>,
    /// While no list is held, the first load's pages that have arrived, by number from 1.
    pages: Vec<Option<Vec<Repository>>>,
    /// The first load's last page number, once page 1 has answered.
    last: Option<u32>,
    /// Why the last load failed; cleared when one succeeds.
    failed: Option<String>,
    /// The last refresh found no token.
    signed_out: bool,
}

impl State {
    /// The list as `GET /v1/github/repositories` answers it.
    fn list(&self) -> GithubRepositoryList {
        let (status, repositories, total) = match &self.held {
            Some(held) => (GithubListStatus::Loaded, held.clone(), Some(len(held))),
            None if self.loading => {
                let repositories: Vec<Repository> =
                    self.pages.iter().flatten().flatten().cloned().collect();
                (GithubListStatus::Loading, repositories, self.estimate())
            }
            None if self.failed.is_some() => (GithubListStatus::Failed, Vec::new(), None),
            None if self.signed_out => (GithubListStatus::SignedOut, Vec::new(), None),
            None => (GithubListStatus::NotLoaded, Vec::new(), None),
        };
        GithubRepositoryList {
            status,
            loaded: len(&repositories),
            total,
            error: self.failed.clone(),
            repositories,
        }
    }

    /// About how many repositories the first load will hold: every page before the last full,
    /// and the last one as it came, or full until it has.
    fn estimate(&self) -> Option<u32> {
        let last = self.last?;
        let last_page = (self.pages.get(last as usize - 1))
            .and_then(Option::as_ref)
            .map_or(REPOS_PAGE_SIZE, |page| len(page));
        Some((last - 1) * REPOS_PAGE_SIZE + last_page)
    }
}

fn len<T>(items: &[T]) -> u32 {
    u32::try_from(items.len()).unwrap_or(u32::MAX)
}

fn repository(found: RemoteRepository) -> Repository {
    Repository {
        id: found.id,
        name: found.name,
        url: found.url,
        private: found.private,
        archived: found.archived,
    }
}

impl GithubList {
    /// A list read from `github` with `auth`'s token, announced on `events`. Watches the
    /// token for changes, so must be called inside a tokio runtime.
    pub fn new(events: Events, auth: GithubAuth, github: Github) -> GithubList {
        let list = GithubList {
            inner: Arc::new(Inner {
                events,
                auth,
                github,
                state: Mutex::new(State::default()),
            }),
        };
        let watching = list.clone();
        let mut changes = list.inner.auth.token_changes();
        tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                watching.sign_in_changed().await;
            }
        });
        list
    }

    /// The list as it stands, with its progress.
    pub fn list(&self) -> GithubRepositoryList {
        self.state().list()
    }

    /// Loads the list again in the background, unless a load runs already. With no token it
    /// reads signed out; with a token GitHub rejects, failed.
    pub async fn refresh(&self) {
        let token = self.inner.auth.current_token().await;
        let (generation, quiet) = {
            let mut state = self.state();
            state.asked = true;
            if state.loading {
                return;
            }
            let quiet = token.is_ok() && state.held.is_some();
            match &token {
                Ok(_) => {
                    state.loading = true;
                    state.signed_out = false;
                    if !quiet {
                        state.pages.clear();
                        state.last = None;
                        state.failed = None;
                    }
                }
                Err(NoToken::None) => {
                    *state = State {
                        generation: state.generation,
                        asked: true,
                        signed_out: true,
                        ..State::default()
                    };
                }
                Err(NoToken::Rejected) => {
                    *state = State {
                        generation: state.generation,
                        asked: true,
                        failed: Some(GithubError::Unauthorized.to_string()),
                        ..State::default()
                    };
                }
            }
            (state.generation, quiet)
        };
        if !quiet {
            self.changed();
        }
        if let Ok(token) = token {
            let this = self.clone();
            tokio::spawn(async move { this.load(generation, token).await });
        }
    }

    /// Drops the list read with the token before, and loads it again with the new one if a
    /// client had asked for it.
    async fn sign_in_changed(&self) {
        let asked = {
            let mut state = self.state();
            let asked = state.asked;
            *state = State {
                generation: state.generation + 1,
                ..State::default()
            };
            asked
        };
        self.changed();
        if asked {
            self.refresh().await;
        }
    }

    /// Reads every page with `token` and keeps the list, unless the sign-in changed meanwhile.
    /// A list held already is replaced whole, and a failure keeps it, saying why; either sends
    /// `changed` only when what clients read differs.
    async fn load(&self, generation: u64, token: String) {
        let loaded = self.read_pages(generation, token).await;
        let changed = {
            let mut state = self.state();
            if state.generation != generation {
                return;
            }
            let before = state.held.is_some().then(|| state.list());
            state.loading = false;
            state.pages.clear();
            state.last = None;
            match loaded {
                Ok(list) => {
                    state.held = Some(list);
                    state.failed = None;
                }
                Err(err) => {
                    tracing::warn!("loading the GitHub repository list: {err}");
                    state.failed = Some(err.to_string());
                }
            }
            before.is_none_or(|before| before != state.list())
        };
        if changed {
            self.changed();
        }
    }

    /// Every page, in order: page 1, then the last, then those between. While no list is held,
    /// each is kept as it arrives.
    async fn read_pages(
        &self,
        generation: u64,
        token: String,
    ) -> Result<Vec<Repository>, GithubError> {
        let (first, token) = self.first_page(token).await?;
        let last = first.last.unwrap_or(1).max(1);
        let mut pages: Vec<Option<Vec<Repository>>> = vec![None; last as usize];
        self.arrived(generation, &mut pages, 1, first, Some(last));
        let order = (last > 1).then_some(last).into_iter().chain(2..last);
        let github = &self.inner.github;
        let token = token.as_str();
        let mut reads = futures_util::stream::iter(order.map(|page| async move {
            github
                .user_repos_page(token, page)
                .await
                .map(|read| (page, read))
        }))
        .buffer_unordered(PAGES_IN_FLIGHT);
        while let Some((page, read)) = reads.try_next().await.inspect_err(|err| {
            if matches!(err, GithubError::Unauthorized) {
                self.inner.auth.rejected();
            }
        })? {
            self.arrived(generation, &mut pages, page, read, None);
        }
        Ok(pages.into_iter().flatten().flatten().collect())
    }

    /// Page 1 and the token that read it: a 401 reads the token's sources again and tries
    /// once more, then counts the token as rejected.
    async fn first_page(&self, token: String) -> Result<(ReposPage, String), GithubError> {
        let github = &self.inner.github;
        match github.user_repos_page(&token, 1).await {
            Err(GithubError::Unauthorized) => {
                let fresh =
                    (self.inner.auth.reread().await).map_err(|_| GithubError::Unauthorized)?;
                match github.user_repos_page(&fresh, 1).await {
                    Err(GithubError::Unauthorized) => {
                        self.inner.auth.rejected();
                        Err(GithubError::Unauthorized)
                    }
                    read => read.map(|page| (page, fresh)),
                }
            }
            read => read.map(|page| (page, token)),
        }
    }

    /// Keeps page `page` in `pages`, and, while no list is held and the sign-in is the same,
    /// serves it at once; `last` is the last page's number, read from page 1.
    fn arrived(
        &self,
        generation: u64,
        pages: &mut [Option<Vec<Repository>>],
        page: u32,
        read: ReposPage,
        last: Option<u32>,
    ) {
        let read: Vec<Repository> = read.repositories.into_iter().map(repository).collect();
        let at = page as usize - 1;
        {
            let mut state = self.state();
            if state.generation == generation && state.held.is_none() {
                state.pages.resize(pages.len(), None);
                state.pages[at] = Some(read.clone());
                if last.is_some() {
                    state.last = last;
                }
            } else {
                pages[at] = Some(read);
                return;
            }
        }
        pages[at] = Some(read);
        self.changed();
    }

    fn changed(&self) {
        self.inner
            .events
            .changed(Entity::GithubRepositories, LIST_ID);
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
