//! `agentdeck pr sync | status`, with `gh` logged out, and `pr close` against httpmock.

use crate::support::TestEnv;

mod pr_status {
    use super::*;

    #[test]
    fn should_report_a_logged_out_github_login() {
        let env = TestEnv::new();

        let stdout = env.ok(&["pr", "status"]);

        assert!(stdout.starts_with("github:    logged_out\n"), "{stdout}");
    }
}

mod pr_sync {
    use super::*;

    #[test]
    fn should_answer_with_the_status_the_sync_left() {
        let env = TestEnv::new();

        let status = env.json(&["pr", "sync"]);

        assert_eq!(status["login"], "logged_out");
        assert!(status["last_error"].is_string(), "{status}");
    }

    #[test]
    fn should_run_a_full_sync_though_an_incremental_one_is_due() {
        use crate::support::github::{Pr, mock_open_prs};
        let github = httpmock::MockServer::start();
        let seven = Pr {
            repo: "acme/widgets",
            number: 7,
        };
        let mocks = mock_open_prs(&github, "gho_test", &[seven]);
        let env = TestEnv::with_github(&github.url("/graphql"), "gho_test");

        env.json(&["pr", "sync"]);
        let status = env.json(&["pr", "sync"]);

        assert_eq!(status["last_error"], serde_json::Value::Null, "{status}");
        // The daemon's first sync at start, then both of ours.
        mocks.list.assert_calls(3);
        mocks.search.assert_calls(0);
    }
}

mod pr_close {
    use httpmock::MockServer;
    use serde_json::{Value, json};

    use super::*;
    use crate::support::github::{Pr, mock_close, mock_node_ids, mock_open_prs};
    use crate::support::pty::run_answering;
    use crate::support::text;

    const TOKEN: &str = "gho_test";

    const SEVEN: Pr = Pr {
        repo: "acme/widgets",
        number: 7,
    };
    const EIGHT: Pr = Pr {
        repo: "acme/widgets",
        number: 8,
    };
    const NINE: Pr = Pr {
        repo: "acme/gadgets",
        number: 9,
    };

    /// A world whose daemon has synced `prs` into PR tasks.
    fn synced(github: &MockServer, prs: &[Pr]) -> TestEnv {
        mock_open_prs(github, TOKEN, prs);
        let env = TestEnv::with_github(&github.url("/graphql"), TOKEN);
        let status = env.json(&["pr", "sync"]);
        assert_eq!(status["last_error"], Value::Null, "{status}");
        env
    }

    /// The id of PR `pr`'s task.
    fn task_id(env: &TestEnv, pr: Pr) -> String {
        let tasks = env.json(&["task", "ls"]);
        let task = tasks
            .as_array()
            .expect("a task list")
            .iter()
            .find(|task| task["pr"]["number"] == pr.number && task["pr"]["repo"] == pr.repo)
            .unwrap_or_else(|| panic!("no task for {}#{}: {tasks}", pr.repo, pr.number));
        text(task, "id").to_owned()
    }

    #[test]
    fn should_close_each_pr_with_yes_and_say_so() {
        let github = MockServer::start();
        let env = synced(&github, &[SEVEN, EIGHT]);
        mock_node_ids(&github, &[SEVEN, EIGHT]);
        let closes = [
            mock_close(&github, SEVEN, None),
            mock_close(&github, EIGHT, None),
        ];
        let ids = [task_id(&env, SEVEN), task_id(&env, EIGHT)];

        let stdout = env.ok(&["pr", "close", &ids[0], &ids[1], "--yes"]);

        assert_eq!(stdout, "closed acme/widgets #7\nclosed acme/widgets #8\n");
        for close in &closes {
            close.assert_calls(1);
        }
        let task = env.json(&["task", "show", &ids[0]]);
        assert_eq!(task["status"], "done");
    }

