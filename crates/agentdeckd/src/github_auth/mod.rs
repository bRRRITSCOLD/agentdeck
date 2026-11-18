//! The daemon's GitHub credentials (spec §8): where the token comes from, the saved token and
//! OAuth client ID in the system keyring, and Sign in with GitHub.
//!
//! The token is the first a source gives: a token saved in the keyring, then `GH_TOKEN`, else
//! `GITHUB_TOKEN`, then `gh auth token`. A read of the sources checks the token it finds with
//! GitHub (`viewer { login }`); one GitHub rejects doesn't send it on to the next source.
//!
//! - [`keyring`]: the keyring port, its Secret Service store and the in-memory fake.
//! - [`gh`]: `gh auth token`.
//! - [`device`]: the OAuth device flow's endpoints.
//!
//! Neither the token nor the client ID is ever in a log line, an error, an event or a
//! response.

pub mod device;
pub mod gh;
pub mod keyring;

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use agentdeck_core::ws::{DeviceOutcome, ServerMessage};
use chrono::{SecondsFormat, Utc};
use tokio::sync::watch;
use tokio::task::AbortHandle;

use crate::events::Events;
use crate::model::{DeviceSignIn, GithubAuthInfo, TokenEnvVar, TokenSource};
use crate::prsync::github::{Github, GithubError};
use device::{OAuth, Poll};
use keyring::{CLIENT_ID_ENTRY, Keyring, KeyringError, TOKEN_PREFIX, token_entry};

/// The environment variable that gives the OAuth client ID while none is saved.
pub const CLIENT_ID_VAR: &str = "AGENTDECK_GITHUB_CLIENT_ID";

/// How long a keyring call may take. A locked keyring may wait on an unlock prompt nobody
/// answers; the daemon goes on without it.
const KEYRING_TIMEOUT: Duration = Duration::from_secs(10);

/// The message of a save or a sign-in with no keyring to save in.
pub const NO_KEYRING: &str = "No system keyring is running, so agentdeck can't save a token. \
                              Set GH_TOKEN in agentdeckd's environment instead.";

/// The message of a pasted token GitHub refused.
pub const INVALID_TOKEN: &str = "GitHub didn't accept this token.";

/// The GitHub variables of the daemon's own environment, read once at start. Empty is the
/// same as unset.
#[derive(Debug, Default, Clone)]
pub struct Env {
    gh_token: Option<String>,
    github_token: Option<String>,
    client_id: Option<String>,
}

impl Env {
    /// From `GH_TOKEN`, `GITHUB_TOKEN` and [`CLIENT_ID_VAR`], as `var` reads each.
    pub fn read(var: impl Fn(&str) -> Option<String>) -> Env {
        let read = |name: &str| var(name).filter(|value| !value.trim().is_empty());
        Env {
            gh_token: read(TokenEnvVar::GhToken.name()),
            github_token: read(TokenEnvVar::GithubToken.name()),
            client_id: read(CLIENT_ID_VAR),
        }
    }

    /// The daemon's own environment.
    pub fn from_process() -> Env {
        Env::read(|name| std::env::var(name).ok())
    }

    /// The token variable that is set, `GH_TOKEN` first, and its value.
    fn token(&self) -> Option<(TokenEnvVar, &str)> {
        match (&self.gh_token, &self.github_token) {
            (Some(token), _) => Some((TokenEnvVar::GhToken, token)),
            (None, Some(token)) => Some((TokenEnvVar::GithubToken, token)),
            (None, None) => None,
        }
    }
}

/// The keyring, called off the async threads and given [`KEYRING_TIMEOUT`].
#[derive(Clone)]
struct Store(Arc<dyn Keyring>);

