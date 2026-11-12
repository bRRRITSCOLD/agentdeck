//! Workspace tasks for agentdeck, run through the cargo aliases in `.cargo/config.toml`.
//!
//! - `cargo xtask ci` runs the chain CI runs: fmt, clippy, unit, integration and doc, with
//!   warnings denied. Alias: `cargo ci`.
//! - `cargo xtask test <tier> [--ci] [-- <nextest args>]` runs one test tier.
//! - `cargo xtask openapi` writes `openapi.json` from the daemon's API.
//! - `cargo xtask client` regenerates `agentdeck-client` from `openapi.json`.

mod ci;
mod client;
mod openapi;
mod test_chain;

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use anyhow::{Context, bail};

use test_chain::Tier;

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("xtask: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> anyhow::Result<ExitCode> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        Some("ci") => Ok(ci::run()),
        Some("test") => {
            let tier: Tier = args
                .next()
                .context("usage: cargo xtask test <tier> [--ci]")?
                .parse()?;
            let rest: Vec<String> = args.collect();
            let ci = rest.iter().any(|a| a == "--ci");
            let passthrough: Vec<String> = rest
                .into_iter()
                .filter(|a| a != "--ci" && a != "--")
                .collect();
            Ok(test_chain::run_tier(tier, ci, &passthrough))
        }
        Some("openapi") => openapi::run().map(|()| ExitCode::SUCCESS),
        Some("client") => client::run().map(|()| ExitCode::SUCCESS),
        Some(other) => bail!("unknown task `{other}`"),
        None => bail!("usage: cargo xtask <task>; tasks: ci, test, openapi, client"),
    }
}

/// The cargo that runs xtask, so nested runs use the pinned toolchain.
fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

/// The workspace root: two levels above this crate's manifest.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root exists")
}

/// Runs `command` and returns its exit code; 1 when it could not start or was killed.
fn status(mut command: Command) -> i32 {
    match command.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(err) => {
            eprintln!("xtask: could not run {command:?}: {err}");
            1
        }
    }
}
