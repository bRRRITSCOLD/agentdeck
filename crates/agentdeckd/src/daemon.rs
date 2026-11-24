//! Running the daemon: lock, database, PR sync, HTTP server, `daemon.json`, then serve until a
//! signal (spec §2).

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use agentdeck_core::DaemonInfo;
use anyhow::Context;
use dropshot::{ConfigDropshot, ServerBuilder};
use tokio::signal::unix::{SignalKind, signal};

use crate::api::{ApiContext, api};
use crate::config::{self, Config};
use crate::db::Db;
use crate::events::Events;
use crate::github_auth::{self, GithubAuth, device, keyring};
use crate::instance::Instance;
use crate::paths::Paths;
use crate::prsync::{self, PrSync};
use crate::suggestions::Suggestions;
use crate::terminal::Terminals;
use crate::{agents, auth, logging};

/// Starts the daemon and serves until SIGINT or SIGTERM.
pub fn run() -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    logging::init(&paths)?;
    let instance = Instance::acquire(&paths)?;
    let runtime = tokio::runtime::Runtime::new().context("starting tokio")?;
    let served = runtime.block_on(serve(&paths, &instance));
    // Dropping the runtime would wait for the database actor, which outlives every live pane:
    // their reader threads hold it until their processes exit, and those exit only once this
    // process does and their PTYs hang up. A grace period lets queries in flight finish.
    runtime.shutdown_timeout(SHUTDOWN_GRACE);
    served
}

/// How long queries in flight get to finish at shutdown.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);

/// The largest request body any route takes (spec §4), where dropshot's default is 1 KiB:
/// room for a pasted log or a long prompt sent with `input`, a layout of many splits
/// or every PR a user has in one close. The API is loopback-only and authenticated.
const REQUEST_BODY_MAX_BYTES: usize = 1024 * 1024;

async fn serve(paths: &Paths, instance: &Instance) -> anyhow::Result<()> {
    std::fs::create_dir_all(&paths.data)
        .with_context(|| format!("creating {}", paths.data.display()))?;
    let db = Db::open(&paths.database())?;
    let github_api = std::env::var(prsync::ENDPOINT_VAR)
        .unwrap_or_else(|_| prsync::github::DEFAULT_ENDPOINT.into());
    let github_rest = std::env::var(prsync::REST_ENDPOINT_VAR)
        .unwrap_or_else(|_| prsync::github::DEFAULT_REST_ENDPOINT.into());
    let events = Events::new();
    let full_every = std::env::var(prsync::FULL_SYNC_VAR)
        .ok()
        .and_then(|secs| secs.parse().ok())
        .map_or(prsync::FULL_SYNC_EVERY, Duration::from_secs);
    let keep_finished = std::env::var(prsync::FINISHED_VAR)
        .ok()
        .and_then(|secs| secs.parse().ok())
        .map_or(prsync::KEEP_FINISHED, Duration::from_secs);
    let settings = Config::load(
        paths.config_file(),
        std::env::var(config::PR_MAX_AGE_VAR).ok().as_deref(),
    );
    let timeout = std::env::var(prsync::TIMEOUT_VAR)
        .ok()
        .and_then(|ms| ms.parse().ok())
        .map_or(prsync::github::TIMEOUT, Duration::from_millis);
    let github = prsync::github::Github::new(github_api, github_rest, timeout)?;
    let oauth = device::OAuth::new(
        std::env::var(device::OAUTH_VAR).unwrap_or_else(|_| device::DEFAULT_OAUTH.into()),
    )?;
    let credentials = GithubAuth::new(
        keyring::from_env(std::env::var(keyring::KEYRING_VAR).ok().as_deref()),
        github_auth::Env::from_process(),
        github.clone(),
        oauth,
        events.clone(),
    );
    let prs = PrSync::start(
        db.clone(),
        events.clone(),
        github.clone(),
        credentials.clone(),
        full_every,
        keep_finished,
        settings.clone(),
    );
    let terminals = Terminals::new(db.clone(), events.clone(), paths.rules_dir());
    terminals.run_state_timer();
    let restarter = agents::restart::Restarter {
        db: db.clone(),
        terminals: terminals.clone(),
        events: events.clone(),
    };
    agents::resume::resume_panes(&restarter)
        .await
        .map_err(|err| anyhow::anyhow!("resuming panes: {err}"))?;
    let suggestions = Suggestions::new(
        db.clone(),
        events.clone(),
        credentials.clone(),
        github.clone(),
    );
    suggestions.watch();
    let token = auth::new_token();
    let context = ApiContext {
        db,
        token: token.clone(),
        started: Instant::now(),
        prs,
        github: credentials,
        github_api: github,
        settings,
        terminals: terminals.clone(),
        events,
        suggestions,
    };
    let config = ConfigDropshot {
        bind_address: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        default_request_body_max_bytes: REQUEST_BODY_MAX_BYTES,
        ..Default::default()
    };
    let server = ServerBuilder::new(api(), context, logging::dropshot_logger())
        .config(config)
        .start()
        .map_err(|err| anyhow::anyhow!("starting the HTTP server: {err}"))?;

    let port = server.local_addr().port();
    instance.publish(&DaemonInfo {
        port,
        token,
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        api_version: agentdeck_core::API_VERSION,
    })?;
    tracing::info!("agentdeckd listening on 127.0.0.1:{port}");

    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
    tracing::info!("agentdeckd shutting down");
    // Clients stop reaching it now, not once it exits; the lock stays held until then, so a
    // daemon a client starts meanwhile waits for it and resumes no pane twice.
    instance.withdraw();
    terminals.close().await;
    server
        .close()
        .await
        .map_err(|err| anyhow::anyhow!("stopping the HTTP server: {err}"))
}