impl Store {
    async fn call<T: Send + 'static>(
        &self,
        call: impl FnOnce(&dyn Keyring) -> Result<T, KeyringError> + Send + 'static,
    ) -> Result<T, KeyringError> {
        let keyring = Arc::clone(&self.0);
        let blocking = tokio::task::spawn_blocking(move || call(&*keyring));
        match tokio::time::timeout(KEYRING_TIMEOUT, blocking).await {
            Ok(Ok(result)) => result,
            Ok(Err(join)) => Err(KeyringError::Failed(join.to_string())),
            Err(_) => Err(KeyringError::Unavailable(format!(
                "it didn't answer in {KEYRING_TIMEOUT:?}"
            ))),
        }
    }

    /// The saved token, if any: v1 keeps one `github.com/` entry; with more, the first by
    /// name.
    async fn saved_token(&self) -> Result<Option<String>, KeyringError> {
        self.call(|keyring| {
            let mut names = token_names(keyring)?;
            names.sort();
            for name in names {
                if let Some(token) = keyring.get(&name)? {
                    return Ok(Some(token));
                }
            }
            Ok(None)
        })
        .await
    }

    /// Whether the keyring answers, so a save can work.
    async fn probe(&self) -> Result<(), KeyringError> {
        self.call(|keyring| keyring.names().map(drop)).await
    }
}

/// Every saved token's entry name.
fn token_names(keyring: &dyn Keyring) -> Result<Vec<String>, KeyringError> {
    Ok(keyring
        .names()?
        .into_iter()
        .filter(|name| name.starts_with(TOKEN_PREFIX))
        .collect())
}

/// A token, and the source that gave it.
#[derive(Debug, PartialEq, Eq)]
struct Found {
    source: TokenSource,
    token: String,
}

/// The first source that gives a token: the keyring, the environment, then `gh`. No keyring
/// (or a keyring that fails) is the same as no saved token.
async fn find<F>(store: &Store, env: &Env, gh: impl FnOnce() -> F) -> Option<Found>
where
    F: Future<Output = Result<String, gh::NoToken>>,
{
    match store.saved_token().await {
        Ok(Some(token)) => {
            return Some(Found {
                source: TokenSource::Keyring,
                token,
            });
        }
        Ok(None) => {}
        Err(err) => tracing::info!("no saved GitHub token: {err}"),
    }
    if let Some((_, token)) = env.token() {
        return Some(Found {
            source: TokenSource::Env,
            token: token.to_owned(),
        });
    }
    match gh().await {
        Ok(token) => Some(Found {
            source: TokenSource::Gh,
            token,
        }),
        Err(err) => {
            tracing::info!("no GitHub token: {err}");
            None
        }
    }
}

/// Whether `gh` no longer gives `token`, the token it gave at the last read: after
/// `gh auth logout`, or a login to another account. `gh auth logout` doesn't revoke the token,
/// so GitHub would go on taking it.
async fn gh_moved_on<F>(token: &str, gh: impl FnOnce() -> F) -> bool
where
    F: Future<Output = Result<String, gh::NoToken>>,
{
    !matches!(gh().await, Ok(current) if current == token)
}

/// Why there's no token to sync with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NoToken {
    /// No source gives one.
    #[error("no GitHub token: sign in to GitHub in Settings, or set GH_TOKEN")]
    None,
    /// GitHub rejected the one found.
    #[error("GitHub rejected the token")]
    Rejected,
}

/// Why a change to the credentials failed. No variant carries a secret.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// A token that's blank, or that GitHub answered 401 for.
    #[error("{INVALID_TOKEN}")]
    InvalidToken,
    /// A blank client ID.
    #[error("The OAuth client ID is blank.")]
    InvalidClientId,
    /// A client ID shaped like a device flow's user code: the code pasted in by mistake.
    #[error("That looks like the sign-in code, not a client ID.")]
    UserCodeForClientId,
    /// No keyring to save in.
    #[error("{NO_KEYRING}")]
    NoKeyring,
    /// A sign-in with no OAuth client ID.
    #[error(
        "Enter the client ID of your GitHub OAuth App in Settings first: Sign in with GitHub \
         needs it."
    )]
    NoClientId,
    /// GitHub refused, or couldn't be asked; the message is GitHub's.
    #[error("{0}")]
    Github(String),
    /// The keyring answered, but failed.
    #[error("{0}")]
    Keyring(String),
}

