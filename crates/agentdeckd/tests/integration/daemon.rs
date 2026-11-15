//! The daemon's lifecycle: the lock, `daemon.json`, its files and shutdown (spec §2).

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use crate::support::{Spawned, TestDaemon, eventually_within, hold_lock};

mod agentdeckd {
    use super::*;

    mod daemon_json {
        use super::*;

        #[tokio::test]
        async fn should_be_readable_by_its_owner_only() {
            let daemon = TestDaemon::start().await;

            let mode = std::fs::metadata(daemon.runtime_dir().join("daemon.json"))
                .expect("daemon.json exists")
                .permissions()
                .mode();

            assert_eq!(mode & 0o777, 0o600);
        }

        #[tokio::test]
        async fn should_name_the_port_token_pid_and_version() {
            let daemon = TestDaemon::start().await;

            assert_ne!(daemon.info.port, 0);
            assert!(daemon.info.token.len() >= 32, "{}", daemon.info.token);
            assert_eq!(daemon.info.version, env!("CARGO_PKG_VERSION"));
        }

        #[tokio::test]
        async fn should_name_the_api_version_clients_compare_with_theirs() {
            let daemon = TestDaemon::start().await;

            assert_eq!(daemon.info.api_version, agentdeck_core::API_VERSION);
        }

        #[tokio::test]
        async fn should_be_removed_on_sigterm() {
            let daemon = TestDaemon::start().await;
            let runtime_dir = daemon.runtime_dir();

            let (status, _dirs) = daemon.terminate();

            assert!(status.success(), "{status}");
            assert!(!runtime_dir.join("daemon.json").exists());
        }
    }

    mod lock {
        use super::*;

        #[tokio::test]
        async fn should_refuse_a_second_daemon_while_one_runs() {
            let daemon = TestDaemon::start().await;

            let (status, stderr) = daemon.run_another();

            assert!(!status.success());
            assert!(stderr.contains("another agentdeckd is running"), "{stderr}");
            daemon
                .client
                .health()
                .send()
                .await
                .expect("the first still serves");
        }

        /// The lock held with no `daemon.json`: a daemon that is shutting down, or one
        /// that won the race to start and doesn't serve yet.
        mod held_by_a_daemon_without_daemon_json {
            use super::*;

            fn waiting(root: &std::path::Path) -> Spawned {
                let mut daemon = Spawned::start(root);
                eventually_within(
                    "agentdeckd to wait for the lock",
                    Duration::from_secs(5),
                    || {
                        assert_eq!(daemon.try_wait(), None, "{}", daemon.stderr());
                        daemon
                            .stderr()
                            .contains("waiting for the agentdeckd")
                            .then_some(())
                    },
                );
                daemon
            }

            #[test]
            fn should_give_up_waiting_once_the_other_daemon_serves() {
                let dirs = tempfile::tempdir().expect("a temp dir");
                let _lock = hold_lock(dirs.path());
                let mut daemon = waiting(dirs.path());

                std::fs::write(dirs.path().join("run/agentdeck/daemon.json"), "{}")
                    .expect("writing daemon.json");
                let status =
                    eventually_within("agentdeckd to give up", Duration::from_secs(2), || {
                        daemon.try_wait()
                    });

                assert!(!status.success());
                let stderr = daemon.stderr();
                assert!(stderr.contains("another agentdeckd is running"), "{stderr}");
            }

            #[test]
            fn should_fail_when_the_other_daemon_never_exits() {
                let dirs = tempfile::tempdir().expect("a temp dir");
                let _lock = hold_lock(dirs.path());
                let mut daemon = waiting(dirs.path());

                let status = eventually_within(
                    "agentdeckd to stop waiting",
                    Duration::from_secs(20),
                    || daemon.try_wait(),
                );

                assert!(!status.success());
                let stderr = daemon.stderr();
                assert!(stderr.contains("hasn't exited after 10s"), "{stderr}");
            }
        }

        #[tokio::test]
        async fn should_let_a_daemon_start_after_the_last_one_stopped() {
            let daemon = TestDaemon::start().await;
            let (_, dirs) = daemon.terminate();

            let daemon = TestDaemon::start_in(dirs).await;

            daemon
                .client
                .health()
                .send()
                .await
                .expect("the new one serves");
        }
    }

    mod files {
        use super::*;

        #[tokio::test]
        async fn should_keep_the_database_under_xdg_data_home() {
            let daemon = TestDaemon::start().await;
            assert!(daemon.data_dir().join("agentdeck.db").is_file());
        }

        #[tokio::test]
        async fn should_log_under_xdg_state_home() {
            let daemon = TestDaemon::start().await;
            daemon.client.health().send().await.expect("health");

            let log = std::fs::read_to_string(daemon.state_dir().join("agentdeckd.log"))
                .expect("the log exists");

            assert!(log.contains("listening on 127.0.0.1"), "{log}");
        }

        #[tokio::test]
        async fn should_keep_data_across_a_restart() {
            let daemon = TestDaemon::start().await;
            let created = daemon
                .client
                .workspace_create()
                .body(agentdeck_client::types::NewWorkspace {
                    name: "kept".into(),
                    dir: None,
                })
                .send()
                .await
                .expect("create")
                .into_inner();
            let (_, dirs) = daemon.terminate();

            let daemon = TestDaemon::start_in(dirs).await;
            let workspaces = daemon
                .client
                .workspace_list()
                .send()
                .await
                .expect("list")
                .into_inner();

            assert_eq!(
                workspaces.iter().map(|w| w.id).collect::<Vec<_>>(),
                vec![created.id]
            );
        }
    }
}
