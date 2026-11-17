//! `agentdeck task ls | add | set | edit | show`.

use crate::support::{TestEnv, short, text};

mod task_add {
    use super::*;

    #[test]
    fn should_add_to_the_inbox_without_a_workspace() {
        let env = TestEnv::new();

        let task = env.json(&["task", "add", "Fix the build", "--body", "CI is red"]);

        assert_eq!(text(&task, "title"), "Fix the build");
        assert_eq!(text(&task, "body"), "CI is red");
        assert_eq!(text(&task, "status"), "todo");
        assert!(task.get("workspace_id").is_none(), "{task}");
    }

    #[test]
    fn should_add_to_a_workspace_named_by_its_name() {
        let env = TestEnv::new();
        let ws = env.workspace("web");

        let task = env.json(&["task", "add", "Ship it", "--ws", text(&ws, "name")]);

        assert_eq!(task["workspace_id"], ws["id"]);
    }
}

mod task_ls {
    use super::*;

    fn titles(tasks: &serde_json::Value) -> Vec<&str> {
        tasks
            .as_array()
            .unwrap()
            .iter()
            .map(|task| text(task, "title"))
            .collect()
    }

    #[test]
    fn should_filter_by_workspace_inbox_and_status() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        env.ok(&["task", "add", "in web", "--ws", "web"]);
        let doing = env.json(&["task", "add", "in inbox"]);
        env.ok(&["task", "set", text(&doing, "id"), "--status", "doing"]);

        assert_eq!(
            titles(&env.json(&["task", "ls", "--ws", text(&ws, "slug")])),
            ["in web"]
        );
        assert_eq!(titles(&env.json(&["task", "ls", "--inbox"])), ["in inbox"]);
        assert_eq!(
            titles(&env.json(&["task", "ls", "--status", "doing"])),
            ["in inbox"]
        );
        assert_eq!(titles(&env.json(&["task", "ls"])).len(), 2);
    }

    #[test]
    fn should_print_a_row_per_task() {
        let env = TestEnv::new();
        let task = env.json(&["task", "add", "Fix the build"]);

        let stdout = env.ok(&["task", "ls"]);

        assert_eq!(
            stdout,
            format!(
                "ID{}  STATUS  TITLE\n{}  todo    Fix the build\n",
                " ".repeat(34),
                text(&task, "id")
            )
        );
    }
}

mod task_set {
    use super::*;

    #[test]
    fn should_change_the_status() {
        let env = TestEnv::new();
        let task = env.json(&["task", "add", "Fix the build"]);

        let set = env.json(&["task", "set", text(&task, "id"), "--status", "done"]);

        assert_eq!(text(&set, "status"), "done");
        assert!(set["done_at"].is_string());
    }

    #[test]
    fn should_change_the_status_of_a_task_named_by_its_short_id() {
        let env = TestEnv::new();
        let task = env.json(&["task", "add", "Fix the build"]);

        let set = env.json(&["task", "set", short(text(&task, "id")), "--status", "doing"]);

        assert_eq!(set["id"], task["id"]);
        assert_eq!(text(&set, "status"), "doing");
    }
}

mod task_edit {
    use super::*;

    #[test]
    fn should_change_the_title_and_body() {
        let env = TestEnv::new();
        let task = env.json(&["task", "add", "Fix the build"]);

        let edited = env.json(&[
            "task",
            "edit",
            text(&task, "id"),
            "--title",
            "Fix the release build",
            "--body",
            "Only the release profile fails.",
        ]);

        assert_eq!(text(&edited, "title"), "Fix the release build");
        assert_eq!(text(&edited, "body"), "Only the release profile fails.");
    }

    #[test]
    fn should_move_a_task_to_a_workspace_and_back_to_the_inbox() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        let task = env.json(&["task", "add", "Fix the build"]);

        let moved = env.json(&["task", "edit", text(&task, "id"), "--ws", "web"]);
        let back = env.json(&["task", "edit", text(&task, "id"), "--inbox"]);