impl From<KeyringError> for AuthError {
    fn from(err: KeyringError) -> Self {
        match err {
            KeyringError::Unavailable(_) => AuthError::NoKeyring,
            KeyringError::Failed(_) => AuthError::Keyring(err.to_string()),
        }
    }
}

/// The credentials as last read.
struct State {
    source: TokenSource,
    /// A token GitHub hasn't rejected; `None` when no source gives one or GitHub rejected it.
    token: Option<String>,
    login: Option<String>,
    scopes: Option<Vec<String>>,
    client_id_set: bool,
}

impl State {
    /// Whether GitHub rejected the token the source gives.
    fn rejected(&self) -> bool {
        self.source != TokenSource::None && self.token.is_none()
    }
}

/// The daemon's GitHub credentials; clones share them.
#[derive(Clone)]
pub struct GithubAuth {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    env: Env,
    github: Github,
    oauth: OAuth,
    events: Events,
    state: Mutex<State>,
    /// Held for a read of the sources or a change to the saved token, so they never
    /// interleave.
    reading: tokio::sync::Mutex<()>,
    /// The sign-in polling now, if any.
    sign_in: Mutex<Option<AbortHandle>>,
    /// Bumped each time a route or a sign-in changes the token, so PR sync syncs at once.
    changes: watch::Sender<u64>,
}

impl GithubAuth {
    /// Credentials kept in `keyring`, with `env`'s variables under them; tokens are checked
    /// with `github`, and sign-ins go through `oauth`. Nothing is read until [`Self::reread`].
    pub fn new(
        keyring: Box<dyn Keyring>,
        env: Env,
        github: Github,
        oauth: OAuth,
        events: Events,
    ) -> GithubAuth {
        let client_id_set = env.client_id.is_some();
        GithubAuth {
            inner: Arc::new(Inner {
                store: Store(Arc::from(keyring)),
                env,
                github,
                oauth,
                events,
                state: Mutex::new(State {
                    source: TokenSource::None,
                    token: None,
                    login: None,
                    scopes: None,
                    client_id_set,
                }),
                reading: tokio::sync::Mutex::new(()),
                sign_in: Mutex::new(None),
                changes: watch::Sender::new(0),
            }),
        }
    }

    /// The token, unless no source gave one or GitHub rejected it.
    pub fn token(&self) -> Option<String> {
        self.state().token.clone()
    }

    /// The token to call GitHub with: the one last read, unless there's none or `gh` gave it
    /// and no longer does; then the sources are read again, as [`Self::reread`] does.
    pub async fn current_token(&self) -> Result<String, NoToken> {
        let (source, token) = {
            let state = self.state();
            (state.source, state.token.clone())
        };
        match token {
            Some(token)
                if source != TokenSource::Gh || !gh_moved_on(&token, gh::gh_auth_token).await =>
            {
                Ok(token)
            }
            _ => self.reread().await,
        }
    }

    /// `GET /v1/github/auth`.
    pub fn info(&self) -> GithubAuthInfo {
        let state = self.state();
        GithubAuthInfo {
            source: state.source,
            env_var: self.inner.env.token().map(|(var, _)| var),
            login: state.login.clone(),
            scopes: state.scopes.clone(),
            rejected: state.rejected(),
            client_id_set: state.client_id_set,
        }
    }

    /// A feed that changes each time a route or a sign-in changes the token.
    pub fn token_changes(&self) -> watch::Receiver<u64> {
        self.inner.changes.subscribe()
    }

    /// Reads the sources again, tells clients when the source, the login or whether GitHub
    /// rejected the token changed, and
    /// returns the token, unless there's none or GitHub rejected it.
    pub async fn reread(&self) -> Result<String, NoToken> {
        if self.read().await {
            self.publish(None, None);
        }
        let state = self.state();
        match (&state.token, state.source) {
            (Some(token), _) => Ok(token.clone()),
            (None, TokenSource::None) => Err(NoToken::None),
            (None, _) => Err(NoToken::Rejected),
        }
    }

