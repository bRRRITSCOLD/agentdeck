//! `agentdeckd`: the daemon that owns the terminals and the data and serves the HTTP API.
//!
//! - `agentdeckd` runs the daemon (spec §2).
//! - `agentdeckd --version` prints its name and version.
//! - `agentdeckd openapi` prints the API's OpenAPI document; `cargo xtask openapi` writes it
//!   to `openapi.json`.

mod api;
mod auth;
mod daemon;
mod db;
mod git;
mod instance;
mod logging;
mod model;
mod paths;
mod prsync;
mod terminal;

use std::process::ExitCode;

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("--version") => {
            println!("{}", agentdeck_core::version_line("agentdeckd"));
            ExitCode::SUCCESS
        }
        Some("openapi") => {
            print!("{}", api::openapi_json());
            ExitCode::SUCCESS
        }
        None => match daemon::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                tracing::error!("{err:#}");
                eprintln!("agentdeckd: {err:#}");
                ExitCode::FAILURE
            }
        },
        Some(other) => {
            eprintln!(
                "agentdeckd: unknown argument `{other}`; usage: agentdeckd [--version | openapi]"
            );
            ExitCode::from(2)
        }
    }
}
