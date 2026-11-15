//! The integration tier: a real `agentdeckd` process on a random port in temp `$XDG_*` dirs,
//! driven through a real `agentdeck-client` (spec §12). Start one with
//! [`support::TestDaemon::start`].

mod agents;
mod auth;
mod comments;
mod daemon;
mod github_auth;
mod health;
mod instructions;
mod instructions_client;
mod keyring;
mod migrations;
mod older_daemon;
mod panes;
mod panes_client;
mod prs;
mod settings;
mod settings_client;
mod support;
mod tabs;
mod tasks;
mod version;
mod workspaces;
mod ws;
mod ws_client;