    /// Forgets the token: GitHub rejected it, though it took it a moment ago. Tells clients,
    /// unless it was already forgotten.
    pub fn rejected(&self) {
        let changed = {
            let mut state = self.state();
            let changed = state.token.is_some();
            state.token = None;
            state.login = None;
            state.scopes = None;
            changed
        };
        if changed {
            self.publish(None, None);
        }
    }

    /// Reads the sources and the client ID; returns whether the source, the login or whether
    /// GitHub rejected the token changed.
    async fn read(&self) -> bool {
        let _reading = self.inner.reading.lock().await;
        let found = find(&self.inner.store, &self.inner.env, gh::gh_auth_token).await;
        let client_id_set = self.client_id().await.is_some();
        let (source, token, login, scopes) = match found {
            None => (TokenSource::None, None, None, None),
            Some(Found { source, token }) => match self.inner.github.viewer(&token).await {
                Ok(viewer) => (source, Some(token), Some(viewer.login), viewer.scopes),
                Err(GithubError::Unauthorized) => {
                    tracing::info!("GitHub rejected the token from {source:?}");
                    (source, None, None, None)
                }
                Err(err) => {
                    tracing::warn!("couldn't check the GitHub token: {err}");
                    // The same token as before keeps what GitHub last said about it.
                    let state = self.state();
                    if state.token.as_deref() == Some(token.as_str()) {
                        (
                            source,
                            Some(token),
                            state.login.clone(),
                            state.scopes.clone(),
                        )
                    } else {
                        (source, Some(token), None, None)
                    }
                }
            },
        };
        let mut state = self.state();
        let changed = state.source != source
            || state.login != login
            || state.rejected() != (source != TokenSource::None && token.is_none());
        *state = State {
            source,
            token,
            login,
            scopes,
            client_id_set,
        };
        changed
    }

    /// Checks `token` with GitHub and saves it in the keyring as `github.com/<login>`,
    /// deleting any other saved token, then tells clients and PR sync.
    pub async fn save_token(&self, token: &str) -> Result<(), AuthError> {
        self.save_token_quietly(token).await?;
        self.changed_token(None, None);
        Ok(())
    }

    async fn save_token_quietly(&self, token: &str) -> Result<(), AuthError> {
        let token = token.trim().to_owned();
        if token.is_empty() {
            return Err(AuthError::InvalidToken);
        }
        let store = &self.inner.store;
        store.probe().await?;
        let viewer = match self.inner.github.viewer(&token).await {
            Ok(viewer) => viewer,
            Err(GithubError::Unauthorized) => return Err(AuthError::InvalidToken),
            Err(err) => return Err(AuthError::Github(err.to_string())),
        };
        let _reading = self.inner.reading.lock().await;
        let name = token_entry(&viewer.login);
        let saved = token.clone();
        store
            .call(move |keyring| {
                keyring.set(&name, &saved)?;
                for other in token_names(keyring)? {
                    if other != name {
                        keyring.delete(&other)?;
                    }
                }
                Ok(())
            })
            .await?;
        // The saved token is the first source: what a read would find now.
        let mut state = self.state();
        state.source = TokenSource::Keyring;
        state.token = Some(token);
        state.login = Some(viewer.login);
        state.scopes = viewer.scopes;
        Ok(())
    }

    /// Deletes the saved token (sign out), reads the sources again, then tells clients and
    /// PR sync. With no keyring there's nothing saved to delete.
    pub async fn delete_token(&self) -> Result<(), AuthError> {
        let deleted = self
            .inner
            .store
            .call(|keyring| {
                for name in token_names(keyring)? {
                    keyring.delete(&name)?;
                }
                Ok(())
            })
            .await;
        match deleted {
            Ok(()) | Err(KeyringError::Unavailable(_)) => {}
            Err(err) => return Err(err.into()),
        }
        self.read().await;
        self.changed_token(None, None);
        Ok(())
    }

