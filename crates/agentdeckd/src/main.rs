//! `agentdeckd`: the daemon that owns the terminals and the data and serves the HTTP API.
//!
//! A stub until the spec's later tickets fill it: only `--version` works.

use std::process::ExitCode;

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("--version") => {
            println!("{}", agentdeck_core::version_line("agentdeckd"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                "agentdeckd: not implemented yet; see https://github.com/bRRRITSCOLD/agentdeck/issues/1"
            );
            ExitCode::FAILURE
        }
    }
}
