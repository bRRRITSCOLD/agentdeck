//! Tasks (spec §3, `tasks.status` and `tasks.kind`).

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

    mod task_kind {
        use super::*;

        #[test]
        fn should_use_the_spec_column_values() {
            let texts: Vec<_> = TaskKind::ALL.iter().map(|k| k.as_str()).collect();
            assert_eq!(texts, ["plain", "pr"]);
        }
    }
}
