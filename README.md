# agentdeck

An agent workspace GUI for Linux. Agents run as real CLIs (Claude Code, Codex, any command) in terminals grouped into free-form workspaces. agentdeck adds tasks that survive across sessions, tracking for your GitHub PRs, comment threads on tasks between you and their agents, standing instructions for every agent in a workspace, and (in v1.1) channels between agents.

It is three programs in one Cargo workspace:

- `agentdeckd`, a local daemon that owns the terminals and the data;
- `agentdeck-gui`, a GPUI window that attaches to the daemon;
- `agentdeck`, a CLI (and MCP server) that agents and scripts use.

Status: being specified. The decisions are in [the v1 map](https://github.com/bRRRITSCOLD/agentdeck/issues/1), and the spec is in [`docs/spec.md`](docs/spec.md) once it lands.

## Develop

Linux only. Install GPUI's system packages once with `script/install-linux-deps.sh` (Ubuntu 24.04), and [cargo-nextest](https://nexte.st) at the version `.config/nextest.toml` requires.

```sh
cargo ci                    # what CI runs: fmt, clippy, unit, integration and doc, warnings denied
cargo test-unit             # one tier; `cargo test-integration` for the other
cargo run -p agentdeck-gui  # the GUI
cargo xtask openapi         # rewrite openapi.json after changing the daemon's API
cargo xtask client          # then regenerate agentdeck-client from it
script/record-pty.py        # record an agent CLI's output as state-rule fixtures (spec §7)
```

| Crate | Kind | |
|---|---|---|
| `agentdeck` | bin | the CLI and `agentdeck mcp` |
| `agentdeckd` | bin | the daemon |
| `agentdeck-gui` | bin | the GPUI app, on `brrritscold-core-gpui` |
| `agentdeck-client` | lib | the generated HTTP client |
| `agentdeck-core` | lib | GPUI-free domain types |
| `agentdeck-notify` | lib | the GUI's desktop notifications, over one D-Bus connection |
| `xtask` | bin | workspace tasks |

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
