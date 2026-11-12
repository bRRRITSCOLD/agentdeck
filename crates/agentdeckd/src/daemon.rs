//! Running the daemon: lock, database, PR sync, HTTP server, `daemon.json`, then serve until a
//! signal (spec §2).

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Instant;

use anyhow::Context;
use dropshot::{ConfigDropshot, ServerBuilder};
use tokio::signal::unix::{SignalKind, signal};

use crate::api::{ApiContext, api};
use crate::db::Db;
use crate::instance::{DaemonInfo, Instance};
use crate::paths::Paths;
use crate::prsync::{self, PrSync};
use crate::{auth, logging};

/// Starts the daemon and serves until SIGINT or SIGTERM.
pub fn run() -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    logging::init(&paths)?;
    let instance = Instance::acquire(&paths)?;
    let runtime = tokio::runtime::Runtime::new().context("starting tokio")?;
    runtime.block_on(serve(&paths, &instance))
}

async fn serve(paths: &Paths, instance: &Instance) -> anyhow::Result<()> {
    std::fs::create_dir_all(&paths.data)
        .with_context(|| format!("creating {}", paths.data.display()))?;
    let db = Db::open(&paths.database())?;
    let github_api = std::env::var(prsync::ENDPOINT_VAR)
        .unwrap_or_else(|_| prsync::github::DEFAULT_ENDPOINT.into());
    let prs = PrSync::start(db.clone(), github_api)?;
    let token = auth::new_token();
    let context = ApiContext {
        db,
        token: token.clone(),
        started: Instant::now(),
        scratch_root: paths.scratch_workspaces(),
        prs,
    };
    let config = ConfigDropshot {
        bind_address: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
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
    })?;
    tracing::info!("agentdeckd listening on 127.0.0.1:{port}");

    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
    tracing::info!("agentdeckd shutting down");
    server
        .close()
        .await
        .map_err(|err| anyhow::anyhow!("stopping the HTTP server: {err}"))
}
