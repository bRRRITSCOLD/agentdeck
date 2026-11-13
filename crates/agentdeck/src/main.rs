//! `agentdeck`: the CLI, and `agentdeck mcp`, an MCP server over stdio (spec §10). No GPUI.
//!
//! Every command reads `daemon.json` and starts `agentdeckd` when nothing answers there. The
//! HTTP client and the MCP server are async, so `main` drives them on a current-thread tokio
//! runtime.

mod cli;
mod commands;
mod launch;
mod mcp;
mod render;
mod resolve;

use std::io::Write;
use std::process::ExitCode;

use agentdeck_client::daemon;
use clap::Parser;

use crate::cli::{Cli, Command};

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
    let client = daemon::connect().await?.client;
    let output = commands::run(&client, cli.command).await?;
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

fn fail(err: &anyhow::Error) -> ExitCode {
    eprintln!("agentdeck: {err:#}");
    ExitCode::FAILURE
}
