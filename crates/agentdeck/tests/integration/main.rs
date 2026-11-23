//! The integration tier: the real `agentdeck` binary in temp `$XDG_*` dirs, against the real
//! `agentdeckd` it starts (spec §10, §12). Every test starts from no daemon at all, so each
//! one also exercises the start path. Never runs a real agent CLI.

mod agent;
mod comment;
mod config;
mod daemon;
mod instruction;
mod pr;
mod support;
mod task;
mod version;
mod ws;
