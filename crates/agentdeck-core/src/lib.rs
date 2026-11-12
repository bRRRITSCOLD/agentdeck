//! GPUI-free domain types for agentdeck: ids, states and the state rules engine.
//! No IO, no GPUI, no tokio.
//!
//! - [`daemon`]: `daemon.json`, where clients find the daemon.
//! - [`id`]: one id newtype per entity.
//! - [`pane`], [`task`], [`pr`]: the text-valued columns of spec §3 as enums.
//! - [`layout`]: a tab's split tree.
//! - [`state`]: agent state from the screen, by data-driven rules (spec §7).
//! - [`ws`]: the `/v1/ws` WebSocket protocol (spec §5).
//!
//! The `schemars` feature adds `JsonSchema` to the ids, the column enums and the layout tree,
//! for the daemon's OpenAPI document.

#[cfg(feature = "schemars")]
mod schema;
mod text_enum;

pub mod daemon;
pub mod id;
pub mod layout;
pub mod pane;
pub mod pr;
pub mod state;
pub mod task;
pub mod ws;

pub use daemon::DaemonInfo;
pub use id::{NoteId, PaneId, TabId, TaskId, WorkspaceId};
pub use layout::{Layout, LayoutError, Ratio, SplitDirection};
pub use pane::{Agent, PaneKind};
pub use pr::{ChecksState, PrState, ReviewState, pr_task_status};
pub use state::AgentState;
pub use task::{TaskKind, TaskStatus};
pub use text_enum::UnknownValue;

/// What `<program> --version` prints: the program's name, then the workspace version, which
/// every agentdeck program shares.
pub fn version_line(program: &str) -> String {
    format!("{program} {}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    mod version_line {
        use super::*;

        #[test]
        fn should_name_the_program_then_the_workspace_version() {
            assert_eq!(
                version_line("agentdeckd"),
                concat!("agentdeckd ", env!("CARGO_PKG_VERSION"))
            );
        }
    }
}
