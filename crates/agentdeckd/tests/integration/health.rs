//! `GET /v1/health`.

use crate::support::TestDaemon;

mod get_v1_health {
    use super::*;

    #[tokio::test]
    async fn should_report_the_daemons_version_200() {
        let daemon = TestDaemon::start().await;

        let response = daemon.client.health().send().await.expect("health");

        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(response.into_inner().version, env!("CARGO_PKG_VERSION"));
    }
}
