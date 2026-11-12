# agentdeck

An agent workspace GUI for Linux. Agents run as real CLIs (Claude Code, Codex, any command) in terminals grouped into free-form workspaces. agentdeck adds tasks that survive across sessions, tracking for your GitHub PRs, notes you leave for agents, and (in v1.1) channels between agents.

It is three programs in one Cargo workspace:

- `agentdeckd`, a local daemon that owns the terminals and the data;
- `agentdeck-gui`, a GPUI window that attaches to the daemon;
- `agentdeck`, a CLI (and MCP server) that agents and scripts use.

Status: being specified. The decisions are in [the v1 map](https://github.com/bRRRITSCOLD/agentdeck/issues/1), and the spec is in [`docs/spec.md`](docs/spec.md) once it lands.