    #[test]
    fn should_close_the_prs_of_tasks_named_by_their_short_ids() {
        let github = MockServer::start();
        let env = synced(&github, &[SEVEN, EIGHT]);
        mock_node_ids(&github, &[SEVEN]);
        let close = mock_close(&github, SEVEN, None);
        let id = task_id(&env, SEVEN);

        let stdout = env.ok(&["pr", "close", crate::support::short(&id), "--yes"]);

        assert_eq!(stdout, "closed acme/widgets #7\n");
        close.assert_calls(1);
    }

    #[test]
    fn should_say_how_many_closed_and_exit_1_when_any_failed() {
        let github = MockServer::start();
        let env = synced(&github, &[SEVEN, EIGHT]);
        mock_node_ids(&github, &[SEVEN, EIGHT]);
        mock_close(&github, SEVEN, None);
        mock_close(
            &github,
            EIGHT,
            Some("Resource not accessible by integration"),
        );
        let ids = [task_id(&env, SEVEN), task_id(&env, EIGHT)];

        let run = env.run(&["pr", "close", &ids[0], &ids[1], "--yes"]);

        assert!(!run.success, "{}", run.stdout);
        assert_eq!(
            run.stdout,
            "closed acme/widgets #7\n\
             failed acme/widgets #8: Resource not accessible by integration\n\
             Closed 1 of 2\n"
        );
    }

    #[test]
    fn should_print_the_results_object_with_json() {
        let github = MockServer::start();
        let env = synced(&github, &[SEVEN]);
        mock_node_ids(&github, &[SEVEN]);
        mock_close(&github, SEVEN, None);
        let id = task_id(&env, SEVEN);

        let results = env.json(&["pr", "close", &id, "--yes"]);

        assert_eq!(
            results,
            json!({ "results": [ { "task_id": id, "ok": true } ] })
        );
    }

    #[test]
    fn should_fail_naming_yes_without_a_terminal_and_close_nothing() {
        let github = MockServer::start();
        let env = synced(&github, &[SEVEN]);
        mock_node_ids(&github, &[SEVEN]);
        let close = mock_close(&github, SEVEN, None);
        let id = task_id(&env, SEVEN);

        let stderr = env.fails(&["pr", "close", &id]);

        assert!(stderr.contains("--yes"), "{stderr}");
        close.assert_calls(0);
    }

    #[test]
    fn should_ask_on_a_terminal_and_close_on_y() {
        let github = MockServer::start();
        let env = synced(&github, &[SEVEN, EIGHT, NINE]);
        mock_node_ids(&github, &[SEVEN, EIGHT, NINE]);
        let closes = [
            mock_close(&github, SEVEN, None),
            mock_close(&github, EIGHT, None),
            mock_close(&github, NINE, None),
        ];
        let ids = [
            task_id(&env, SEVEN),
            task_id(&env, EIGHT),
            task_id(&env, NINE),
        ];

        let run = run_answering(
            &env,
            &["pr", "close", &ids[0], &ids[1], &ids[2]],
            "[y/N]",
            "y",
        );

        assert!(run.success, "{}", run.output);
        assert!(
            run.output.contains(
                "Close 3 PRs on GitHub? Their repos' owners will see them closed. \
                 You can reopen a PR on GitHub."
            ),
            "{}",
            run.output
        );
        assert!(
            run.output.contains("  acme/gadgets  1 PR"),
            "{}",
            run.output
        );
        assert!(
            run.output.contains("  acme/widgets  2 PRs"),
            "{}",
            run.output
        );
        assert!(
            run.output.contains("closed acme/gadgets #9"),
            "{}",
            run.output
        );
        for close in &closes {
            close.assert_calls(1);
        }
    }

    #[test]
    fn should_close_nothing_on_any_answer_but_y() {
        let github = MockServer::start();
        let env = synced(&github, &[SEVEN]);
        mock_node_ids(&github, &[SEVEN]);
        let close = mock_close(&github, SEVEN, None);
        let id = task_id(&env, SEVEN);

        let run = run_answering(&env, &["pr", "close", &id], "[y/N]", "yes please");

        assert!(
            run.output.contains("Close 1 PR on GitHub?"),
            "{}",
            run.output
        );
        assert!(run.output.contains("Closed nothing"), "{}", run.output);
        close.assert_calls(0);
    }
}
