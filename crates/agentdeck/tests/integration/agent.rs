//! `agentdeck agent ls | new | send | link | stop` and `agentdeck tail`, against `sh` panes and the
//! stub `claude` and `codex`; never a real agent.

use agentdeck_core::Layout;

use crate::support::{
    TestEnv, agent_argv, agent_typed, eventually, shell_pane, shell_pane_in, short, tabs, text,
};

mod agent_ls {
    use serde_json::Value;

    use super::*;

    /// The listed pane whose id is `id`.
    fn listed<'a>(list: &'a Value, id: &str) -> &'a Value {
        (list.as_array().expect("a list").iter())
            .find(|pane| pane["id"] == id)
            .unwrap_or_else(|| panic!("{id} isn't listed: {list}"))
    }

    #[test]
    fn should_print_each_panes_id_workspace_tab_agent_state_and_task() {
        let env = TestEnv::new();
        env.workspace("web");
        let task = env.json(&["task", "add", "Fix the flaky test", "--ws", "web"]);
        let agent = env.json(&["agent", "new", "--ws", "web", "--task", text(&task, "id")]);
        let api = env.workspace("api");
        let shell = shell_pane_in(&env, &api, "build", "sleep 60");

        let stdout = env.ok(&["agent", "ls"]);

        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines.len(), 3, "{stdout}");
        let header: Vec<&str> = lines[0].split_whitespace().collect();
        assert_eq!(
            header,
            ["ID", "WORKSPACE", "TAB", "AGENT", "STATE", "TASK"],
            "{stdout}"
        );
        let agent_row = (lines.iter())
            .find(|line| line.starts_with(text(&agent, "id")))
            .unwrap_or_else(|| panic!("no row for the agent: {stdout}"));
        let cells: Vec<&str> = agent_row.split_whitespace().collect();
        assert_eq!(cells[1..4], ["web", "claude", "claude"], "{stdout}");
        assert!(agent_row.ends_with("  Fix the flaky test"), "{stdout}");
        let shell_row = (lines.iter())
            .find(|line| line.starts_with(&shell))
            .unwrap_or_else(|| panic!("no row for the shell: {stdout}"));
        let cells: Vec<&str> = shell_row.split_whitespace().collect();
        assert_eq!(cells[1..4], ["api", "build", "shell"], "{stdout}");
    }

    #[test]
    fn should_print_the_panes_with_where_they_are_as_json() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        let task = env.json(&["task", "add", "Fix the flaky test", "--ws", "web"]);
        let agent = env.json(&["agent", "new", "--ws", "web", "--task", text(&task, "id")]);

        let list = env.json(&["agent", "ls"]);

        let pane = listed(&list, text(&agent, "id"));
        assert_eq!(pane["kind"], "agent");
        assert_eq!(pane["agent"], "claude");
        assert_eq!(pane["tab_id"], agent["tab_id"]);
        assert_eq!(pane["task_id"], task["id"]);
        assert_eq!(pane["workspace_id"], ws["id"]);
        assert_eq!(pane["workspace"], "web");
        assert_eq!(pane["tab"], "claude");
        assert_eq!(pane["task_title"], "Fix the flaky test");
    }

    #[test]
    fn should_list_only_the_workspace_ws_names() {
        let env = TestEnv::new();
        let web = env.workspace("web");
        let api = env.workspace("api");
        let in_web = shell_pane(&env, &web, "sleep 60");
        let in_api = shell_pane(&env, &api, "sleep 60");

        let list = env.json(&["agent", "ls", "--ws", "api"]);

        let ids: Vec<&Value> = (list.as_array().unwrap().iter())
            .map(|pane| &pane["id"])
            .collect();
        assert_eq!(ids, [&Value::from(in_api)], "not {in_web}");
    }

    #[test]
    fn should_list_a_pane_whose_process_exited_as_exited() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        let pane = shell_pane(&env, &ws, "exit 3");

        let state = eventually("the pane to exit", || {
            let list = env.json(&["agent", "ls"]);
            let state = listed(&list, &pane)["state"].clone();
            (state == "exited").then_some(state)
        });

        assert_eq!(state, "exited");
        let stdout = env.ok(&["agent", "ls"]);
        assert!(stdout.contains("  shell  exited"), "{stdout}");
    }

    #[test]
    fn should_say_so_when_there_are_no_panes() {
        let env = TestEnv::new();
        env.workspace("web");

        assert_eq!(env.ok(&["agent", "ls"]), "no panes\n");
    }

    #[test]
    fn should_fail_for_an_unknown_workspace() {
        let env = TestEnv::new();

        let stderr = env.fails(&["agent", "ls", "--ws", "nope"]);

        assert!(stderr.contains("unknown_workspace"), "{stderr}");
    }
}

