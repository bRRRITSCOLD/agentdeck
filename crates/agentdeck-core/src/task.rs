//! Tasks (spec §3, `tasks.status`, `tasks.kind` and `tasks.workspace_id`).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::WorkspaceId;
use crate::text_enum::text_enum;

text_enum! {
    /// Where a task is in its life. "Blocked" is an agent state, never a task status.
    pub enum TaskStatus ("task status") {
        /// Not started.
        Todo => "todo",
        /// In progress.
        Doing => "doing",
        /// Waiting on review; new PR tasks start here.
        Review => "review",
        /// Finished; a merged or closed PR's task ends here.
        Done => "done",
    }
}

text_enum! {
    /// Whether a task is written by hand or backed by a GitHub pull request.
    pub enum TaskKind ("task kind") {
        /// A hand-written task.
        Plain => "plain",
        /// A task created and moved by PR sync.
        Pr => "pr",
    }
}

/// Where a task lives: in a workspace, or in the inbox, which is no workspace. In JSON, the
/// workspace's id, or `null` for the inbox, so `PATCH /v1/tasks/{id}` can tell moving a task
/// to the inbox (`"workspace_id": null`) from leaving it where it is (no `workspace_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskPlace {
    /// No workspace.
    Inbox,
    /// This workspace.
    Workspace(WorkspaceId),
}

impl TaskPlace {
    /// The workspace, or `None` for the inbox.
    pub fn workspace(self) -> Option<WorkspaceId> {
        match self {
            TaskPlace::Inbox => None,
            TaskPlace::Workspace(workspace) => Some(workspace),
        }
    }
}

impl From<Option<WorkspaceId>> for TaskPlace {
    fn from(workspace: Option<WorkspaceId>) -> Self {
        workspace.map_or(TaskPlace::Inbox, TaskPlace::Workspace)
    }
}

impl Serialize for TaskPlace {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.workspace().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TaskPlace {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<WorkspaceId>::deserialize(deserializer).map(TaskPlace::from)
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for TaskPlace {
    fn schema_name() -> String {
        "TaskPlace".to_owned()
    }

    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        let mut schema = generator
            .subschema_for::<Option<WorkspaceId>>()
            .into_object();
        schema.metadata().description = Some("A workspace's id, or null for the inbox.".to_owned());
        schema.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod task_status {
        use super::*;

        #[test]
        fn should_use_the_spec_column_values() {
            let texts: Vec<_> = TaskStatus::ALL.iter().map(|s| s.as_str()).collect();
            assert_eq!(texts, ["todo", "doing", "review", "done"]);
        }

        #[test]
        fn should_parse_its_own_text_form() {
            for status in TaskStatus::ALL {
                assert_eq!(status.as_str().parse::<TaskStatus>(), Ok(*status));
            }
        }

        #[test]
        fn should_reject_a_word_that_is_not_a_status() {
            let err = "blocked".parse::<TaskStatus>().unwrap_err();
            assert_eq!(err.to_string(), "unknown task status `blocked`");
        }

        #[test]
        fn should_serialize_as_its_text_form() {
            let json = serde_json::to_string(&TaskStatus::Review).expect("serialize");
            assert_eq!(json, r#""review""#);
            let back: TaskStatus = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, TaskStatus::Review);
        }

        #[test]
        fn should_fail_to_deserialize_an_unknown_word() {
            let err = serde_json::from_str::<TaskStatus>(r#""later""#).unwrap_err();
            assert!(
                err.to_string().contains("unknown task status `later`"),
                "{err}"
            );
        }
    }

    mod task_place {
        use super::*;

        const ID: &str = "0190f5b2-6c4e-7d6a-9b1e-2f3a4b5c6d7e";

        #[test]
        fn should_serialize_the_inbox_as_null_and_a_workspace_as_its_id() {
            let workspace: WorkspaceId = ID.parse().expect("an id");

            assert_eq!(serde_json::to_string(&TaskPlace::Inbox).unwrap(), "null");
            assert_eq!(
                serde_json::to_string(&TaskPlace::Workspace(workspace)).unwrap(),
                format!("\"{ID}\"")
            );
        }

        #[test]
        fn should_read_null_as_the_inbox() {
            let place: TaskPlace = serde_json::from_str("null").expect("deserialize");

            assert_eq!(place, TaskPlace::Inbox);
            assert_eq!(place.workspace(), None);
        }

        #[test]
        fn should_read_an_id_as_that_workspace() {
            let place: TaskPlace = serde_json::from_str(&format!("\"{ID}\"")).expect("deserialize");

            assert_eq!(place.workspace(), Some(ID.parse().unwrap()));
        }
    }

    mod task_kind {
        use super::*;

        #[test]
        fn should_use_the_spec_column_values() {
            let texts: Vec<_> = TaskKind::ALL.iter().map(|k| k.as_str()).collect();
            assert_eq!(texts, ["plain", "pr"]);
        }
    }
}
