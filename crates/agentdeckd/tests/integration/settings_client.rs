//! `agentdeck_client::settings`: setting one dotted key, against a real daemon.

use agentdeck_client::settings::set;
use agentdeck_client::types::ThemePreference;
use serde_json::json;

use crate::support::{TestDaemon, set_daemon_env};

/// A daemon that reads `config.toml` alone.
async fn daemon() -> TestDaemon {
    let dirs = tempfile::tempdir().expect("a temp dir");
    set_daemon_env(dirs.path(), &[("AGENTDECK_PR_MAX_AGE_DAYS", "")]);
    TestDaemon::start_in(dirs).await
}

mod set {
    use super::*;

    #[tokio::test]
    async fn should_set_the_key_and_answer_with_every_setting() {
        let daemon = daemon().await;

        let settings = set(&daemon.client, "appearance.theme", json!("light"))
            .await
            .expect("set the theme");

        assert_eq!(settings.appearance.theme, ThemePreference::Light);
        assert_eq!(settings.pr_sync.max_age_days, 90);
    }

    #[tokio::test]
    async fn should_give_the_daemons_message_and_code_for_a_value_it_does_not_take() {
        let daemon = daemon().await;

        let err = set(&daemon.client, "pr_sync.max_age_days", json!("soon"))
            .await
            .expect_err("a refusal");

        assert_eq!(err.code.as_deref(), Some("invalid_setting"));
        assert!(err.message.starts_with("pr_sync.max_age_days "), "{err}");
    }
}