mod agent_send {
    use super::*;

    #[test]
    fn should_type_the_text_and_press_enter() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        let pane = shell_pane(&env, &ws, "read line; echo \"got:$line\"; sleep 60");

        let sent = env.json(&["agent", "send", &pane, "hello", "--now"]);

        assert_eq!(sent["queued"], false);
        eventually("the pane to echo the line", || {
            env.ok(&["tail", &pane]).contains("got:hello").then_some(())
        });
    }

    #[test]
    fn should_send_to_a_pane_named_by_its_short_id() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        shell_pane_in(&env, &ws, "other", "sleep 60");
        let pane = shell_pane(&env, &ws, "read line; echo \"got:$line\"; sleep 60");

        let stdout = env.ok(&["agent", "send", short(&pane), "hello", "--now"]);

        assert_eq!(stdout, format!("sent to {pane}\n"));
        eventually("the pane to echo the line", || {
            env.ok(&["tail", &pane]).contains("got:hello").then_some(())
        });
    }

    #[test]
    fn should_list_every_pane_whose_id_holds_the_digits() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        let first = shell_pane_in(&env, &ws, "one", "sleep 60");
        let second = shell_pane_in(&env, &ws, "two", "sleep 60");

        let stderr = env.fails(&["agent", "send", &first[..6], "hello"]);

        assert!(stderr.contains("(ambiguous_id)"), "{stderr}");
        assert!(stderr.contains(&format!("{first}  web · one")), "{stderr}");
        assert!(stderr.contains(&format!("{second}  web · two")), "{stderr}");
    }
}

mod agent_link {
    use super::*;

    #[test]
    fn should_link_the_pane_to_the_task_then_unlink_it() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        let pane = shell_pane(&env, &ws, "sleep 60");
        let task = env.json(&["task", "add", "Fix it", "--ws", "web"]);

        let linked = env.json(&["agent", "link", &pane, "--task", text(&task, "id")]);
        assert_eq!(linked["task_id"], task["id"]);
        assert_eq!(
            env.ok(&["agent", "link", &pane, "--task", text(&task, "id")]),
            format!("linked {pane} to {}\n", text(&task, "id"))
        );

        assert_eq!(
            env.ok(&["agent", "link", &pane, "--none"]),
            format!("unlinked {pane}\n")
        );
        let unlinked = env.json(&["agent", "link", &pane, "--none"]);
        assert!(
            unlinked.get("task_id").is_none_or(|id| id.is_null()),
            "{unlinked}"
        );
    }

    #[test]
    fn should_link_a_pane_and_task_named_by_their_short_ids() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        let pane = shell_pane(&env, &ws, "sleep 60");
        let task = env.json(&["task", "add", "Fix it", "--ws", "web"]);

        let stdout = env.ok(&[
            "agent",
            "link",
            short(&pane),
            "--task",
            short(text(&task, "id")),
        ]);

        assert_eq!(stdout, format!("linked {pane} to {}\n", text(&task, "id")));
    }

    #[test]
    fn should_fail_for_a_done_task() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        let pane = shell_pane(&env, &ws, "sleep 60");
        let task = env.json(&["task", "add", "Fix it", "--ws", "web"]);
        env.ok(&["task", "set", text(&task, "id"), "--status", "done"]);

        let stderr = env.fails(&["agent", "link", &pane, "--task", text(&task, "id")]);

        assert!(stderr.contains("(invalid_task)"), "{stderr}");
    }
}

mod agent_stop {
    use super::*;

    /// The pane's state, as `agent ls` lists it.
    fn state(env: &TestEnv, pane: &str) -> String {
        let list = env.json(&["agent", "ls"]);
        let listed = (list.as_array().expect("a list").iter()).find(|p| p["id"] == pane);
        text(listed.expect("the pane is listed"), "state").to_owned()
    }

