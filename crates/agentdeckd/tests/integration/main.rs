//! The integration tier: a real `agentdeckd` process on a random port in temp `$XDG_*` dirs,
//! driven through a real `agentdeck-client` (spec §12). Start one with
//! [`support::TestDaemon::start`].

mod agents;
mod auth;
mod daemon;
mod folders;
mod github_auth;
mod health;
mod hosted;
mod keyring;
mod migrations;
mod modes;
mod older_daemon;
mod panes;
mod panes_client;
mod prs;
mod prs_client;
mod repositories;
mod repositories_client;
mod settings;
mod settings_client;
mod support;
mod tabs;
mod version;
mod workspaces;
mod workspaces_client;
mod ws;
mod ws_client;
