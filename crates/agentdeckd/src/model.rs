//! The API's wire types (spec §3, §4).
//!
//! These are the daemon's own until `agentdeck-core` (#6) lands its ids, enums and layout
//! tree; then the shared ones move there and this module re-exports them.

use std::collections::HashSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `GET /v1/health`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Health {
    /// The daemon's version.
    pub version: String,
    /// Seconds since the daemon started.
    pub uptime_secs: u64,
}

/// A free-form unit of work with its own directory.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Workspace {
    /// The workspace's id.
    pub id: Uuid,
    /// Unique, derived from the name at creation; names a scratch dir.
    pub slug: String,
    /// The display name.
    pub name: String,
    /// The absolute path of the workspace's directory.
    pub dir: String,
    /// True when agentdeck created `dir` as a scratch dir, so deleting the workspace removes it.
    pub dir_owned: bool,
    /// Notifications are muted.
    pub muted: bool,
    /// When the workspace was archived; archived workspaces keep their data.
    pub archived_at: Option<String>,
    /// When the workspace was created.
    pub created_at: String,
}

/// `POST /v1/workspaces`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewWorkspace {
    /// The display name.
    pub name: String,
    /// An existing absolute directory to attach. Without it, agentdeck creates a scratch dir.
    pub dir: Option<String>,
}

/// `PATCH /v1/workspaces/{id}`: the fields to change.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct WorkspacePatch {
    /// A new display name. The slug and the dir stay as they are.
    pub name: Option<String>,
    /// Mute or unmute notifications.
    pub muted: Option<bool>,
}

/// The git repos under a workspace's dir that hold work not on any remote.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DirtyReport {
    /// Only the dirty repos; an empty list means deleting loses nothing git knows about.
    pub repos: Vec<DirtyRepo>,
}

/// One dirty git repo.
#[derive(Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DirtyRepo {
    /// The repo's absolute path.
    pub path: String,
    /// Changed, staged and untracked files, as `git status --porcelain` prints them.
    pub uncommitted: Vec<String>,
    /// Local branches with commits that are on no remote.
    pub unpushed_branches: Vec<String>,
}

/// A tab in a workspace, holding a split tree of panes.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Tab {
    /// The tab's id.
    pub id: Uuid,
    /// The workspace it belongs to.
    pub workspace_id: Uuid,
    /// The tab's title.
    pub title: String,
    /// Sort key among the workspace's tabs.
    pub position: i64,
    /// The split tree; `None` until the tab has a pane.
    pub layout: Option<Layout>,
}

/// `POST /v1/workspaces/{id}/tabs`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewTab {
    /// The tab's title.
    pub title: String,
    /// The initial split tree, if any.
    pub layout: Option<Layout>,
}

/// `PATCH /v1/tabs/{id}`: the fields to change.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct TabPatch {
    /// A new title.
    pub title: Option<String>,
    /// A new sort key.
    pub position: Option<i64>,
    /// A new split tree.
    pub layout: Option<Layout>,
}

/// A tab's split tree: `{"pane":id}` or `{"split":"h"|"v","ratio":f,"a":…,"b":…}`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum Layout {
    /// A leaf: one pane.
    #[schemars(title = "Pane")]
    Pane {
        /// The pane's id.
        pane: Uuid,
    },
    /// Two subtrees side by side (`h`) or stacked (`v`).
    #[schemars(title = "Split")]
    Split {
        /// The direction of the split.
        split: SplitAxis,
        /// The share of the space `a` takes, strictly between 0 and 1.
        ratio: f64,
        /// The first subtree: left or top.
        a: Box<Layout>,
        /// The second subtree: right or bottom.
        b: Box<Layout>,
    },
}

/// The direction of a split.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SplitAxis {
    /// Side by side.
    H,
    /// Stacked.
    V,
}

/// A task's place in todo → doing → review → done.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    /// Not started.
    Todo,
    /// In progress.
    Doing,
    /// Waiting on review.
    Review,
    /// Finished.
    Done,
}

/// Whether a task is plain or backed by a GitHub PR.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    /// Created by a person or an agent.
    Plain,
    /// Created by PR sync.
    Pr,
}

/// An item in the one global task list.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Task {
    /// The task's id.
    pub id: Uuid,
    /// The title.
    pub title: String,
    /// The body, possibly empty.
    pub body: String,
    /// The status.
    pub status: TaskStatus,
    /// Plain or PR.
    pub kind: TaskKind,
    /// The linked workspace; `None` puts the task in the inbox.
    pub workspace_id: Option<Uuid>,
    /// When the task was created.
    pub created_at: String,
    /// When the task last changed.
    pub updated_at: String,
    /// When the task last moved to done; `None` while it isn't done.
    pub done_at: Option<String>,
}

/// `POST /v1/tasks`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewTask {
    /// The title.
    pub title: String,
    /// The body; empty when absent.
    pub body: Option<String>,
    /// The status; `todo` when absent.
    pub status: Option<TaskStatus>,
    /// The workspace to link; the inbox when absent.
    pub workspace_id: Option<Uuid>,
}

