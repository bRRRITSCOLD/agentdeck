//! `agentdeck note ls | add | read`.

use std::ffi::OsString;

use crate::support::{TestEnv, text};

mod note_add {
    use super::*;

    #[test]
    fn should_leave_a_note_on_a_workspace() {
        let env = TestEnv::new();
        let ws = env.workspace("web");

        let note = env.json(&["note", "add", "use the staging db", "--ws", "web"]);

        assert_eq!(note["workspace_id"], ws["id"]);
        assert!(note.get("read_at").is_none(), "{note}");
    }

    #[test]
    fn should_leave_a_note_on_a_task() {
        let env = TestEnv::new();
        let task = env.json(&["task", "add", "Fix the build"]);

        let note = env.json(&[
            "note",
            "add",
            "look at the logs",
            "--task",
            text(&task, "id"),
        ]);

        assert_eq!(note["task_id"], task["id"]);
    }
}

mod note_ls {
    use super::*;

    #[test]
    fn should_list_only_unread_notes_on_request() {
        let env = TestEnv::new();
        env.workspace("web");
        let read = env.json(&["note", "add", "old news", "--ws", "web"]);
        env.ok(&["note", "add", "fresh", "--ws", "web"]);
        env.ok(&["note", "read", text(&read, "id")]);

        let unread = env.json(&["note", "ls", "--ws", "web", "--unread"]);

        let bodies: Vec<&str> = unread
            .as_array()
            .unwrap()
            .iter()
            .map(|n| text(n, "body"))
            .collect();
        assert_eq!(bodies, ["fresh"]);
    }
}

mod note_read {
    use super::*;

    #[test]
    fn should_print_the_body_and_record_the_callers_pane() {
        let env = TestEnv::new();
        env.workspace("web");
        let note = env.json(&["note", "add", "use the staging db", "--ws", "web"]);
        let pane = OsString::from("0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b");

        let run = env.run_with(
            &["note", "read", text(&note, "id")],
            &[("AGENTDECK_PANE", &pane)],
        );

        assert!(run.success, "{}", run.stderr);
        assert_eq!(run.stdout, "use the staging db\n");
        let listed = env.json(&["note", "ls", "--ws", "web"]);
        assert!(listed[0]["read_at"].is_string(), "{listed}");
        assert_eq!(
            listed[0]["read_by_pane"],
            "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b"
        );
    }
}
