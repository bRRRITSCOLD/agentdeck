//! The integration tier: a real `agentdeckd` process on a random port in temp `$XDG_*` dirs,
//! driven through a real `agentdeck-client` (spec §12). Start one with
//! [`support::TestDaemon::start`].

mod auth;
mod daemon;
mod health;
mod notes;
mod prs;
mod support;
mod tabs;
mod tasks;
mod version;
mod workspaces;
