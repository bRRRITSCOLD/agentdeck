//! GPUI-free domain types for agentdeck: ids, states and the state rules engine.
//! No IO, no GPUI, no tokio.
//!
//! - [`daemon`]: `daemon.json`, where clients find the daemon, and [`API_VERSION`].
//! - [`id`]: one id newtype per entity.
//! - [`pane`], [`task`], [`pr`], [`comment`]: the text-valued columns of spec §3 as enums.
//! - [`repository`] and [`PrRef`]: a Repository's GitHub id and name, and the references that
//!   name a Repository or a pull request (spec §10).
//! - [`layout`]: a tab's split tree.
//! - [`opener`]: the command a click on a file path in a pane runs.
//! - [`state`]: agent state from the screen, by data-driven rules (spec §7).
//! - [`ws`]: the `/v1/ws` WebSocket protocol (spec §5).
//!
//! The `schemars` feature adds `JsonSchema` to the ids, the column enums and the layout tree,
//! for the daemon's OpenAPI document.

#[cfg(feature = "schemars")]
mod schema;
mod text_enum;

pub mod comment;
pub mod daemon;
pub mod id;
pub mod layout;
pub mod opener;
pub mod pane;
pub mod pr;
pub mod repository;
pub mod state;
pub mod task;
pub mod ws;

pub use comment::CommentAuthor;
pub use daemon::{API_VERSION, DaemonInfo};
pub use id::{CommentId, InstructionId, PaneId, TabId, TaskId, WorkspaceId};
pub use layout::{Layout, LayoutError, Ratio, SplitDirection};
pub use pane::{Agent, PaneKind};
pub use pr::{
    ChecksState, NotAPr, PrRef, PrState, ReviewState, pr_details, pr_task_status, prs_details,
};
pub use repository::{Linking, NotARepository, RepositoryId, RepositoryName, pr_alerts_muted};
pub use state::AgentState;
pub use task::{TaskKind, TaskPlace, TaskStatus};
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