        assert_eq!(moved["workspace_id"], ws["id"]);
        assert!(back["workspace_id"].is_null(), "{back}");
        assert_eq!(
            env.json(&["task", "ls", "--inbox"])
                .as_array()
                .map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn should_print_the_task_as_show_does() {
        let env = TestEnv::new();
        let task = env.json(&["task", "add", "Fix the build"]);

        let stdout = env.ok(&["task", "edit", text(&task, "id"), "--title", "Ship it"]);

        assert!(stdout.starts_with("Ship it\n"), "{stdout}");
        assert!(stdout.contains("workspace: inbox"), "{stdout}");
    }

    #[test]
    fn should_fail_for_a_blank_title() {
        let env = TestEnv::new();
        let task = env.json(&["task", "add", "Fix the build"]);

        let stderr = env.fails(&["task", "edit", text(&task, "id"), "--title", " "]);

        assert!(stderr.contains("(invalid_title)"), "{stderr}");
    }

    #[test]
    fn should_fail_for_an_archived_workspace() {
        let env = TestEnv::new();
        env.workspace("old");
        env.ok(&["ws", "archive", "old"]);
        let task = env.json(&["task", "add", "Fix the build"]);

        let stderr = env.fails(&["task", "edit", text(&task, "id"), "--ws", "old"]);

        assert!(stderr.contains("(unknown_workspace)"), "{stderr}");
    }
}

mod task_show {
    use super::*;

    #[test]
    fn should_show_the_task_and_its_body() {
        let env = TestEnv::new();
        let task = env.json(&["task", "add", "Fix the build", "--body", "CI is red"]);

        let stdout = env.ok(&["task", "show", text(&task, "id")]);

        assert!(stdout.starts_with("Fix the build\n"), "{stdout}");
        assert!(stdout.contains("status:    todo"), "{stdout}");
        assert!(stdout.ends_with("\nCI is red\n"), "{stdout}");
    }

    #[test]
    fn should_fail_for_an_unknown_task() {
        let env = TestEnv::new();

        let stderr = env.fails(&["task", "show", "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b"]);

        assert!(stderr.contains("(not_found)"), "{stderr}");
    }

    mod by_short_id {
        use super::*;

        #[test]
        fn should_show_the_task_its_last_digits_name() {
            let env = TestEnv::new();
            env.ok(&["task", "add", "Other"]);
            let task = env.json(&["task", "add", "Fix the build"]);

            let shown = env.json(&["task", "show", short(text(&task, "id"))]);

            assert_eq!(shown["id"], task["id"]);
        }

        #[test]
        fn should_list_every_task_whose_id_holds_the_digits() {
            let env = TestEnv::new();
            let first = env.json(&["task", "add", "First"]);
            let second = env.json(&["task", "add", "Second"]);
            // Made milliseconds apart, so their leading (time) digits match.
            let leading = &text(&first, "id")[..6];

            let stderr = env.fails(&["task", "show", leading]);

            assert!(stderr.contains("(ambiguous_id)"), "{stderr}");
            assert!(
                stderr.contains(&format!("{}  First", text(&first, "id"))),
                "{stderr}"
            );
            assert!(
                stderr.contains(&format!("{}  Second", text(&second, "id"))),
                "{stderr}"
            );
        }

        #[test]
        fn should_fail_for_digits_no_task_holds() {
            let env = TestEnv::new();
            env.ok(&["task", "add", "Fix the build"]);

            // Every UUIDv7 has its version digit, 7, among any 30 of its 32 digits.
            let stderr = env.fails(&["task", "show", &"0".repeat(30)]);

            assert!(stderr.contains("(not_found)"), "{stderr}");
        }

        #[test]
        fn should_refuse_fewer_than_six_digits() {
            let env = TestEnv::new();
            let task = env.json(&["task", "add", "Fix the build"]);

            let stderr = env.fails(&["task", "show", &text(&task, "id")[32..]]);

            assert!(stderr.contains("(invalid_id)"), "{stderr}");
        }
    }
}
