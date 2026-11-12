//! `agentdeck`: the CLI, and `agentdeck mcp`, an MCP server over stdio. No GPUI, no tokio.
//!
//! A stub until the spec's later tickets fill it: only `--version` works.

use std::process::ExitCode;

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("--version") => {
            println!("{}", agentdeck_core::version_line("agentdeck"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                "agentdeck: not implemented yet; see https://github.com/bRRRITSCOLD/agentdeck/issues/1"
            );
            ExitCode::FAILURE
        }
    }
}
