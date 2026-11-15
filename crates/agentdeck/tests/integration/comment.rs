//! `agentdeck comment ls | add`.

use std::ffi::OsString;

use agentdeck_core::Agent;

use crate::support::{TestEnv, hosting_pane, text};

mod comment_add {
    use super::*;

    #[test]
    fn should_add_the_users_comment_to_the_task() {
        let env = TestEnv::new();
        let task = env.json(&["task", "add", "Fix the build"]);

        let comment = env.json(&[
            "comment",
            "add",
            "look at the logs",
            "--task",
            text(&task, "id"),
        ]);

        assert_eq!(comment["task_id"], task["id"]);
        assert_eq!(comment["author"], "user");
        assert!(comment.get("author_pane").is_none(), "{comment}");
    }

    #[test]
    fn should_author_it_by_the_callers_pane() {
        let env = TestEnv::new();
        env.workspace("web");
        let task = env.json(&["task", "add", "Fix the build", "--ws", "web"]);
        let pane = env.json(&["agent", "new", "--ws", "web"]);
        let caller = OsString::from(text(&pane, "id"));

        let comment = env.json_with(
            &["comment", "add", "PR opened", "--task", text(&task, "id")],
            &[("AGENTDECK_PANE", &caller)],
        );

        assert_eq!(comment["author"], "agent");
        assert_eq!(comment["author_agent"], "claude");
        assert_eq!(comment["author_pane"], pane["id"]);
    }

    #[test]
    fn should_author_it_by_the_agent_the_callers_shell_pane_hosts() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        let task = env.json(&["task", "add", "Fix the build", "--ws", "web"]);
        let pane = hosting_pane(&env, &ws, Agent::Codex);
        let caller = OsString::from(&pane);

        let comment = env.json_with(
            &["comment", "add", "PR opened", "--task", text(&task, "id")],
            &[("AGENTDECK_PANE", &caller)],
        );

        assert_eq!(comment["author"], "agent");
        assert_eq!(comment["author_agent"], "codex");
        assert_eq!(comment["author_pane"], pane.as_str());
    }

    #[test]
    fn should_print_the_comments_id_and_task() {
        let env = TestEnv::new();
        let task = env.json(&["task", "add", "Fix the build"]);
        let task_id = text(&task, "id");

        let stdout = env.ok(&["comment", "add", "look at the logs", "--task", task_id]);

        assert!(stdout.ends_with(&format!("  task {task_id}\n")), "{stdout}");
    }
}

mod comment_ls {
    use super::*;

    #[test]
    fn should_list_the_tasks_comments_oldest_first() {
        let env = TestEnv::new();
        let task = env.json(&["task", "add", "Fix the build"]);
        let other = env.json(&["task", "add", "Something else"]);
        let task_id = text(&task, "id");
        env.ok(&["comment", "add", "first", "--task", task_id]);
        env.ok(&["comment", "add", "elsewhere", "--task", text(&other, "id")]);
        env.ok(&["comment", "add", "second", "--task", task_id]);

        let listed = env.json(&["comment", "ls", "--task", task_id]);

        let bodies: Vec<&str> = listed
            .as_array()
            .unwrap()
            .iter()
            .map(|c| text(c, "body"))
            .collect();
        assert_eq!(bodies, ["first", "second"]);
    }

    #[test]
    fn should_say_who_wrote_each_and_whether_it_reached_an_agent() {
        let env = TestEnv::new();
        env.workspace("web");
        let task = env.json(&["task", "add", "Fix the build", "--ws", "web"]);
        let task_id = text(&task, "id");
        env.ok(&["comment", "add", "look at the logs", "--task", task_id]);
        let pane = env.json(&["agent", "new", "--ws", "web"]);
        let caller = OsString::from(text(&pane, "id"));
        let run = env.run_with(
            &["comment", "add", "PR opened", "--task", task_id],
            &[("AGENTDECK_PANE", &caller)],
        );
        assert!(run.success, "{}", run.stderr);

        let stdout = env.ok(&["comment", "ls", "--task", task_id]);

        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines.len(), 3, "{stdout}");
        assert!(lines[0].starts_with("ID"), "{stdout}");
        assert!(lines[1].contains("you"), "{stdout}");
        assert!(lines[1].contains("waiting"), "{stdout}");
        assert!(lines[1].ends_with("look at the logs"), "{stdout}");
        assert!(lines[2].contains("claude"), "{stdout}");
        assert!(lines[2].ends_with("PR opened"), "{stdout}");
    }

    #[test]
    fn should_fail_for_an_unknown_task() {
        let env = TestEnv::new();

        let stderr = env.fails(&[
            "comment",
            "ls",
            "--task",
            "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
        ]);

        assert!(stderr.contains("not_found"), "{stderr}");
    }
}