/// `PATCH /v1/tasks/{id}`: the fields to change.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct TaskPatch {
    /// A new title.
    pub title: Option<String>,
    /// A new body.
    pub body: Option<String>,
    /// A new status.
    pub status: Option<TaskStatus>,
    /// A workspace to link.
    pub workspace_id: Option<Uuid>,
}

/// `GET /v1/tasks` filters; they combine with AND.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct TaskFilter {
    /// Only tasks with this status.
    pub status: Option<TaskStatus>,
    /// Only tasks linked to this workspace.
    pub workspace: Option<Uuid>,
    /// Only tasks of this kind.
    pub kind: Option<TaskKind>,
    /// `true`: only tasks with no workspace; `false`: only tasks with one.
    pub inbox: Option<bool>,
}

/// A message left on a workspace or a task for agents.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Note {
    /// The note's id.
    pub id: Uuid,
    /// The message.
    pub body: String,
    /// The workspace it's on; exactly one of this and `task_id` is set.
    pub workspace_id: Option<Uuid>,
    /// The task it's on; exactly one of this and `workspace_id` is set.
    pub task_id: Option<Uuid>,
    /// When the note was created.
    pub created_at: String,
    /// When the note was delivered or marked read.
    pub read_at: Option<String>,
    /// The pane it was delivered to, if a pane read it.
    pub read_by_pane: Option<Uuid>,
}

/// `POST /v1/notes`: exactly one of `workspace_id` and `task_id`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewNote {
    /// The message.
    pub body: String,
    /// The workspace to leave it on.
    pub workspace_id: Option<Uuid>,
    /// The task to leave it on.
    pub task_id: Option<Uuid>,
}

/// `PATCH /v1/notes/{id}`: the fields to change.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct NotePatch {
    /// A new message.
    pub body: Option<String>,
    /// `true` marks the note read now, `false` marks it unread.
    pub read: Option<bool>,
    /// The pane that read it; recorded only with `read: true`.
    pub read_by_pane: Option<Uuid>,
}

/// `GET /v1/notes` filters; they combine with AND.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct NoteFilter {
    /// Only notes on this workspace.
    pub workspace: Option<Uuid>,
    /// Only notes on this task.
    pub task: Option<Uuid>,
    /// `true`: only unread notes; `false`: only read ones.
    pub unread: Option<bool>,
}

impl Layout {
    /// Checks every ratio is strictly between 0 and 1 and no pane appears twice.
    pub fn validate(&self) -> Result<(), String> {
        fn walk(node: &Layout, seen: &mut HashSet<Uuid>) -> Result<(), String> {
            match node {
                Layout::Pane { pane } => {
                    if seen.insert(*pane) {
                        Ok(())
                    } else {
                        Err(format!("pane {pane} appears more than once"))
                    }
                }
                Layout::Split { ratio, a, b, .. } => {
                    if !(*ratio > 0.0 && *ratio < 1.0) {
                        return Err(format!("ratio {ratio} is not strictly between 0 and 1"));
                    }
                    walk(a, seen)?;
                    walk(b, seen)
                }
            }
        }
        walk(self, &mut HashSet::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(n: u128) -> Layout {
        Layout::Pane {
            pane: Uuid::from_u128(n),
        }
    }

    fn split(ratio: f64, a: Layout, b: Layout) -> Layout {
        Layout::Split {
            split: SplitAxis::H,
            ratio,
            a: Box::new(a),
            b: Box::new(b),
        }
    }

    mod layout {
        use super::*;

        mod serde {
            use super::*;

            #[test]
            fn should_read_the_spec_shape() {
                let json = r#"{"split":"v","ratio":0.25,
                    "a":{"pane":"00000000-0000-0000-0000-000000000001"},
                    "b":{"pane":"00000000-0000-0000-0000-000000000002"}}"#;

                let layout: Layout = serde_json::from_str(json).unwrap();

                assert_eq!(
                    layout,
                    Layout::Split {
                        split: SplitAxis::V,
                        ratio: 0.25,
                        a: Box::new(pane(1)),
                        b: Box::new(pane(2)),
                    }
                );
            }
        }

        mod validate {
            use super::*;

            #[test]
            fn should_accept_a_nested_tree() {
                let layout = split(0.5, pane(1), split(0.3, pane(2), pane(3)));
                assert_eq!(layout.validate(), Ok(()));
            }

            #[test]
            fn should_reject_a_ratio_of_zero() {
                assert!(split(0.0, pane(1), pane(2)).validate().is_err());
            }

            #[test]
            fn should_reject_a_ratio_of_one() {
                assert!(split(1.0, pane(1), pane(2)).validate().is_err());
            }

            #[test]
            fn should_reject_a_nan_ratio() {
                assert!(split(f64::NAN, pane(1), pane(2)).validate().is_err());
            }

            #[test]
            fn should_reject_a_bad_ratio_deep_in_the_tree() {
                let layout = split(0.5, pane(1), split(1.5, pane(2), pane(3)));
                assert!(layout.validate().is_err());
            }

            #[test]
            fn should_reject_a_pane_that_appears_twice() {
                let layout = split(0.5, pane(1), split(0.5, pane(2), pane(1)));
                assert!(layout.validate().is_err());
            }
        }
    }
}
