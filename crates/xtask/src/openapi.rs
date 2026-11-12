//! `cargo xtask openapi`: writes `openapi.json` from the daemon's dropshot API description.
//!
//! The document comes from `agentdeckd openapi`, so xtask doesn't build the daemon's
//! dependencies into itself. A unit test in `agentdeckd` fails while the file is stale.

use std::process::Command;

use anyhow::{Context, bail};

/// Regenerates `openapi.json` at the workspace root.
pub fn run() -> anyhow::Result<()> {
    let output = Command::new(crate::cargo())
        .args([
            "run",
            "--quiet",
            "--locked",
            "--package",
            "agentdeckd",
            "--",
            "openapi",
        ])
        .output()
        .context("running `agentdeckd openapi`")?;
    if !output.status.success() {
        bail!(
            "`agentdeckd openapi` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let path = crate::workspace_root().join("openapi.json");
    std::fs::write(&path, &output.stdout).with_context(|| format!("writing {}", path.display()))?;
    eprintln!("xtask openapi: wrote {}", path.display());
    Ok(())
}
