//! `agentdeck instruction ls | add | edit | rm`.

use std::ffi::OsString;

use crate::support::{TestEnv, text};

mod instruction_add {
    use super::*;

    #[test]
    fn should_add_an_instruction_to_the_workspace_named() {
        let env = TestEnv::new();
        let ws = env.workspace("web");

        let added = env.json(&["instruction", "add", "Use the staging DB", "--ws", "web"]);

        assert_eq!(added["workspace_id"], ws["id"]);
        assert_eq!(added["body"], "Use the staging DB");
    }

    #[test]
    fn should_default_to_the_callers_workspace() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        let caller = OsString::from(text(&ws, "id"));

        let added = env.json_with(
            &["instruction", "add", "Use the staging DB"],
            &[("AGENTDECK_WORKSPACE", &caller)],
        );

        assert_eq!(added["workspace_id"], ws["id"]);
    }

    #[test]
    fn should_ask_for_a_workspace_outside_one() {
        let env = TestEnv::new();

        let stderr = env.fails(&["instruction", "add", "Use the staging DB"]);

        assert!(stderr.contains("--ws"), "{stderr}");
    }
}

mod instruction_ls {
    use super::*;

    #[test]
    fn should_list_the_workspaces_instructions_oldest_first() {
        let env = TestEnv::new();
        env.workspace("web");
        env.workspace("api");
        env.ok(&["instruction", "add", "first", "--ws", "web"]);
        env.ok(&["instruction", "add", "elsewhere", "--ws", "api"]);
        env.ok(&["instruction", "add", "second", "--ws", "web"]);

        let listed = env.json(&["instruction", "ls", "--ws", "web"]);

        let bodies: Vec<&str> = listed
            .as_array()
            .unwrap()
            .iter()
            .map(|i| text(i, "body"))
            .collect();
        assert_eq!(bodies, ["first", "second"]);
    }

    #[test]
    fn should_print_each_id_and_first_line() {
        let env = TestEnv::new();
        env.workspace("web");
        let added = env.json(&["instruction", "add", "first\nsecond", "--ws", "web"]);

        let stdout = env.ok(&["instruction", "ls", "--ws", "web"]);

        assert!(stdout.starts_with("ID"), "{stdout}");
        assert!(stdout.contains(text(&added, "id")), "{stdout}");
        assert!(stdout.contains("first …"), "{stdout}");
        assert!(!stdout.contains("second"), "{stdout}");
    }
}

mod instruction_edit {
    use super::*;

    #[test]
    fn should_replace_the_body() {
        let env = TestEnv::new();
        env.workspace("web");
        let added = env.json(&["instruction", "add", "old", "--ws", "web"]);

        let edited = env.json(&["instruction", "edit", text(&added, "id"), "new"]);

        assert_eq!(edited["id"], added["id"]);
        assert_eq!(edited["body"], "new");
        let listed = env.json(&["instruction", "ls", "--ws", "web"]);
        assert_eq!(listed[0]["body"], "new");
    }
}

mod instruction_rm {
    use super::*;

    #[test]
    fn should_delete_the_instruction() {
        let env = TestEnv::new();
        env.workspace("web");
        let added = env.json(&["instruction", "add", "gone soon", "--ws", "web"]);

        let removed = env.json(&["instruction", "rm", text(&added, "id")]);

        assert_eq!(removed["id"], added["id"]);
        assert_eq!(removed["deleted"], true);
        let listed = env.json(&["instruction", "ls", "--ws", "web"]);
        assert_eq!(listed.as_array().unwrap().len(), 0, "{listed}");
    }

    #[test]
    fn should_fail_for_an_unknown_instruction() {
        let env = TestEnv::new();

        let stderr = env.fails(&["instruction", "rm", "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b"]);

        assert!(stderr.contains("not_found"), "{stderr}");
    }
}
