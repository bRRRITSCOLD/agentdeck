//! The integration tier: a real `agentdeckd` process on a random port in temp `$XDG_*` dirs,
//! driven through a real `agentdeck-client` (spec §12). Start one with
//! [`support::TestDaemon::start`].

mod agents;
mod auth;
mod comments;
mod daemon;
mod health;
mod instructions;
mod instructions_client;
mod migrations;
mod panes;
mod panes_client;
mod prs;
mod support;
mod tabs;
mod tasks;
mod version;
mod workspaces;
mod ws;
mod ws_client;
