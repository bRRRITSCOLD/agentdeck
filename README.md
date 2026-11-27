# agentdeck

An agent workspace GUI for Linux. Agents run as real CLIs (Claude Code, Codex, any command) in terminals grouped into Workspaces, each linking the GitHub Repositories it works on. agentdeck keeps your GitHub pull requests in one view, by Repository, and (in v1.1) adds channels between agents. It doesn't brief agents: they read their own `CLAUDE.md` or `AGENTS.md`, and you hand them work by pasting or with `agentdeck agent send`.

It is three programs in one Cargo workspace:

- `agentdeckd`, a local daemon that owns the terminals and the data;
- `agentdeck-gui`, a GPUI window that attaches to the daemon;
- `agentdeck`, a CLI that agents and scripts use; every Pane finds it on its `PATH`.

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
agentdeck pr ls          # your pull requests
```

Or install them: `for c in agentdeck agentdeckd agentdeck-gui; do cargo install --path crates/$c; done`.

- **After a pull:** a GUI newer than the running daemon shows an "older daemon" banner; press **Restart** (or `agentdeck daemon restart`). Running agents resume.
- **The domain reset:** the first daemon from after it finds a database with tasks in it, moves it aside to `agentdeck.db.pre-domain-reset` (with its `-wal` and `-shm` files) and starts empty: your Workspaces, Panes and Links are gone, and PRs come back with the next sync. Nothing upgrades the old file; it stays there, and so do scratch folders under `workspaces/`. The removed code is in the tag `pre-domain-reset`.
- **Workspaces have no folder:** a Workspace is a name, the Repositories it links and its Panes, so its agents work in any repo. A new Pane starts in the focused Pane's folder, or in `~` when the Workspace has none. Updating drops each Workspace's folder from the database and deletes nothing on disk: a scratch folder agentdeck made stays under `~/.local/share/agentdeck/workspaces/` until you remove it. `agentdeck ws new <name>` makes one, and `agentdeck repo link owner/name --ws <name>` links a Repository to it.
- **Stop everything:** closing the window leaves agents running in `agentdeckd` (spec §2). `pkill -TERM agentdeckd` stops it and every pane.
- **A daemon by hand:** with the GUI closed, `pkill -TERM agentdeckd`, then start one yourself, e.g. `GH_TOKEN=… agentdeckd &`, before `agentdeck-gui`. A second `agentdeckd` while one is healthy exits 1 with "another agentdeckd is running".

### Agents you start yourself

Every Pane's `PATH` ends with the folder holding `agentdeckd`, so any agent in a Pane, launched by agentdeck or started in a shell, can run `agentdeck` when it is installed beside `agentdeckd`. agentdeck starts an agent with no prompt and never types into it on its own, so nothing tells the agent so; paste this into your `CLAUDE.md` or `AGENTS.md`:

```markdown
## agentdeck

In an agentdeck Pane, `agentdeck` is on your PATH and `$AGENTDECK_WORKSPACE` names your Workspace.

- Its pull requests: `agentdeck pr ls --ws "$AGENTDECK_WORKSPACE" --json`
- The repositories it links: `agentdeck repo ls --ws "$AGENTDECK_WORKSPACE" --json`

Ask GitHub (`gh`) for a pull request's diff, checks and reviews.
```

To hand a running agent work, paste it into its Pane, or send it from a shell: `agentdeck agent send <pane> "<text>"` waits until the Pane is idle, then types the text and presses Enter (`--now` types it at once).

agentdeck no longer has an MCP server: if you added one with an older `agentdeck mcp install`, remove it with `claude mcp remove --scope user agentdeck` and by deleting `[mcp_servers.agentdeck]` from `~/.codex/config.toml`.

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

Its log is `$XDG_STATE_HOME/agentdeck/agentdeckd.log`. `AGENTDECK_GITHUB_API`, `AGENTDECK_GITHUB_REST_API` and `AGENTDECK_GITHUB_OAUTH` point PR sync, Repository lookups and sign-in at a fake GitHub, as the integration tier does.

### Desktop entry

For GNOME to group the window and its notifications under one dock icon, and show the app's icon there, install the desktop file and the icons once (its `Exec=agentdeck-gui` needs the binary on your session's `PATH`):

```sh
install -Dm644 packaging/agentdeck.desktop ~/.local/share/applications/agentdeck.desktop
mkdir -p ~/.local/share/icons && cp -r packaging/icons/hicolor ~/.local/share/icons/
```

| Crate | Kind | |
|---|---|---|
| `agentdeck` | bin | the CLI |
| `agentdeckd` | bin | the daemon |
| `agentdeck-gui` | bin | the GPUI app, on `brrritscold-core-gpui` |
| `agentdeck-client` | lib | the generated HTTP client |
| `agentdeck-core` | lib | GPUI-free domain types |
| `agentdeck-notify` | lib | the GUI's desktop notifications, over one D-Bus connection |
| `xtask` | bin | workspace tasks |

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
