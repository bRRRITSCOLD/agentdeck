//! `agentdeck ws ls | new | archive | mute | unmute | rm`.

use std::process::Command;

use crate::support::{TestEnv, text};

mod ws_new {
    use super::*;

    #[test]
    fn should_attach_an_existing_dir_given_relative_to_the_cwd() {
        let env = TestEnv::new();
        env.new_dir("web");

        let output = env
            .command()
            .current_dir(env.root().join("dirs"))
            .args(["ws", "new", "Web app", "--dir", "web", "--json"])
            .output()
            .expect("running agentdeck");

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let ws: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(text(&ws, "name"), "Web app");
        assert_eq!(
            std::path::Path::new(text(&ws, "dir")),
            env.root().join("dirs/web")
        );
        assert_eq!(ws["dir_owned"], false);
    }

    #[test]
    fn should_create_a_scratch_dir_without_one() {
        let env = TestEnv::new();

        let ws = env.json(&["ws", "new", "Scratch"]);

        assert_eq!(ws["dir_owned"], true);
        assert!(std::path::Path::new(text(&ws, "dir")).is_dir());
    }

    #[test]
    fn should_print_the_id_slug_and_dir() {
        let env = TestEnv::new();

        let stdout = env.ok(&["ws", "new", "Scratch"]);

        let ws = &env.json(&["ws", "ls"])[0];
        assert_eq!(
            stdout,
            format!(
                "{}  {}  {}\n",
                text(ws, "id"),
                text(ws, "slug"),
                text(ws, "dir")
            )
        );
    }
}

mod ws_ls {
    use super::*;

    #[test]
    fn should_list_workspaces_under_a_header() {
        let env = TestEnv::new();
        env.workspace("web");
        env.workspace("api");

        let stdout = env.ok(&["ws", "ls"]);

        let lines: Vec<&str> = stdout.lines().collect();
        assert!(lines[0].starts_with("ID"), "{stdout}");
        assert_eq!(lines.len(), 3, "{stdout}");
        assert!(stdout.contains("web") && stdout.contains("api"), "{stdout}");
    }

    #[test]
    fn should_say_so_when_there_are_none() {
        let env = TestEnv::new();

        assert_eq!(env.ok(&["ws", "ls"]), "no workspaces\n");
    }
}

mod ws_archive {
    use super::*;

    #[test]
    fn should_archive_by_slug() {
        let env = TestEnv::new();
        let ws = env.workspace("web");

        let stdout = env.ok(&["ws", "archive", text(&ws, "slug")]);

        assert_eq!(stdout, format!("archived {}\n", text(&ws, "slug")));
        assert!(env.json(&["ws", "ls"])[0]["archived_at"].is_string());
    }

    #[test]
    fn should_fail_for_an_unknown_workspace() {
        let env = TestEnv::new();

        let stderr = env.fails(&["ws", "archive", "nope"]);

        assert!(stderr.contains("unknown_workspace"), "{stderr}");
    }
}

mod ws_mute {
    use super::*;

    #[test]
    fn should_mute_by_slug() {
        let env = TestEnv::new();
        let ws = env.workspace("web");

        let stdout = env.ok(&["ws", "mute", text(&ws, "slug")]);

        assert_eq!(stdout, format!("muted {}\n", text(&ws, "slug")));
        assert_eq!(env.json(&["ws", "ls"])[0]["muted"], true);
    }

    #[test]
    fn should_print_the_workspace_with_json() {
        let env = TestEnv::new();
        let ws = env.workspace("web");

        let muted = env.json(&["ws", "mute", text(&ws, "slug")]);

        assert_eq!(muted["id"], ws["id"]);
        assert_eq!(muted["muted"], true);
    }

    #[test]
    fn should_fail_for_an_unknown_workspace() {
        let env = TestEnv::new();

        let stderr = env.fails(&["ws", "mute", "nope"]);

        assert!(stderr.contains("unknown_workspace"), "{stderr}");
    }
}

mod ws_unmute {
    use super::*;

    #[test]
    fn should_unmute_a_muted_workspace() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        env.ok(&["ws", "mute", text(&ws, "slug")]);

        let stdout = env.ok(&["ws", "unmute", text(&ws, "slug")]);

        assert_eq!(stdout, format!("unmuted {}\n", text(&ws, "slug")));
        assert_eq!(env.json(&["ws", "ls"])[0]["muted"], false);
    }
}

mod ws_rm {
    use super::*;

    /// A workspace whose dir is a git repo with an untracked file.
    fn dirty_workspace(env: &TestEnv) -> serde_json::Value {
        let ws = env.workspace("dirty");
        let dir = text(&ws, "dir");
        let status = Command::new("git")
            .args(["init", "--quiet", dir])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", env.root().join("home"))
            .status()
            .expect("running git");
        assert!(status.success());
        std::fs::write(std::path::Path::new(dir).join("notes.txt"), "wip").unwrap();
        ws
    }

    #[test]
    fn should_delete_by_name() {
        let env = TestEnv::new();
        env.workspace("web");

        let stdout = env.ok(&["ws", "rm", "web"]);

        assert_eq!(stdout, "deleted web\n");
        assert_eq!(env.json(&["ws", "ls"]), serde_json::json!([]));
    }

    #[test]
    fn should_refuse_a_dirty_dir_and_show_what_it_holds() {
        let env = TestEnv::new();
        let ws = dirty_workspace(&env);

        let stderr = env.fails(&["ws", "rm", text(&ws, "slug")]);

        assert!(stderr.contains("--force"), "{stderr}");
        assert!(stderr.contains("notes.txt"), "{stderr}");
        assert_eq!(env.json(&["ws", "ls"]).as_array().unwrap().len(), 1);
    }

    #[test]
    fn should_delete_a_dirty_dir_with_force() {
        let env = TestEnv::new();
        let ws = dirty_workspace(&env);

        let deleted = env.json(&["ws", "rm", text(&ws, "slug"), "--force"]);

        assert_eq!(deleted["deleted"], true);
        assert_eq!(env.json(&["ws", "ls"]), serde_json::json!([]));
    }
}
