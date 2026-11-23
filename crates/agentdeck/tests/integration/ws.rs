//! `agentdeck ws ls | new | rename | archive | mute | unmute | rm`. A workspace has no
//! folder: none of them makes or removes one.

use httpmock::MockServer;
use serde_json::json;

use crate::support::github::mock_repo;
use crate::support::{TestEnv, folder, shell_pane, text};

const TOKEN: &str = "gho_test";
const WIDGETS: &str = "acme/widgets";
const GADGETS: &str = "acme/gadgets";

/// A world whose `gh` is signed in, where GitHub shows `repos`, so they can be linked.
fn showing(github: &MockServer, repos: &[&str]) -> TestEnv {
    for repo in repos {
        mock_repo(github, TOKEN, repo);
    }
    TestEnv::with_github(&github.url("/graphql"), TOKEN)
}

mod ws_new {
    use super::*;

    #[test]
    fn should_create_a_workspace_with_no_folder() {
        let env = TestEnv::new();

        let ws = env.json(&["ws", "new", "Web app"]);

        assert_eq!(text(&ws, "name"), "Web app");
        assert_eq!(text(&ws, "slug"), "web-app");
        assert_eq!(ws.get("dir"), None, "{ws}");
        assert_eq!(ws.get("dir_owned"), None, "{ws}");
        assert!(
            !env.root().join("data/agentdeck/workspaces").exists(),
            "no scratch folder"
        );
    }

    #[test]
    fn should_print_the_id_and_slug() {
        let env = TestEnv::new();

        let stdout = env.ok(&["ws", "new", "Scratch"]);

        let ws = &env.json(&["ws", "ls"])[0];
        assert_eq!(
            stdout,
            format!("{}  {}\n", text(ws, "id"), text(ws, "slug"))
        );
    }

    #[test]
    fn should_take_no_dir_or_repo() {
        let env = TestEnv::new();
        let dir = env.new_dir("web");

        for flag in [["--dir", dir.to_str().unwrap()], ["--repo", WIDGETS]] {
            let stderr = env.fails(&["ws", "new", "web", flag[0], flag[1]]);

            assert!(stderr.contains(flag[0]), "{stderr}");
        }
        assert_eq!(env.json(&["ws", "ls"]), json!([]));
    }
}

mod ws_ls {
    use super::*;

    #[test]
    fn should_list_workspaces_with_their_repositories() {
        let github = MockServer::start();
        let env = showing(&github, &[WIDGETS, GADGETS]);
        let web = env.workspace("web");
        let api = env.workspace("api");
        env.ok(&["repo", "link", WIDGETS, "--ws", "web"]);
        env.ok(&["repo", "link", GADGETS, "--ws", "web"]);

        let stdout = env.ok(&["ws", "ls"]);

        let pad = " ".repeat(text(&web, "id").len() - 2);
        assert_eq!(
            stdout,
            format!(
                "ID{pad}  SLUG  NAME  REPOSITORIES\n\
                 {}  web   web   acme/gadgets, acme/widgets\n\
                 {}  api   api   -\n",
                text(&web, "id"),
                text(&api, "id"),
            )
        );
    }

    #[test]
    fn should_print_each_workspace_with_its_repositories_as_an_array_with_json() {
        let github = MockServer::start();
        let env = showing(&github, &[WIDGETS]);
        let web = env.workspace("web");
        env.workspace("api");
        env.ok(&["repo", "link", WIDGETS, "--ws", "web"]);

        let listed = env.json(&["ws", "ls"]);

        assert_eq!(listed[0]["id"], web["id"]);
        assert_eq!(listed[0]["slug"], "web");
        assert_eq!(listed[0]["repositories"], json!([WIDGETS]));
        assert_eq!(listed[1]["repositories"], json!([]));
        assert_eq!(listed[0].get("dir"), None, "{listed}");
    }

    #[test]
    fn should_say_so_when_there_are_none() {
        let env = TestEnv::new();

        assert_eq!(env.ok(&["ws", "ls"]), "no workspaces\n");
    }
}

mod ws_rename {
    use super::*;

