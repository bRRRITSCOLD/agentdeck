//! `agentdeck repo ls | link | unlink`, against httpmock as GitHub's REST and GraphQL APIs.

use std::ffi::OsString;

use httpmock::MockServer;
use serde_json::{Value, json};

use crate::support::github::{
    Pr, mock_open_prs, mock_repo, mock_repo_by_id_missing, mock_repo_missing, repository_id,
};
use crate::support::{TestEnv, text};

const TOKEN: &str = "gho_test";

/// A world whose `gh` is signed in and whose daemon has synced `prs`, so their Repositories
/// are kept.
fn synced(github: &MockServer, prs: &[Pr]) -> TestEnv {
    mock_open_prs(github, TOKEN, prs);
    let env = TestEnv::with_github(&github.url("/graphql"), TOKEN);
    let status = env.json(&["pr", "sync"]);
    assert_eq!(status["last_error"], Value::Null, "{status}");
    env
}

/// A world whose `gh` is signed in, where GitHub shows `repos`.
fn showing(github: &MockServer, repos: &[&str]) -> TestEnv {
    for repo in repos {
        mock_repo(github, TOKEN, repo);
    }
    TestEnv::with_github(&github.url("/graphql"), TOKEN)
}

const WIDGETS: &str = "acme/widgets";
const GADGETS: &str = "acme/gadgets";

mod repo_ls {
    use super::*;

    #[test]
    fn should_list_each_repository_with_the_workspaces_linking_it() {
        let github = MockServer::start();
        mock_repo(&github, TOKEN, WIDGETS);
        let env = synced(&github, &[Pr::open(GADGETS, 9)]);
        env.workspace("web");
        env.workspace("api");
        env.ok(&["repo", "link", WIDGETS, "--ws", "web"]);
        env.ok(&["repo", "link", WIDGETS, "--ws", "api"]);

        let stdout = env.ok(&["repo", "ls"]);

        assert_eq!(
            stdout,
            "REPOSITORY    WORKSPACES\n\
             acme/gadgets  -\n\
             acme/widgets  web, api\n"
        );
    }

    #[test]
    fn should_list_only_the_repositories_a_workspace_links() {
        let github = MockServer::start();
        let env = showing(&github, &[WIDGETS, GADGETS]);
        env.workspace("web");
        env.workspace("api");
        env.ok(&["repo", "link", WIDGETS, "--ws", "web"]);
        env.ok(&["repo", "link", GADGETS, "--ws", "api"]);

        let stdout = env.ok(&["repo", "ls", "--ws", "api"]);

        assert_eq!(stdout, "REPOSITORY    WORKSPACES\nacme/gadgets  api\n");
    }

    #[test]
    fn should_print_each_repository_with_its_workspaces_ids_and_unreachable_with_json() {
        let github = MockServer::start();
        let env = showing(&github, &[WIDGETS]);
        let web = env.workspace("web");
        env.ok(&["repo", "link", WIDGETS, "--ws", "web"]);

        let repositories = env.json(&["repo", "ls"]);

        assert_eq!(
            repositories,
            json!([{
                "id": repository_id(WIDGETS),
                "name": WIDGETS,
                "url": "https://github.com/acme/widgets",
                "private": false,
                "archived": false,
                "unreachable": false,
                "workspaces": [text(&web, "id")]
            }])
        );
    }

    /// A linked Repository GitHub no longer shows the sign-in, once `pr sync` read it by id.
    fn unreachable_widgets(github: &MockServer) -> TestEnv {
        let env = showing(github, &[WIDGETS]);
        mock_open_prs(github, TOKEN, &[]);
        mock_repo_by_id_missing(github, WIDGETS);
        env.workspace("web");
        env.ok(&["repo", "link", WIDGETS, "--ws", "web"]);
        let status = env.json(&["pr", "sync"]);
        assert_eq!(status["last_error"], Value::Null, "{status}");
        env
    }

    #[test]
    fn should_mark_a_repository_github_no_longer_shows_unreachable_and_keep_its_link() {
        let github = MockServer::start();
        let env = unreachable_widgets(&github);

        let stdout = env.ok(&["repo", "ls"]);

        assert_eq!(
            stdout,
            "REPOSITORY                  WORKSPACES\n\
             acme/widgets (unreachable)  web\n"
        );
    }

    #[test]
    fn should_say_it_is_unreachable_with_json() {
        let github = MockServer::start();
        let env = unreachable_widgets(&github);

        let repositories = env.json(&["repo", "ls"]);

        assert_eq!(repositories[0]["name"], WIDGETS);
        assert_eq!(repositories[0]["unreachable"], true);
    }

