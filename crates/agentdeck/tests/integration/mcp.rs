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
                "note_create",
                "note_mark_read",
                "notes_list",
                "prs_status",
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
        fn should_leave_list_and_mark_notes_read_as_the_callers_pane() {
            let env = TestEnv::new();
            let ws = env.workspace("web");
            let pane = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";
            let mut mcp = McpClient::start(
                &env,
                &[
                    ("AGENTDECK_WORKSPACE", text(&ws, "id")),
                    ("AGENTDECK_PANE", pane),
                ],
            );
            mcp.initialize();

            let note = mcp.call_ok("note_create", json!({ "body": "ping" }));
            let unread = mcp.call_ok("notes_list", json!({ "unread": true }));
            let read = mcp.call_ok("note_mark_read", json!({ "id": note["id"] }));

            assert_eq!(note["workspace_id"], ws["id"]);
            assert_eq!(unread.as_array().unwrap().len(), 1, "{unread}");
            assert_eq!(read["read_by_pane"], pane);
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
        fn should_refuse_a_note_with_nowhere_to_go() {
            let env = TestEnv::new();
            let mut mcp = McpClient::start(&env, &[]);
            mcp.initialize();

            let result = mcp.call("note_create", json!({ "body": "lost" }));

            assert_eq!(result["isError"], true, "{result}");
        }
    }
}
