//! The integration tier: the real `agentdeck` binary in temp `$XDG_*` dirs, against the real
//! `agentdeckd` it starts (spec §10, §12). Every test starts from no daemon at all, so each
//! one also exercises the start path. Never runs a real agent CLI.

mod agent;
mod daemon;
mod mcp;
mod note;
mod pr;
mod support;
mod task;
mod version;
mod ws;
