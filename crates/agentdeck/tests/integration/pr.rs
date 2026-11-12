//! `agentdeck pr sync | status`, with `gh` logged out.

use crate::support::TestEnv;

mod pr_status {
    use super::*;

    #[test]
    fn should_report_a_logged_out_gh() {
        let env = TestEnv::new();

        let stdout = env.ok(&["pr", "status"]);

        assert!(stdout.starts_with("gh:        logged_out\n"), "{stdout}");
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
}
