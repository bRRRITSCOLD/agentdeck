//! Entity ids (spec §3): one newtype per entity, so a pane id can't be passed where a task id
//! is expected. Each is a UUID written as its hyphenated string. Core never generates ids,
//! since that needs a clock and randomness; the daemon makes UUIDv7s and wraps them.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// The UUID inside.
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        #[cfg(feature = "schemars")]
        impl schemars::JsonSchema for $name {
            fn schema_name() -> String {
                stringify!($name).to_owned()
            }

            fn json_schema(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
                crate::schema::uuid_string()
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0.hyphenated(), f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }
    };
}

id!(
    /// A workspace's id (`workspaces.id`).
    WorkspaceId
);
id!(
    /// A tab's id (`tabs.id`).
    TabId
);
id!(
    /// A pane's id (`panes.id`), also the leaf of a tab's [`Layout`](crate::Layout).
    PaneId
);
id!(
    /// A task's id (`tasks.id`), also a pull request's key.
    TaskId
);
id!(
    /// A note's id (`notes.id`).
    NoteId
);

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "0196f1d2-7c3a-7b8e-9a01-23456789abcd";

    mod pane_id {
        use super::*;

        #[test]
        fn should_display_the_hyphenated_uuid_it_was_parsed_from() {
            let id: PaneId = TEXT.parse().expect("parse");
            assert_eq!(id.to_string(), TEXT);
        }

        #[test]
        fn should_reject_text_that_is_not_a_uuid() {
            assert!("pane-1".parse::<PaneId>().is_err());
        }

        #[test]
        fn should_serialize_as_a_plain_string() {
            let id: PaneId = TEXT.parse().expect("parse");
            let json = serde_json::to_string(&id).expect("serialize");
            assert_eq!(json, format!("\"{TEXT}\""));
            assert_eq!(
                serde_json::from_str::<PaneId>(&json).expect("deserialize"),
                id
            );
        }
    }
}
