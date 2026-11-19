# agentdeck

An agent workspace GUI for Linux. Agents run as real CLIs (Claude Code, Codex, any command) in terminals grouped into free-form workspaces. agentdeck adds tasks that survive across sessions, tracking for your GitHub PRs, comment threads on tasks between you and their agents, standing instructions for every agent in a workspace, and (in v1.1) channels between agents.

It is three programs in one Cargo workspace:

- `agentdeckd`, a local daemon that owns the terminals and the data.
- `agentdeck-gui`, a GPUI window that attaches to the daemon.
- `agentdeck`, a CLI (and MCP server) that agents and scripts use.

The decisions are in [the v1 map](https://github.com/bRRRITSCOLD/agentdeck/issues/1), and the spec is [`docs/spec.md`](docs/spec.md).

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

### Run the dev build

`cargo build` puts all three programs in `target/debug`. Put that first on `PATH` in the shell you test from, so `agentdeck` and `agentdeckd` are found:

```sh
export PATH="$PWD/target/debug:$PATH"
agentdeck-gui &          # starts agentdeckd if none is running
agentdeck ws ls          # the CLI talks to the same daemon
agentdeck agent ls       # pane ids, for `agentdeck agent send <pane>`
agentdeck task ls --ws <ws>   # task ids
```

Or install them: `for c in agentdeck agentdeckd agentdeck-gui; do cargo install --path crates/$c; done`.

- **After a pull:** a GUI newer than the running daemon shows an "older daemon" banner; press **Restart** (or `agentdeck daemon restart`). Running agents resume.
- **Stop everything:** closing the window leaves agents running in `agentdeckd` (spec §2). `pkill -TERM agentdeckd` stops it and every pane.
- **A daemon by hand:** with the GUI closed, `pkill -TERM agentdeckd`, then start one yourself, e.g. `GH_TOKEN=… agentdeckd &`, before `agentdeck-gui`. A second `agentdeckd` while one is healthy exits 1 with "another agentdeckd is running".

### A scratch instance

Every file agentdeck keeps follows the XDG variables, so a scratch instance that never touches your real one is a temp dir away:

```sh
d=$(mktemp -d); mkdir -m 700 "$d/run"
export XDG_RUNTIME_DIR="$d/run" XDG_DATA_HOME="$d/data" XDG_STATE_HOME="$d/state" XDG_CONFIG_HOME="$d/config"
export AGENTDECK_KEYRING=memory   # an in-memory keyring, not your saved GitHub token
unset GH_TOKEN                    # no token from the environment
# `gh auth token` is still tried: put a `gh` that exits 1 first on PATH to keep it off your GitHub
agentdeck-gui
```

Its log is `$XDG_STATE_HOME/agentdeck/agentdeckd.log`. `AGENTDECK_GITHUB_API` and `AGENTDECK_GITHUB_OAUTH` point PR sync and sign-in at a fake GitHub, as the integration tier does.

### Desktop entry

For GNOME to group the window and its notifications under one dock icon, install the desktop file once (its `Exec=agentdeck-gui` needs the binary on your session's `PATH`):

```sh
install -Dm644 packaging/agentdeck.desktop ~/.local/share/applications/agentdeck.desktop
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
