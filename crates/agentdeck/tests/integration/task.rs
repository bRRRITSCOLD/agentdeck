//! `agentdeck task ls | add | set | show`.

use crate::support::{TestEnv, text};

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
}