    #[test]
    fn should_stop_the_agent_and_keep_its_pane_exited() {
        let env = TestEnv::new();
        env.workspace("web");
        let pane = env.json(&["agent", "new", "--ws", "web"]);
        let id = text(&pane, "id");

        let stdout = env.ok(&["agent", "stop", short(id)]);

        assert_eq!(stdout, format!("stopped {id}\n"));
        eventually("the agent to stop", || {
            (state(&env, id) == "exited").then_some(())
        });
    }

    #[test]
    fn should_print_what_it_stopped_as_json() {
        let env = TestEnv::new();
        env.workspace("web");
        let pane = env.json(&["agent", "new", "--ws", "web"]);
        let id = text(&pane, "id");

        let stopped = env.json(&["agent", "stop", id]);

        assert_eq!(stopped, serde_json::json!({ "id": id, "stopped": true }));
    }

    #[test]
    fn should_fail_for_a_pane_whose_process_exited() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        let pane = shell_pane(&env, &ws, "exit 0");
        eventually("the pane to exit", || {
            (state(&env, &pane) == "exited").then_some(())
        });

        let stderr = env.fails(&["agent", "stop", &pane]);

        assert!(stderr.contains("(exited)"), "{stderr}");
    }
}

mod tail {
    use super::*;

    #[test]
    fn should_print_only_the_last_bytes_asked_for() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        let pane = shell_pane(&env, &ws, "printf 'abcdef-done'; sleep 60");
        eventually("the pane's output", || {
            env.ok(&["tail", &pane])
                .contains("abcdef-done")
                .then_some(())
        });

        let stdout = env.ok(&["tail", &pane, "--bytes", "4"]);

        assert_eq!(stdout, "done");
    }

    #[test]
    fn should_print_a_pane_named_by_its_short_id() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        let pane = shell_pane(&env, &ws, "printf 'short-done'; sleep 60");

        eventually("the pane's output", || {
            env.ok(&["tail", short(&pane)])
                .contains("short-done")
                .then_some(())
        });
    }

    #[test]
    fn should_fail_for_digits_no_pane_holds() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        shell_pane(&env, &ws, "sleep 60");

        // Every UUIDv7 has its version digit, 7, among any 30 of its 32 digits.
        let stderr = env.fails(&["tail", &"0".repeat(30)]);

        assert!(stderr.contains("(not_found)"), "{stderr}");
    }

    #[test]
    fn should_fail_for_an_unknown_pane() {
        let env = TestEnv::new();

        let stderr = env.fails(&["tail", "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b"]);

        assert!(stderr.contains("not_found"), "{stderr}");
    }
}

mod agent_new {
    use super::*;

    /// The stub agent's argv, once it has started.
    fn started(env: &TestEnv, pane: &str) -> Vec<String> {
        eventually("the stub agent to start", || agent_argv(env, pane))
    }