    /// Saves the OAuth client ID in the keyring, then tells clients.
    pub async fn save_client_id(&self, client_id: &str) -> Result<(), AuthError> {
        let client_id = client_id.trim().to_owned();
        if client_id.is_empty() {
            return Err(AuthError::InvalidClientId);
        }
        if looks_like_user_code(&client_id) {
            return Err(AuthError::UserCodeForClientId);
        }
        self.inner
            .store
            .call(move |keyring| keyring.set(CLIENT_ID_ENTRY, &client_id))
            .await?;
        self.state().client_id_set = true;
        self.publish(None, None);
        Ok(())
    }

    /// Deletes the saved OAuth client ID, then tells clients.
    pub async fn delete_client_id(&self) -> Result<(), AuthError> {
        match self
            .inner
            .store
            .call(|keyring| keyring.delete(CLIENT_ID_ENTRY))
            .await
        {
            Ok(()) | Err(KeyringError::Unavailable(_)) => {}
            Err(err) => return Err(err.into()),
        }
        self.state().client_id_set = self.inner.env.client_id.is_some();
        self.publish(None, None);
        Ok(())
    }

    /// The saved client ID, else [`CLIENT_ID_VAR`]'s.
    async fn client_id(&self) -> Option<String> {
        match self
            .inner
            .store
            .call(|keyring| keyring.get(CLIENT_ID_ENTRY))
            .await
        {
            Ok(Some(client_id)) => return Some(client_id),
            Ok(None) => {}
            Err(err) => tracing::debug!("no saved OAuth client ID: {err}"),
        }
        self.inner.env.client_id.clone()
    }

    /// Starts Sign in with GitHub, cancelling one already running: asks GitHub for a device
    /// code, then polls for the token in the background. How it ends comes as a
    /// `github_auth` event.
    pub async fn start_sign_in(&self) -> Result<DeviceSignIn, AuthError> {
        self.cancel_sign_in();
        self.inner.store.probe().await?;
        let client_id = self.client_id().await.ok_or(AuthError::NoClientId)?;
        let code = self
            .inner
            .oauth
            .device_code(&client_id)
            .await
            .map_err(|err| AuthError::Github(err.0))?;
        tracing::info!(
            "Sign in with GitHub started: the code expires in {} s, polled every {} s",
            code.expires_in,
            code.interval
        );
        let expires_in = Duration::from_secs(code.expires_in);
        let deadline = tokio::time::Instant::now() + expires_in;
        let expires_at = Utc::now()
            + chrono::TimeDelta::from_std(expires_in).unwrap_or(chrono::TimeDelta::zero());
        let sign_in = DeviceSignIn {
            user_code: code.user_code.clone(),
            verification_uri: code.verification_uri.clone(),
            expires_at: expires_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        };
        let auth = self.clone();
        let polling =
            tokio::spawn(async move { auth.poll_sign_in(client_id, code, deadline).await });
        let previous = self.sign_in().replace(polling.abort_handle());
        if let Some(previous) = previous {
            previous.abort();
        }
        Ok(sign_in)
    }

    /// Cancels the sign-in running now, if any. No event.
    pub fn cancel_sign_in(&self) {
        if let Some(polling) = self.sign_in().take() {
            polling.abort();
        }
    }

    /// Polls for the token at GitHub's interval until the user approves or denies, GitHub
    /// fails, or `deadline` passes; saves the token, then sends how it ended.
    async fn poll_sign_in(
        self,
        client_id: String,
        code: device::DeviceCode,
        deadline: tokio::time::Instant,
    ) {
        let mut slow_downs = 0;
        // Logs each poll, and how the sign-in ended or that it was cancelled (dropped).
        let mut log = PollLog::default();
        let (outcome, message) = loop {
            let wake = tokio::time::Instant::now() + device::poll_wait(code.interval, slow_downs);
            if wake >= deadline {
                tokio::time::sleep_until(deadline).await;
                break (DeviceOutcome::Expired, None);
            }
            tokio::time::sleep_until(wake).await;
            let poll = self.inner.oauth.poll(&client_id, &code.device_code).await;
            log.record(&poll);
            match poll {
                Poll::Pending | Poll::Unreachable(_) => {}
                Poll::SlowDown => slow_downs += 1,
                Poll::Token(token) => match self.save_token_quietly(&token).await {
                    Ok(()) => break (DeviceOutcome::SignedIn, None),
                    Err(err) => break (DeviceOutcome::Failed, Some(err.to_string())),
                },
                Poll::Expired => break (DeviceOutcome::Expired, None),
                Poll::Denied => break (DeviceOutcome::Denied, None),
                Poll::Failed(message) => break (DeviceOutcome::Failed, Some(message)),
            }
        };
        log.ended(outcome);
        if outcome == DeviceOutcome::SignedIn {
            self.changed_token(Some(outcome), None);
        } else {
            self.publish(Some(outcome), message);
        }
    }

