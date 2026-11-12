//! `agentdeck --version`.

use crate::support::TestEnv;

mod agentdeck_version {
    use super::*;

    #[test]
    fn should_print_the_name_and_the_workspace_version() {
        let env = TestEnv::new();

        let stdout = env.ok(&["--version"]);

        assert_eq!(stdout, format!("agentdeck {}\n", env!("CARGO_PKG_VERSION")));
        assert!(env.daemon_info().is_none(), "--version started a daemon");
    }
}
