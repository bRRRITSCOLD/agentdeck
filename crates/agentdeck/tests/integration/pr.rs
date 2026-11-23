//! `agentdeck pr sync | status`, with `gh` logged out, and `pr ls` (by Repository and by
//! workspace) and `pr close` against httpmock.

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

mod pr_ls {
    use httpmock::MockServer;
    use serde_json::Value;

    use super::*;
    use crate::support::github::{Pr, mock_open_prs};

    const TOKEN: &str = "gho_test";

    /// A world whose daemon has synced `prs`.
    fn synced(github: &MockServer, prs: &[Pr]) -> TestEnv {
        mock_open_prs(github, TOKEN, prs);
        let env = TestEnv::with_github(&github.url("/graphql"), TOKEN);
        let status = env.json(&["pr", "sync"]);
        assert_eq!(status["last_error"], Value::Null, "{status}");
        env
    }

    fn widgets(number: i64) -> Pr {
        Pr {
            repo: "acme/widgets",
            number,
        }
    }

    const GADGET: Pr = Pr {
        repo: "acme/gadgets",
        number: 9,
    };

    #[test]
    fn should_list_each_pr_by_its_reference_with_its_state() {
        let github = MockServer::start();
        let env = synced(&github, &[widgets(7), GADGET]);

        let stdout = env.ok(&["pr", "ls"]);

        assert_eq!(
            stdout,
            "PR              STATE  TITLE\n\
             acme/gadgets#9  open   PR 9\n\
             acme/widgets#7  open   PR 7\n"
        );
    }

    #[test]
    fn should_list_only_the_repository_given_by_name_or_url() {
        let github = MockServer::start();
        let env = synced(&github, &[widgets(7), widgets(8), GADGET]);

        let by_name = env.ok(&["pr", "ls", "--repo", "acme/gadgets"]);
        let by_url = env.ok(&["pr", "ls", "--repo", "https://github.com/acme/widgets"]);

        assert_eq!(
            by_name,
            "PR              STATE  TITLE\nacme/gadgets#9  open   PR 9\n"
        );
        assert!(by_url.contains("acme/widgets#7"), "{by_url}");
        assert!(by_url.contains("acme/widgets#8"), "{by_url}");
        assert!(!by_url.contains("acme/gadgets"), "{by_url}");
    }

    #[test]
    fn should_print_each_pull_request_with_its_repository_with_json() {
        let github = MockServer::start();
        let env = synced(&github, &[widgets(7)]);

        let prs = env.json(&["pr", "ls"]);

        let pr = &prs.as_array().expect("a list")[0];
        assert_eq!(pr["reference"], "acme/widgets#7");
        assert_eq!(pr["state"], "open");
        assert_eq!(pr["head_ref"], "feat/x");
        assert_eq!(pr["base_ref"], "main");
        assert_eq!(pr["author"], "octocat");
        assert_eq!(pr["repository"]["name"], "acme/widgets");
        assert!(pr["repository"]["id"].is_u64(), "{pr}");
    }

    #[test]
    fn should_list_only_the_pull_requests_of_the_repositories_a_workspace_links() {
        let github = MockServer::start();
        let env = synced(&github, &[widgets(7), GADGET]);
        env.workspace("web");
        env.workspace("api");
        env.ok(&["repo", "link", "acme/gadgets", "--ws", "web"]);

        let webs = env.ok(&["pr", "ls", "--ws", "web"]);
        let apis = env.ok(&["pr", "ls", "--ws", "api"]);
        let webs_json = env.json(&["pr", "ls", "--ws", "web"]);

        assert_eq!(
            webs,
            "PR              STATE  TITLE\nacme/gadgets#9  open   PR 9\n"
        );
        assert_eq!(apis, "no pull requests\n");
        let references: Vec<&Value> = (webs_json.as_array().expect("a list").iter())
            .map(|pr| &pr["reference"])
            .collect();
        assert_eq!(references, [&Value::from("acme/gadgets#9")]);
    }

    #[test]
    fn should_list_every_pull_request_whatever_agentdeck_workspace_says() {
        let github = MockServer::start();
        let env = synced(&github, &[widgets(7), GADGET]);
        let web = env.workspace("web");
        env.ok(&["repo", "link", "acme/gadgets", "--ws", "web"]);
        let id = std::ffi::OsString::from(crate::support::text(&web, "id"));

        let run = env.run_with(&["pr", "ls"], &[("AGENTDECK_WORKSPACE", &id)]);

        assert!(run.success, "{}", run.stderr);
        assert!(run.stdout.contains("acme/widgets#7"), "{}", run.stdout);
        assert!(run.stdout.contains("acme/gadgets#9"), "{}", run.stdout);
    }