    /// Tells clients the token changed through a route or a sign-in, and PR sync to sync.
    fn changed_token(&self, device: Option<DeviceOutcome>, message: Option<String>) {
        self.publish(device, message);
        self.inner
            .changes
            .send_modify(|generation| *generation += 1);
    }

    fn publish(&self, device: Option<DeviceOutcome>, message: Option<String>) {
        self.inner
            .events
            .publish(ServerMessage::GithubAuth { device, message });
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // Nothing panics while holding the lock.
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn sign_in(&self) -> MutexGuard<'_, Option<AbortHandle>> {
        self.inner
            .sign_in
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Whether `client_id` has the shape of a device flow's user code (`WDJB-MJHT`: 4 capitals or
/// digits, a hyphen, 4 more), which no OAuth client ID has.
fn looks_like_user_code(client_id: &str) -> bool {
    let bytes = client_id.as_bytes();
    let code_char = |byte: &u8| byte.is_ascii_uppercase() || byte.is_ascii_digit();
    bytes.len() == 9
        && bytes[4] == b'-'
        && bytes[..4].iter().all(code_char)
        && bytes[5..].iter().all(code_char)
}

/// A sign-in's polls, for the log: a poll that failed in transit at warn, each pending answer
/// at debug, then how it ended at info, or, dropped before that (cancelled, or replaced by a
/// new sign-in), that it was cancelled. Never logs a token.
#[derive(Default)]
struct PollLog {
    polls: u32,
    pending: u32,
    unreachable: u32,
    ended: bool,
}

impl PollLog {
    fn record(&mut self, poll: &Poll) {
        self.polls += 1;
        match poll {
            Poll::Pending => {
                self.pending += 1;
                tracing::debug!(
                    "Sign in with GitHub: not approved yet ({} pending answers)",
                    self.pending
                );
            }
            Poll::Unreachable(err) => {
                self.unreachable += 1;
                tracing::warn!("Sign in with GitHub: couldn't poll GitHub, trying again: {err}");
            }
            Poll::SlowDown => tracing::debug!("Sign in with GitHub: GitHub asked to poll slower"),
            Poll::Token(_) | Poll::Expired | Poll::Denied | Poll::Failed(_) => {}
        }
    }

    fn ended(&mut self, outcome: DeviceOutcome) {
        self.ended = true;
        tracing::info!("Sign in with GitHub ended: {outcome:?} after {self}");
    }
}

impl std::fmt::Display for PollLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = if self.polls == 1 { "" } else { "s" };
        write!(
            f,
            "{} poll{s} ({} pending, {} unreachable)",
            self.polls, self.pending, self.unreachable
        )
    }
}

