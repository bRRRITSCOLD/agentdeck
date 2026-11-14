//! `agentdeck mcp`: initialize, tools/list and tool calls over stdio, against a real daemon.

use serde_json::json;

use crate::support::mcp::McpClient;
use crate::support::{TestEnv, text};

mod agentdeck_mcp {
    use super::*;

    #[test]
    fn should_initialize_and_list_the_spec_tools() {
        let env = TestEnv::new();
        let mut mcp = McpClient::start(&env, &[]);

        let init = mcp.initialize();
        let tools = mcp.request("tools/list", json!({}));

        assert_eq!(init["serverInfo"]["name"], "agentdeck");
        assert!(init["capabilities"]["tools"].is_object(), "{init}");
        let mut names: Vec<&str> = tools["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("no tools: {tools}"))
            .iter()
            .map(|tool| text(tool, "name"))
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "instructions_list",
                "prs_status",
                "task_comment",
                "task_comments",
                "task_create",
                "task_get",
                "task_update",
                "tasks_list",
                "workspaces_list",
            ]
        );
    }

    #[test]
    fn should_start_the_daemon_for_a_tool_call() {
        let env = TestEnv::new();
        let mut mcp = McpClient::start(&env, &[]);
        mcp.initialize();

        let status = mcp.call_ok("prs_status", json!({}));

        assert_eq!(status["login"], "logged_out");
        assert!(env.daemon_info().is_some());
    }

    mod in_a_workspace {
        use super::*;

        #[test]
        fn should_create_and_list_tasks_in_the_callers_workspace() {
            let env = TestEnv::new();
            let ws = env.workspace("web");
            env.ok(&["task", "add", "in the inbox"]);
            let mut mcp = McpClient::start(&env, &[("AGENTDECK_WORKSPACE", text(&ws, "id"))]);
            mcp.initialize();

            let created = mcp.call_ok("task_create", json!({ "title": "from an agent" }));
            let listed = mcp.call_ok("tasks_list", json!({}));
            let everything = mcp.call_ok("tasks_list", json!({ "all_workspaces": true }));

            assert_eq!(created["workspace_id"], ws["id"]);
            assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
            assert_eq!(text(&listed[0], "title"), "from an agent");
            assert_eq!(everything.as_array().unwrap().len(), 2, "{everything}");
        }

        #[test]
        fn should_update_and_get_a_task() {
            let env = TestEnv::new();
            let ws = env.workspace("web");
            let task = env.json(&["task", "add", "Fix the build", "--ws", "web"]);
            let mut mcp = McpClient::start(&env, &[("AGENTDECK_WORKSPACE", text(&ws, "slug"))]);
            mcp.initialize();

            mcp.call_ok(
                "task_update",
                json!({ "id": task["id"], "status": "doing" }),
            );
            let got = mcp.call_ok("task_get", json!({ "id": task["id"] }));

            assert_eq!(text(&got, "status"), "doing");
        }

        #[test]
        fn should_comment_on_a_task_as_the_callers_pane_and_list_its_comments() {
            let env = TestEnv::new();
            let ws = env.workspace("web");
            let task = env.json(&["task", "add", "Fix the build", "--ws", "web"]);
            env.ok(&[
                "comment",
                "add",
                "look at the logs",
                "--task",
                text(&task, "id"),
            ]);
            let pane = env.json(&["agent", "new", "--ws", "web"]);
            let mut mcp = McpClient::start(
                &env,
                &[
                    ("AGENTDECK_WORKSPACE", text(&ws, "id")),
                    ("AGENTDECK_PANE", text(&pane, "id")),
                ],
            );
            mcp.initialize();

            let comment = mcp.call_ok(
                "task_comment",
                json!({ "task": task["id"], "body": "PR opened" }),
            );
            let thread = mcp.call_ok("task_comments", json!({ "task": task["id"] }));

            assert_eq!(comment["author"], "agent");
            assert_eq!(comment["author_agent"], "claude");
            assert_eq!(comment["author_pane"], pane["id"]);
            let bodies: Vec<&str> = thread
                .as_array()
                .unwrap()
                .iter()
                .map(|c| text(c, "body"))
                .collect();
            assert_eq!(bodies, ["look at the logs", "PR opened"]);
        }

        #[test]
        fn should_list_the_callers_workspace_instructions() {
            let env = TestEnv::new();
            let ws = env.workspace("web");
            env.workspace("api");
            env.ok(&["instruction", "add", "Use the staging DB", "--ws", "web"]);
            env.ok(&["instruction", "add", "Not yours", "--ws", "api"]);
            let mut mcp = McpClient::start(&env, &[("AGENTDECK_WORKSPACE", text(&ws, "id"))]);
            mcp.initialize();

            let mine = mcp.call_ok("instructions_list", json!({}));
            let theirs = mcp.call_ok("instructions_list", json!({ "workspace": "api" }));

            assert_eq!(mine.as_array().unwrap().len(), 1, "{mine}");
            assert_eq!(text(&mine[0], "body"), "Use the staging DB");
            assert_eq!(text(&theirs[0], "body"), "Not yours");
        }
    }

    mod tool_errors {
        use super::*;

        #[test]
        fn should_report_a_daemon_error_as_a_tool_error() {
            let env = TestEnv::new();
            let mut mcp = McpClient::start(&env, &[]);
            mcp.initialize();

            let result = mcp.call(
                "task_get",
                json!({ "id": "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b" }),
            );

            assert_eq!(result["isError"], true, "{result}");
            assert!(
                result["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("not_found"),
                "{result}"
            );
        }

        #[test]
        fn should_ask_for_a_workspace_outside_one() {
            let env = TestEnv::new();
            let mut mcp = McpClient::start(&env, &[]);
            mcp.initialize();

            let result = mcp.call("instructions_list", json!({}));

            assert_eq!(result["isError"], true, "{result}");
        }
    }
}
