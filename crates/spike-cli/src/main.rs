//! PROTOTYPE (issue #2): `spike-cli state <state> --session <id>`.
//!
//! Called from a Claude Code hook. Reads the hook's JSON payload from stdin (when there is
//! one), and POSTs `{state, hook}` to the daemon found in
//! `$XDG_RUNTIME_DIR/agentdeck-spike/daemon.json`. No tokio, no GPUI: it has to start fast.
//! It never fails the hook: errors go to stderr and the exit code stays 0.

use std::io::{IsTerminal as _, Read as _};
use std::time::Instant;

use anyhow::{Context as _, bail};
use serde_json::{Value, json};

fn main() {
    let started = Instant::now();
    if let Err(e) = run() {
        eprintln!("spike-cli: {e:#}");
    }
    if std::env::var_os("SPIKE_CLI_TIMING").is_some() {
        eprintln!("spike-cli: {:?}", started.elapsed());
    }
}

fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (state, session) = match args.as_slice() {
        [cmd, state, flag, session] if cmd == "state" && flag == "--session" => (state, session),
        _ => bail!("usage: spike-cli state <working|blocked|done|idle> --session <id>"),
    };
    let mut hook = Value::Null;
    if !std::io::stdin().is_terminal() {
        let mut raw = String::new();
        std::io::stdin().read_to_string(&mut raw)?;
        if !raw.trim().is_empty() {
            hook = serde_json::from_str(&raw).unwrap_or(Value::String(raw));
        }
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR")?;
    let file = std::path::Path::new(&runtime).join("agentdeck-spike/daemon.json");
    let daemon: Value =
        serde_json::from_str(&std::fs::read_to_string(&file).context("daemon.json")?)?;
    let url = format!(
        "http://127.0.0.1:{}/sessions/{session}/state",
        daemon["port"]
    );
    ureq::post(&url)
        .header(
            "authorization",
            &format!("Bearer {}", daemon["token"].as_str().unwrap_or("")),
        )
        .send_json(json!({ "state": state, "hook": hook }))?;
    Ok(())
}