    #[test]
    fn should_rename_by_slug_and_print_it_as_ws_ls_does() {
        let env = TestEnv::new();
        let ws = env.workspace("web");

        let stdout = env.ok(&["ws", "rename", text(&ws, "slug"), "Web app"]);

        assert_eq!(stdout, env.ok(&["ws", "ls"]));
        assert!(stdout.contains("Web app"), "{stdout}");
    }

    #[test]
    fn should_print_its_repositories_as_ws_ls_does() {
        let github = MockServer::start();
        let env = showing(&github, &[WIDGETS]);
        env.workspace("web");
        env.ok(&["repo", "link", WIDGETS, "--ws", "web"]);

        let stdout = env.ok(&["ws", "rename", "web", "Web app"]);

        assert_eq!(stdout, env.ok(&["ws", "ls"]));
        assert!(stdout.contains("Web app  acme/widgets\n"), "{stdout}");
    }

    #[test]
    fn should_keep_the_slug_so_the_old_slug_still_finds_it() {
        let env = TestEnv::new();
        let ws = env.workspace("web");

        let renamed = env.json(&["ws", "rename", "web", "Web app"]);

        assert_eq!(renamed["id"], ws["id"]);
        assert_eq!(text(&renamed, "name"), "Web app");
        assert_eq!(renamed["slug"], ws["slug"]);
        env.ok(&["ws", "mute", text(&ws, "slug")]);
    }

    #[test]
    fn should_say_nothing_of_a_dir_in_its_help() {
        let env = TestEnv::new();

        let help = env.ok(&["ws", "rename", "--help"]);

        assert!(!help.contains("dir"), "{help}");
    }

    #[test]
    fn should_refuse_a_blank_name() {
        let env = TestEnv::new();
        env.workspace("web");

        let stderr = env.fails(&["ws", "rename", "web", " "]);

        assert!(stderr.contains("invalid_name"), "{stderr}");
        assert_eq!(text(&env.json(&["ws", "ls"])[0], "name"), "web");
    }

    #[test]
    fn should_fail_for_an_unknown_workspace() {
        let env = TestEnv::new();

        let stderr = env.fails(&["ws", "rename", "nope", "x"]);

        assert!(stderr.contains("unknown_workspace"), "{stderr}");
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

    #[test]
    fn should_delete_by_name() {
        let env = TestEnv::new();
        env.workspace("web");

        let stdout = env.ok(&["ws", "rm", "web"]);

        assert_eq!(stdout, "deleted web\n");
        assert_eq!(env.json(&["ws", "ls"]), json!([]));
    }

    #[test]
    fn should_print_what_it_deleted_with_json() {
        let env = TestEnv::new();
        let ws = env.workspace("web");

        let deleted = env.json(&["ws", "rm", "web"]);

        assert_eq!(deleted, json!({"id": ws["id"], "deleted": true}));
    }

    #[test]
    fn should_delete_no_folder_its_panes_work_in() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        let dir = folder(&env, &ws);
        std::fs::write(dir.join("notes.txt"), "wip").unwrap();
        shell_pane(&env, &ws, "exec sleep 60");

        env.ok(&["ws", "rm", "web"]);

        assert_eq!(
            std::fs::read_to_string(dir.join("notes.txt")).unwrap(),
            "wip"
        );
    }

    #[test]
    fn should_unlink_its_repositories() {
        let github = MockServer::start();
        let env = showing(&github, &[WIDGETS]);
        env.workspace("web");
        env.workspace("api");
        env.ok(&["repo", "link", WIDGETS, "--ws", "web"]);
        env.ok(&["repo", "link", WIDGETS, "--ws", "api"]);

        env.ok(&["ws", "rm", "web"]);

        assert_eq!(
            env.ok(&["repo", "ls"]),
            "REPOSITORY    WORKSPACES\nacme/widgets  api\n"
        );
    }

    #[test]
    fn should_take_no_force() {
        let env = TestEnv::new();
        env.workspace("web");

        let stderr = env.fails(&["ws", "rm", "web", "--force"]);

        assert!(stderr.contains("--force"), "{stderr}");
        assert_eq!(env.json(&["ws", "ls"]).as_array().unwrap().len(), 1);
    }
}