    #[test]
    fn should_say_when_there_are_none() {
        let env = TestEnv::new();

        assert_eq!(env.ok(&["pr", "ls"]), "no pull requests\n");
    }

    #[test]
    fn should_refuse_a_repository_it_cannot_read() {
        let env = TestEnv::new();

        let run = env.run(&["pr", "ls", "--repo", "widgets"]);

        assert!(!run.success);
        assert!(
            run.stderr.contains("`widgets` isn't a Repository"),
            "{}",
            run.stderr
        );
    }
}

mod pr_close {
    use httpmock::MockServer;
    use serde_json::{Value, json};

    use super::*;
    use crate::support::github::{Pr, mock_close, mock_node_ids, mock_open_prs};
    use crate::support::pty::run_answering;

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

    /// `pr`'s reference, `owner/name#number`.
    fn reference(pr: Pr) -> String {
        format!("{}#{}", pr.repo, pr.number)
    }

    /// The state `pr ls --json` lists `pr` in.
    fn state(env: &TestEnv, pr: Pr) -> Value {
        let prs = env.json(&["pr", "ls"]);
        let listed = prs
            .as_array()
            .expect("a list of PRs")
            .iter()
            .find(|listed| listed["reference"] == reference(pr))
            .unwrap_or_else(|| panic!("no {}: {prs}", reference(pr)));
        listed["state"].clone()
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

        let stdout = env.ok(&["pr", "close", &reference(SEVEN), &reference(EIGHT), "--yes"]);

        assert_eq!(stdout, "closed acme/widgets#7\nclosed acme/widgets#8\n");
        for close in &closes {
            close.assert_calls(1);
        }
        // A closed PR is Finished, which `pr ls` leaves out.
        assert_eq!(env.json(&["pr", "ls"]), json!([]));
    }

    #[test]
    fn should_close_a_pr_named_by_its_url() {
        let github = MockServer::start();
        let env = synced(&github, &[SEVEN, EIGHT]);
        mock_node_ids(&github, &[SEVEN]);
        let close = mock_close(&github, SEVEN, None);

        let stdout = env.ok(&[
            "pr",
            "close",
            "https://github.com/acme/widgets/pull/7/files",
            "--yes",
        ]);

        assert_eq!(stdout, "closed acme/widgets#7\n");
        close.assert_calls(1);
        assert_eq!(state(&env, EIGHT), "open");
    }

    #[test]
    fn should_refuse_what_names_no_pull_request_and_close_nothing() {
        let github = MockServer::start();
        let env = synced(&github, &[SEVEN]);
        let close = mock_close(&github, SEVEN, None);

        let run = env.run(&["pr", "close", "acme/widgets", "--yes"]);

        assert!(!run.success);
        assert!(
            run.stderr.contains("`acme/widgets` isn't a pull request"),
            "{}",
            run.stderr
        );
        close.assert_calls(0);
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

        let run = env.run(&["pr", "close", &reference(SEVEN), &reference(EIGHT), "--yes"]);

        assert!(!run.success, "{}", run.stdout);
        assert_eq!(
            run.stdout,
            "closed acme/widgets#7\n\
             failed acme/widgets#8: Resource not accessible by integration\n\
             Closed 1 of 2\n"
        );
    }

    #[test]
    fn should_print_the_results_object_with_json() {
        let github = MockServer::start();
        let env = synced(&github, &[SEVEN]);
        mock_node_ids(&github, &[SEVEN]);
        mock_close(&github, SEVEN, None);

        let results = env.json(&["pr", "close", "acme/widgets#7", "--yes"]);

        assert_eq!(
            results,
            json!({ "results": [ { "pr": "acme/widgets#7", "ok": true } ] })
        );
    }

    #[test]
    fn should_fail_naming_yes_without_a_terminal_and_close_nothing() {
        let github = MockServer::start();
        let env = synced(&github, &[SEVEN]);
        mock_node_ids(&github, &[SEVEN]);
        let close = mock_close(&github, SEVEN, None);

        let stderr = env.fails(&["pr", "close", &reference(SEVEN)]);

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
        let refs = [reference(SEVEN), reference(EIGHT), reference(NINE)];

        let run = run_answering(
            &env,
            &["pr", "close", &refs[0], &refs[1], &refs[2]],
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
            run.output.contains("closed acme/gadgets#9"),
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

        let run = run_answering(
            &env,
            &["pr", "close", &reference(SEVEN)],
            "[y/N]",
            "yes please",
        );

        assert!(
            run.output.contains("Close 1 PR on GitHub?"),
            "{}",
            run.output
        );
        assert!(run.output.contains("Closed nothing"), "{}", run.output);
        close.assert_calls(0);
    }
}