    #[test]
    fn should_launch_claude_in_a_new_tab_in_the_workspace_dir() {
        let env = TestEnv::new();
        let ws = env.workspace("web");

        let pane = env.json(&["agent", "new", "--ws", "web"]);

        assert_eq!(pane["kind"], "agent");
        assert_eq!(pane["agent"], "claude");
        assert_eq!(pane["cwd"], ws["dir"]);
        let session = text(&pane, "session_id");
        assert_eq!(pane["argv"][0], "claude");
        let argv = started(&env, text(&pane, "id"));
        assert_eq!(argv, ["--session-id", session]);
        let tabs = tabs(&env, &ws);
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].id.to_string(), text(&pane, "tab_id"));
        assert_eq!(
            tabs[0].layout,
            Some(Layout::single(text(&pane, "id").parse().unwrap()))
        );
    }

    #[test]
    fn should_fill_an_empty_tab_before_making_another() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        let first = env.json(&["agent", "new", "--ws", "web"]);

        let second = env.json(&["agent", "new", "--ws", "web", "--agent", "codex"]);

        assert_ne!(first["tab_id"], second["tab_id"]);
        assert_eq!(tabs(&env, &ws).len(), 2);
    }

    #[test]
    fn should_launch_codex_without_a_session_id_at_first() {
        let env = TestEnv::new();
        env.workspace("web");

        let pane = env.json(&["agent", "new", "--ws", "web", "--agent", "codex"]);

        assert_eq!(pane["agent"], "codex");
        assert_eq!(pane["argv"][0], "codex");
        assert!(pane.get("session_id").is_none(), "{pane}");
        let argv = started(&env, text(&pane, "id"));
        assert_eq!(argv, Vec::<String>::new());
    }

    #[test]
    fn should_prompt_with_the_task_instructions_and_comments_and_mark_them_delivered() {
        let env = TestEnv::new();
        env.workspace("web");
        let task = env.json(&["task", "add", "Fix the flaky test", "--ws", "web"]);
        let task_id = text(&task, "id");
        env.ok(&[
            "instruction",
            "add",
            "Push only after 20 green runs",
            "--ws",
            "web",
        ]);
        env.ok(&["comment", "add", "it fails on CI only", "--task", task_id]);

        let pane = env.json(&["agent", "new", "--ws", "web", "--task", task_id]);

        assert_eq!(pane["task_id"], task["id"]);
        let prompt = started(&env, text(&pane, "id")).join(" ");
        assert!(prompt.contains("Fix the flaky test"), "{prompt}");
        assert!(prompt.contains("Push only after 20 green runs"), "{prompt}");
        assert!(prompt.contains("it fails on CI only"), "{prompt}");
        let comments = env.json(&["comment", "ls", "--task", task_id]);
        assert_eq!(comments[0]["delivered_to_pane"], pane["id"]);
    }

    #[test]
    fn should_link_a_task_named_by_its_short_id() {
        let env = TestEnv::new();
        env.workspace("web");
        let task = env.json(&["task", "add", "Fix it", "--ws", "web"]);

        let pane = env.json(&[
            "agent",
            "new",
            "--ws",
            "web",
            "--task",
            short(text(&task, "id")),
        ]);

        assert_eq!(pane["task_id"], task["id"]);
    }

    #[test]
    fn should_run_in_a_cwd_given_relative_to_the_shell() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        let sub = std::path::Path::new(text(&ws, "dir")).join("app");
        std::fs::create_dir(&sub).unwrap();

        let output = env
            .command()
            .current_dir(text(&ws, "dir"))
            .args(["agent", "new", "--ws", "web", "--cwd", "app", "--json"])
            .output()
            .expect("running agentdeck");

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let pane: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(std::path::Path::new(text(&pane, "cwd")), sub);
    }

    #[test]
    fn should_refuse_a_missing_cwd_and_leave_no_empty_tab() {
        let env = TestEnv::new();
        let ws = env.workspace("web");

        let stderr = env.fails(&["agent", "new", "--ws", "web", "--cwd", "/no/such/dir"]);

        assert!(stderr.contains("invalid_dir"), "{stderr}");
        assert!(tabs(&env, &ws).is_empty());
    }

    #[test]
    fn should_name_an_unknown_task() {
        let env = TestEnv::new();
        env.workspace("web");
        let task = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";

        let stderr = env.fails(&["agent", "new", "--ws", "web", "--task", task]);

        assert!(stderr.contains(&format!("no task {task}")), "{stderr}");
    }

    #[test]
    fn should_print_the_pane_agent_and_session() {
        let env = TestEnv::new();
        env.workspace("web");

        let stdout = env.ok(&["agent", "new", "--ws", "web"]);

        assert!(stdout.contains("  agent:   claude\n"), "{stdout}");
        assert!(stdout.contains("  session: "), "{stdout}");
    }
}

mod agent_send_to_an_agent {
    use super::*;

    #[test]
    fn should_type_the_message_into_the_agent_once_it_is_idle() {
        let env = TestEnv::new();
        env.workspace("web");
        let pane = env.json(&["agent", "new", "--ws", "web"]);
        let id = text(&pane, "id");

        env.ok(&["agent", "send", id, "hello agent"]);

        let submitted = eventually("the agent to submit the message", || {
            Some(agent_typed(&env, id)).filter(|typed| !typed.is_empty())
        });
        assert_eq!(submitted, "hello agent\n");
    }
}
