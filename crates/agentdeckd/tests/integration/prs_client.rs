//! `agentdeck_client::prs`: the pull requests as the GUI reads them, against a real daemon
//! with httpmock as GitHub. The GUI's fake API answers the same way (spec §12).

use agentdeck_client::prs::list;
use agentdeck_core::PrState;
use httpmock::MockServer;

use crate::support::TestDaemon;
use crate::support::github::{Pr, mock_search, set_gh_token};

const TOKEN: &str = "gho_test";

mod list {
    use super::*;

    #[tokio::test]
    async fn should_list_each_pr_a_sync_kept_with_its_repository_and_branches() {
        let github = MockServer::start_async().await;
        let gadget = Pr {
            repo: "acme/gadgets",
            head_ref: "fix/pane-restore",
            base_ref: "main",
            ..Pr::open(3)
        };
        mock_search(&github, TOKEN, &[Pr::open(7), gadget]);
        let dirs = tempfile::tempdir().expect("a temp dir");
        set_gh_token(dirs.path(), Some(TOKEN));
        let daemon = TestDaemon::start_with_github(dirs, &github.url("/graphql")).await;
        daemon.client.prs_sync().send().await.expect("sync");

        let prs = list(&daemon.client).await.expect("list PRs");

        let listed: Vec<String> = prs.iter().map(|pr| pr.reference.to_string()).collect();
        assert_eq!(listed, ["acme/gadgets#3", "acme/widgets#7"]);
        assert_eq!(prs[0].repository.name.to_string(), "acme/gadgets");
        assert_eq!(
            (prs[0].head_ref.as_str(), prs[0].base_ref.as_str()),
            ("fix/pane-restore", "main")
        );
        assert_eq!(prs[0].state, PrState::Open);
        assert!(!prs[0].unreachable);
    }

    #[tokio::test]
    async fn should_list_none_on_a_daemon_older_than_the_route() {
        let daemon = TestDaemon::start().await;
        // Under a prefix, every route the client knows is one the daemon lacks.
        let older = agentdeck_client::authenticated(
            &format!("{}/later", daemon.base_url()),
            &daemon.info.token,
        )
        .expect("a client");

        let prs = list(&older).await.expect("no failure");

        assert_eq!(prs, []);
    }

    #[tokio::test]
    async fn should_fail_with_the_daemons_words_when_it_refuses_the_token() {
        let daemon = TestDaemon::start().await;
        let stranger =
            agentdeck_client::authenticated(&daemon.base_url(), "not-the-token").expect("a client");

        let err = list(&stranger).await.expect_err("a refusal");

        assert_eq!(err.code.as_deref(), Some("unauthorized"));
    }
}
