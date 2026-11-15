//! `agentdeck_client::ApiError` against a daemon that lacks a route the client calls, as an
//! older daemon does (spec §2): what the real daemon answers is never a bare "Not Found".

use agentdeck_client::ApiError;
use agentdeck_client::error::OLDER_DAEMON;

use crate::support::TestDaemon;

mod a_route_the_daemon_lacks {
    use super::*;

    #[tokio::test]
    async fn should_say_the_daemon_is_older_than_the_client_404() {
        let daemon = TestDaemon::start().await;
        // Under a prefix, every route the client knows is one the daemon lacks.
        let client = agentdeck_client::authenticated(
            &format!("{}/later", daemon.base_url()),
            &daemon.info.token,
        )
        .expect("a client");

        let err = ApiError::from(client.health().send().await.expect_err("no such route"));

        assert_eq!(err.message, OLDER_DAEMON);
        assert_eq!(err.code, None);
    }
}