    #[test]
    fn should_say_when_there_are_none() {
        let env = TestEnv::new();

        assert_eq!(env.ok(&["repo", "ls"]), "no repositories\n");
    }
}

mod repo_link {
    use super::*;

    #[test]
    fn should_link_a_repository_github_finds_by_its_url() {
        let github = MockServer::start();
        let env = showing(&github, &[WIDGETS]);
        env.workspace("web");

        let stdout = env.ok(&[
            "repo",
            "link",
            "https://github.com/acme/widgets",
            "--ws",
            "web",
        ]);

        assert_eq!(stdout, "linked acme/widgets to web\n");
        assert!(env.ok(&["repo", "ls"]).contains("acme/widgets  web\n"));
    }

    #[test]
    fn should_print_the_repository_with_the_workspaces_linking_it_with_json() {
        let github = MockServer::start();
        let env = showing(&github, &[WIDGETS]);
        let web = env.workspace("web");

        let linked = env.json(&["repo", "link", WIDGETS, "--ws", "web"]);

        assert_eq!(linked["name"], WIDGETS);
        assert_eq!(linked["id"], repository_id(WIDGETS));
        assert_eq!(linked["workspaces"], json!([text(&web, "id")]));
    }

    #[test]
    fn should_refuse_a_repository_github_cannot_see() {
        let github = MockServer::start();
        mock_repo_missing(&github, "acme/nope");
        let env = showing(&github, &[]);
        env.workspace("web");

        let stderr = env.fails(&["repo", "link", "acme/nope", "--ws", "web"]);

        assert_eq!(
            stderr,
            "agentdeck: GitHub can't see acme/nope. Check the name or your token's access. \
             (unknown_repository)\n"
        );
        assert_eq!(env.ok(&["repo", "ls"]), "no repositories\n");
    }

    #[test]
    fn should_not_take_its_workspace_from_agentdeck_workspace() {
        let github = MockServer::start();
        let env = showing(&github, &[WIDGETS]);
        let web = env.workspace("web");
        let id = OsString::from(text(&web, "id"));

        let run = env.run_with(&["repo", "link", WIDGETS], &[("AGENTDECK_WORKSPACE", &id)]);

        assert!(!run.success, "{}", run.stdout);
        assert!(run.stderr.contains("--ws"), "{}", run.stderr);
        assert_eq!(env.ok(&["repo", "ls"]), "no repositories\n");
    }
}

mod repo_unlink {
    use super::*;

    /// A world where workspace `web` links `acme/widgets`.
    fn web_linking_widgets(github: &MockServer) -> (TestEnv, Value) {
        let env = showing(github, &[WIDGETS]);
        let web = env.workspace("web");
        env.ok(&["repo", "link", WIDGETS, "--ws", "web"]);
        (env, web)
    }

    #[test]
    fn should_unlink_and_forget_a_repository_nothing_else_names() {
        let github = MockServer::start();
        let (env, _web) = web_linking_widgets(&github);

        let stdout = env.ok(&["repo", "unlink", "ACME/Widgets", "--ws", "web"]);

        assert_eq!(stdout, "unlinked acme/widgets from web\n");
        assert_eq!(env.ok(&["repo", "ls"]), "no repositories\n");
    }

    #[test]
    fn should_print_what_it_unlinked_with_json() {
        let github = MockServer::start();
        let (env, web) = web_linking_widgets(&github);

        let unlinked = env.json(&["repo", "unlink", WIDGETS, "--ws", "web"]);

        assert_eq!(
            unlinked,
            json!({
                "workspace_id": text(&web, "id"),
                "repository_id": repository_id(WIDGETS),
                "unlinked": true
            })
        );
    }

    #[test]
    fn should_keep_a_repository_one_of_your_pull_requests_belongs_to() {
        let github = MockServer::start();
        let env = synced(&github, &[Pr::open(WIDGETS, 7)]);
        env.workspace("web");
        env.ok(&["repo", "link", WIDGETS, "--ws", "web"]);

        env.ok(&["repo", "unlink", WIDGETS, "--ws", "web"]);

        assert_eq!(
            env.ok(&["repo", "ls"]),
            "REPOSITORY    WORKSPACES\nacme/widgets  -\n"
        );
    }

    #[test]
    fn should_refuse_a_repository_the_workspace_does_not_link() {
        let github = MockServer::start();
        let (env, _web) = web_linking_widgets(&github);
        env.workspace("api");

        let stderr = env.fails(&["repo", "unlink", WIDGETS, "--ws", "api"]);

        assert_eq!(
            stderr,
            "agentdeck: acme/widgets isn't linked to api (not_found)\n"
        );
    }
}
