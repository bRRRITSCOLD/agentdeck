//! The daemon's lifecycle: the lock, `daemon.json`, its files and shutdown (spec §2).

use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use crate::support::{DaemonInfo, Spawned, TestDaemon, eventually_within, hold_lock};

/// A `daemon.json` as a killed daemon leaves it: its pid (this live process stands in) and a
/// port nothing listens on.
fn stale_daemon_json() -> String {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("binding a port")
        .local_addr()
        .unwrap()
        .port();
    serde_json::json!({
        "port": port,
        "token": "stale",
        "pid": std::process::id(),
        "version": "0.0.0",
        "api_version": agentdeck_core::API_VERSION,
    })
    .to_string()
}

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

            let started = Instant::now();
            let (status, stderr) = daemon.run_another();

            assert!(!status.success());
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "the second daemon waited {:?} before giving up",
                started.elapsed()
            );
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

            #[tokio::test]
            async fn should_give_up_waiting_once_the_other_daemon_serves() {
                let dirs = tempfile::tempdir().expect("a temp dir");
                let _lock = hold_lock(dirs.path());
                let mut daemon = waiting(dirs.path());
                // A daemon that serves, whose daemon.json the holder of the lock now writes.
                let serving = TestDaemon::start().await;

                std::fs::copy(
                    serving.runtime_dir().join("daemon.json"),
                    dirs.path().join("run/agentdeck/daemon.json"),
                )
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

        /// The lock held with a `daemon.json` that answers nothing: a daemon that was killed
        /// or crashed and hasn't finished exiting, so its file is left but its port is closed.
        mod held_by_a_daemon_whose_daemon_json_answers_nothing {
            use super::*;

            #[test]
            fn should_wait_for_the_lock_then_serve_in_its_place() {
                let dirs = tempfile::tempdir().expect("a temp dir");
                let lock = hold_lock(dirs.path());
                let daemon_json = dirs.path().join("run/agentdeck/daemon.json");
                std::fs::write(&daemon_json, stale_daemon_json()).expect("writing daemon.json");
                let mut daemon = Spawned::start(dirs.path());
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

                drop(lock);

                let info = eventually_within(
                    "agentdeckd to write its own daemon.json",
                    Duration::from_secs(10),
                    || {
                        assert_eq!(daemon.try_wait(), None, "{}", daemon.stderr());
                        let text = std::fs::read_to_string(&daemon_json).ok()?;
                        let info: DaemonInfo = serde_json::from_str(&text).ok()?;
                        (info.pid == daemon.pid()).then_some(info)
                    },
                );
                assert_ne!(info.port, 0);
            }

            #[test]
            fn should_fail_when_the_other_daemon_never_exits() {
                let dirs = tempfile::tempdir().expect("a temp dir");
                let _lock = hold_lock(dirs.path());
                std::fs::write(
                    dirs.path().join("run/agentdeck/daemon.json"),
                    stale_daemon_json(),
                )
                .expect("writing daemon.json");
                let mut daemon = Spawned::start(dirs.path());

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
                    link: Vec::new(),
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

mod test_daemon {
    use super::*;
    use crate::support::terminal::{Place, alive, printed, sh};

    #[tokio::test]
    async fn should_end_what_its_panes_left_running_once_dropped() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        // With job control on, as in an interactive shell, the job gets a process group of
        // its own. The PTY's hang-up ends the shell, which leads the session, but no SIGHUP
        // reaches a background group, so the job would outlive the daemon (issue #274).
        let pane = place
            .spawn(&daemon.client, &sh("set -m; sleep 60 & echo pid:$!; wait"))
            .await;
        let text = printed(&daemon.client, pane.id, "pid:").await;
        let digits = text[text.find("pid:").unwrap() + 4..]
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap();
        let sleeper: u32 = digits.parse().expect("a pid");
        assert!(alive(sleeper));

        drop(daemon);

        eventually_within("the pane's child to die", Duration::from_secs(5), || {
            (!alive(sleeper)).then_some(())
        });
    }
}