impl Drop for PollLog {
    fn drop(&mut self) {
        if !self.ended {
            tracing::info!("Sign in with GitHub cancelled after {self}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keyring::Memory;

    fn store(entries: &[(&str, &str)]) -> Store {
        let memory = Memory::default();
        for (name, secret) in entries {
            memory.set(name, secret).unwrap();
        }
        Store(Arc::new(memory))
    }

    /// A keyring with no Secret Service behind it.
    struct Unavailable;

    impl Keyring for Unavailable {
        fn names(&self) -> Result<Vec<String>, KeyringError> {
            Err(KeyringError::Unavailable("no session bus".into()))
        }
        fn get(&self, _: &str) -> Result<Option<String>, KeyringError> {
            Err(KeyringError::Unavailable("no session bus".into()))
        }
        fn set(&self, _: &str, _: &str) -> Result<(), KeyringError> {
            Err(KeyringError::Unavailable("no session bus".into()))
        }
        fn delete(&self, _: &str) -> Result<(), KeyringError> {
            Err(KeyringError::Unavailable("no session bus".into()))
        }
    }

    fn env(vars: &[(&str, &str)]) -> Env {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        Env::read(|name| {
            vars.iter()
                .find(|(var, _)| var == name)
                .map(|(_, value)| value.clone())
        })
    }

    async fn gh_token() -> Result<String, gh::NoToken> {
        Ok("gho_gh".into())
    }

    async fn gh_logged_out() -> Result<String, gh::NoToken> {
        Err(gh::NoToken("no oauth token found for github.com".into()))
    }

    fn found(source: TokenSource, token: &str) -> Option<Found> {
        Some(Found {
            source,
            token: token.into(),
        })
    }

    mod find {
        use super::*;

        #[tokio::test]
        async fn should_take_the_saved_token_over_the_environment_and_gh() {
            let store = store(&[("github.com/octocat", "ghp_saved")]);
            let env = env(&[("GH_TOKEN", "ghp_env")]);

            assert_eq!(
                find(&store, &env, gh_token).await,
                found(TokenSource::Keyring, "ghp_saved")
            );
        }

        #[tokio::test]
        async fn should_take_gh_token_before_github_token() {
            let env = env(&[("GITHUB_TOKEN", "ghp_github"), ("GH_TOKEN", "ghp_gh")]);

            assert_eq!(
                find(&store(&[]), &env, gh_token).await,
                found(TokenSource::Env, "ghp_gh")
            );
        }

        #[tokio::test]
        async fn should_take_github_token_when_gh_token_is_empty() {
            let env = env(&[("GH_TOKEN", ""), ("GITHUB_TOKEN", "ghp_github")]);

            assert_eq!(
                find(&store(&[]), &env, gh_token).await,
                found(TokenSource::Env, "ghp_github")
            );
        }

        #[tokio::test]
        async fn should_fall_back_to_gh_without_a_saved_token_or_variable() {
            assert_eq!(
                find(&store(&[]), &env(&[]), gh_token).await,
                found(TokenSource::Gh, "gho_gh")
            );
        }

        #[tokio::test]
        async fn should_not_take_the_client_id_for_a_token() {
            let store = store(&[(CLIENT_ID_ENTRY, "Iv1.abc")]);

            assert_eq!(find(&store, &env(&[]), gh_logged_out).await, None);
        }

        #[tokio::test]
        async fn should_go_on_to_the_environment_without_a_keyring() {
            let env = env(&[("GH_TOKEN", "ghp_env")]);

            assert_eq!(
                find(&Store(Arc::new(Unavailable)), &env, gh_token).await,
                found(TokenSource::Env, "ghp_env")
            );
        }

        #[tokio::test]
        async fn should_find_none_when_no_source_gives_one() {
            assert_eq!(find(&store(&[]), &env(&[]), gh_logged_out).await, None);
        }
    }

    mod gh_moved_on {
        use super::*;

        #[tokio::test]
        async fn should_stay_while_gh_gives_the_same_token() {
            assert!(!gh_moved_on("gho_gh", gh_token).await);
        }

        #[tokio::test]
        async fn should_move_on_once_gh_logs_out() {
            assert!(gh_moved_on("gho_gh", gh_logged_out).await);
        }

        #[tokio::test]
        async fn should_move_on_once_gh_gives_another_token() {
            assert!(gh_moved_on("gho_old", gh_token).await);
        }
    }

    mod env {
        use super::*;

        #[test]
        fn should_read_a_blank_variable_as_unset() {
            let env = env(&[("GH_TOKEN", "  "), (CLIENT_ID_VAR, "")]);

            assert_eq!(env.token(), None);
            assert_eq!(env.client_id, None);
        }
    }

    mod looks_like_user_code {
        use super::*;

        #[test]
        fn should_spot_a_sign_in_code() {
            assert!(looks_like_user_code("WDJB-MJHT"));
            assert!(looks_like_user_code("ABCD-1234"));
        }

        #[test]
        fn should_pass_a_client_id() {
            for client_id in [
                "Ov23liAbCdEf12345678",
                "Iv1.0123456789abcdef",
                "wdjb-mjht",
                "ABCD1234",
            ] {
                assert!(!looks_like_user_code(client_id), "{client_id}");
            }
        }
    }

    mod poll_log {
        use std::io;

        use super::*;

        /// What a test's subscriber wrote.
        #[derive(Clone, Default)]
        struct Written(Arc<Mutex<Vec<u8>>>);

        impl io::Write for Written {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        /// Everything `run` logs, debug and up.
        fn logged(run: impl FnOnce()) -> String {
            let written = Written::default();
            let writer = written.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_ansi(false)
                .with_max_level(tracing::Level::DEBUG)
                .with_writer(move || writer.clone())
                .finish();
            tracing::subscriber::with_default(subscriber, run);
            let bytes = written.0.lock().unwrap().clone();
            String::from_utf8(bytes).unwrap()
        }

        #[test]
        fn should_warn_with_the_error_when_a_poll_fails_in_transit() {
            let logged = logged(|| {
                let mut log = PollLog::default();
                log.record(&Poll::Unreachable(
                    "error sending request: connection refused".into(),
                ));
                log.ended(DeviceOutcome::Expired);
            });

            let warning = logged
                .lines()
                .find(|line| line.contains("WARN"))
                .expect("a warning");
            assert!(
                warning.contains("error sending request: connection refused"),
                "{logged}"
            );
        }

        #[test]
        fn should_count_pending_answers_at_debug() {
            let logged = logged(|| {
                let mut log = PollLog::default();
                log.record(&Poll::Pending);
                log.record(&Poll::Pending);
                log.ended(DeviceOutcome::Expired);
            });

            let pending: Vec<&str> = logged
                .lines()
                .filter(|line| line.contains("DEBUG") && line.contains("pending"))
                .collect();
            assert_eq!(pending.len(), 2, "{logged}");
            assert!(pending[1].contains('2'), "{logged}");
        }

        #[test]
        fn should_sum_the_polls_up_when_it_ends() {
            let logged = logged(|| {
                let mut log = PollLog::default();
                log.record(&Poll::Pending);
                log.record(&Poll::Unreachable("timed out".into()));
                log.record(&Poll::Token("gho_secret".into()));
                log.ended(DeviceOutcome::SignedIn);
            });

            let end = logged
                .lines()
                .find(|line| line.contains("ended"))
                .expect("the end");
            assert!(end.contains("INFO"), "{logged}");
            assert!(
                end.contains("SignedIn after 3 polls (1 pending, 1 unreachable)"),
                "{logged}"
            );
            assert!(!logged.contains("gho_secret"), "{logged}");
        }

        #[test]
        fn should_say_it_was_cancelled_when_dropped_before_it_ended() {
            let logged = logged(|| {
                let mut log = PollLog::default();
                log.record(&Poll::Pending);
                drop(log);
            });

            assert!(
                logged.contains(
                    "Sign in with GitHub cancelled after 1 poll (1 pending, 0 unreachable)"
                ),
                "{logged}"
            );
        }

        #[test]
        fn should_not_say_cancelled_once_it_ended() {
            let logged = logged(|| PollLog::default().ended(DeviceOutcome::Denied));

            assert!(!logged.contains("cancelled"), "{logged}");
        }
    }

    mod auth_error {
        use super::*;

        #[test]
        fn should_read_no_keyring_as_no_keyring() {
            let err = AuthError::from(KeyringError::Unavailable("no session bus".into()));

            assert!(matches!(err, AuthError::NoKeyring));
            assert_eq!(err.to_string(), NO_KEYRING);
        }
    }
}
