//! `agentdeck`: the CLI, and `agentdeck mcp`, an MCP server over stdio (spec §10). No GPUI.
//!
//! Every command reads `daemon.json` and starts `agentdeckd` when nothing answers there,
//! warning on stderr when that daemon is older than the CLI; `agentdeck daemon restart`
//! replaces it. The HTTP client and the MCP server are async, so `main` drives them on a
//! current-thread tokio runtime.

mod cli;
mod commands;
mod launch;
mod mcp;
mod render;
mod resolve;

use std::io::Write;
use std::process::ExitCode;

use agentdeck_client::daemon;
use agentdeck_core::DaemonInfo;
use clap::Parser;
use serde_json::json;

use crate::cli::{Cli, Command, DaemonCommand};
use crate::commands::Output;

/// What the CLI prints on stderr, after `agentdeck: `, when the daemon predates it (spec §10).
const OLDER_DAEMON: &str = "warning: agentdeck's background service (agentdeckd) is older than \
    this CLI. Run agentdeck daemon restart to use new features; your agents resume after the \
    restart.";

/// The one-line warning for a daemon older than this CLI, if `info`'s is.
fn older_daemon_warning(info: &DaemonInfo) -> Option<String> {
    info.predates_client()
        .then(|| format!("agentdeck: {OLDER_DAEMON}"))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return fail(&anyhow::anyhow!("starting tokio: {err}")),
    };
    match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => fail(&err),
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    if let Command::Mcp = cli.command {
        return mcp::serve().await;
    }
    let daemon = daemon::connect().await?;
    let output = match cli.command {
        Command::Daemon(DaemonCommand::Restart) => restart(&daemon.info).await?,
        command => {
            if let Some(warning) = older_daemon_warning(&daemon.info) {
                eprintln!("{warning}");
            }
            commands::run(&daemon.client, command).await?
        }
    };
    let mut stdout = std::io::stdout().lock();
    if cli.json {
        serde_json::to_writer_pretty(&mut stdout, &output.json)?;
        stdout.write_all(b"\n")?;
    } else {
        stdout.write_all(&output.human)?;
    }
    stdout.flush()?;
    Ok(())
}

/// `agentdeck daemon restart`: stops the daemon `connected` names and starts a new one.
async fn restart(connected: &DaemonInfo) -> anyhow::Result<Output> {
    let new = daemon::restart(connected).await?.info;
    Ok(Output {
        json: json!({ "pid": new.pid, "version": new.version, "api_version": new.api_version }),
        human: format!("restarted agentdeckd, now pid {}\n", new.pid).into_bytes(),
    })
}

fn fail(err: &anyhow::Error) -> ExitCode {
    eprintln!("agentdeck: {err:#}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    mod older_daemon_warning {
        use super::*;

        fn info(api_version: u32) -> DaemonInfo {
            DaemonInfo {
                port: 1,
                token: String::new(),
                pid: 1,
                version: String::new(),
                api_version,
            }
        }

        #[test]
        fn should_say_how_to_restart_a_daemon_older_than_the_cli_in_one_line() {
            let warning = older_daemon_warning(&info(0)).expect("a warning");

            assert!(warning.starts_with("agentdeck: warning: "), "{warning}");
            assert!(warning.contains("agentdeck daemon restart"), "{warning}");
            assert!(!warning.contains('\n'), "{warning}");
        }

        #[test]
        fn should_be_none_for_a_daemon_as_new_as_the_cli() {
            assert_eq!(
                older_daemon_warning(&info(agentdeck_core::API_VERSION)),
                None
            );
        }
    }
}
