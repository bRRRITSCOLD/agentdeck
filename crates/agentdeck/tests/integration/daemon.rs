//! Reaching the daemon (spec §2): `daemon.json`, and starting `agentdeckd` when nothing
//! answers there.

use std::ffi::OsString;
use std::net::TcpListener;
use std::process::{Command, Stdio};

use agentdeck_core::DaemonInfo;

use serde_json::json;

use crate::support::mcp::McpClient;
use crate::support::{
    Run, TestEnv, agent_argv, alive, eventually, parent_pid, process_state, shell_pane, text,
};

/// A port nothing listens on: bound, then released.
fn closed_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("binding a port")
        .local_addr()
        .unwrap()
        .port()
}

fn stale_info(pid: u32) -> DaemonInfo {
    DaemonInfo {
        port: closed_port(),
        token: "stale".into(),
        pid,
        version: "0.0.0".into(),
        api_version: agentdeck_core::API_VERSION,
    }
}

mod starting_the_daemon {
    use super::*;

    #[test]
    fn should_start_one_when_none_is_running() {
        let env = TestEnv::new();
        assert!(env.daemon_info().is_none());

        env.ok(&["ws", "ls"]);

        let info = env.daemon_info().expect("daemon.json after the command");
        assert!(alive(info.pid), "the daemon it started isn't running");
    }

    #[test]
    fn should_reuse_the_running_daemon() {
        let env = TestEnv::new();
        env.ok(&["ws", "ls"]);
        let first = env.daemon_info().unwrap();

        env.ok(&["task", "ls"]);

        assert_eq!(env.daemon_info().unwrap(), first);
    }

    #[test]
    fn should_replace_a_daemon_json_whose_process_is_gone() {
        let env = TestEnv::new();
        let mut exited = Command::new("true").spawn().expect("running true");
        let dead_pid = exited.id();
        exited.wait().unwrap();
        env.write_daemon_info(&stale_info(dead_pid));

        env.ok(&["ws", "ls"]);

        let info = env.daemon_info().unwrap();
        assert_ne!(info.pid, dead_pid);
        assert!(alive(info.pid));
    }

    #[test]
    fn should_replace_a_daemon_json_whose_port_refuses_connections() {
        let env = TestEnv::new();
        // A live process that isn't a daemon, as after a pid is reused.
        let mut impostor = Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("running sleep");
        env.write_daemon_info(&stale_info(impostor.id()));

        let run = env.run(&["ws", "ls"]);
        let info = env.daemon_info();
        let _ = impostor.kill();
        let _ = impostor.wait();

        assert!(run.success, "{}", run.stderr);
        let info = info.unwrap();
        assert_ne!(info.pid, impostor.id());
        assert!(alive(info.pid));
    }

    #[test]
    fn should_fail_when_the_daemon_exits_before_serving() {
        let env = TestEnv::new();
        let false_bin = OsString::from("/bin/false");

        let run = env.run_with(&["ws", "ls"], &[("AGENTDECK_DAEMON", &false_bin)]);

        assert!(!run.success);
        assert!(run.stderr.contains("exited"), "{}", run.stderr);
    }

    #[test]
    fn should_fail_without_a_runtime_dir() {
        let env = TestEnv::new();

        let output = env
            .command()
            .env_remove("XDG_RUNTIME_DIR")
            .args(["ws", "ls"])
            .output()
            .expect("running agentdeck");

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("XDG_RUNTIME_DIR"), "{stderr}");
    }
}

/// A client that connects while a daemon is shutting down: it has withdrawn `daemon.json`
/// but still holds the lock until it exits, as after `pkill -TERM agentdeckd`.
mod connecting_while_a_daemon_shuts_down {
    use super::*;

    #[test]
    fn should_start_a_daemon_once_the_old_one_lets_go_of_the_lock() {
        let env = TestEnv::new();
        let lock = env.hold_lock();
        let mut cli = env
            .command()
            .args(["ws", "ls"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("running agentdeck");

        // The started daemon waits for the lock; one that gave up on it at once has made
        // the CLI give up too.
        eventually("the started daemon to wait for the lock", || {
            let waiting = env.daemon_log().contains("waiting for the agentdeckd");
            let gave_up = cli.try_wait().expect("polling agentdeck").is_some();
            (waiting || gave_up).then_some(())
        });
        drop(lock);
        let run = Run::from(cli.wait_with_output().expect("waiting on agentdeck"));

        assert!(run.success, "{}", run.stderr);
        let info = env.daemon_info().expect("daemon.json after the command");
        assert!(alive(info.pid), "the daemon it started isn't running");
    }

    #[test]
    fn should_start_a_fresh_daemon_while_the_stopped_one_still_exits() {
        let env = TestEnv::new();
        let workspace = env.workspace("web");
        // A live pane holds the database actor, so the stopped daemon's runtime waits its
        // whole shutdown grace before the daemon lets go of the lock.
        shell_pane(&env, &workspace, "sleep 60");
        let old = env.daemon_info().expect("a running daemon");
        let status = Command::new("kill")
            .args(["-TERM", &old.pid.to_string()])
            .status()
            .expect("running kill");
        assert!(status.success(), "kill -TERM {} failed", old.pid);

        eventually("the stopped daemon to withdraw daemon.json", || {
            env.daemon_info().is_none().then_some(())
        });
        assert!(
            matches!(process_state(old.pid), Some(state) if state != 'Z'),
            "the stopped daemon withdrew daemon.json only as it exited"
        );
        let run = env.run(&["ws", "ls"]);

        assert!(run.success, "{}", run.stderr);
        let new = env.daemon_info().expect("daemon.json after the command");
        assert_ne!(new.pid, old.pid);
        assert!(alive(new.pid), "the fresh daemon isn't running");
    }
}

/// Rewrites the running daemon's `daemon.json` without `api_version`, as a daemon from before
/// it existed wrote it.
fn make_older(env: &TestEnv) -> DaemonInfo {
    let info = env.daemon_info().expect("a running daemon");
    let mut json = serde_json::to_value(&info).unwrap();
    json.as_object_mut().unwrap().remove("api_version");
    std::fs::write(env.daemon_json(), json.to_string()).expect("writing daemon.json");
    info
}

mod older_daemon {
    use super::*;

    #[test]
    fn should_warn_in_one_line_on_stderr_and_still_run_the_command() {
        let env = TestEnv::new();
        env.ok(&["ws", "ls"]);
        make_older(&env);

        let run = env.run(&["ws", "ls"]);

        assert!(run.success, "{}", run.stderr);
        assert_eq!(run.stderr.lines().count(), 1, "{}", run.stderr);
        assert!(run.stderr.contains("older than this CLI"), "{}", run.stderr);
        assert!(
            run.stderr.contains("agentdeck daemon restart"),
            "{}",
            run.stderr
        );
    }

    #[test]
    fn should_not_warn_about_a_daemon_as_new_as_the_cli() {
        let env = TestEnv::new();
        env.ok(&["ws", "ls"]);

        let run = env.run(&["ws", "ls"]);

        assert!(run.success, "{}", run.stderr);
        assert_eq!(run.stderr, "");
    }
}

mod daemon_restart {
    use super::*;

    #[test]
    fn should_stop_the_daemon_and_start_a_new_one_on_the_same_data() {
        let env = TestEnv::new();
        env.workspace("web");
        let old = make_older(&env);

        let run = env.run(&["daemon", "restart"]);

        assert!(run.success, "{}", run.stderr);
        assert_eq!(
            run.stderr, "",
            "restart warned about the daemon it replaces"
        );
        assert!(!alive(old.pid), "the old daemon is still running");
        let new = env.daemon_info().expect("daemon.json after the restart");
        assert_ne!(new.pid, old.pid);
        assert_eq!(new.api_version, agentdeck_core::API_VERSION);
        assert!(run.stdout.contains(&new.pid.to_string()), "{}", run.stdout);
        assert!(env.ok(&["ws", "ls"]).contains("web"));
    }

    #[test]
    fn should_resume_the_agents_in_the_new_daemon() {
        let env = TestEnv::new();
        env.workspace("web");
        let pane = env.json(&["agent", "new", "--ws", "web"]);
        let (id, session) = (text(&pane, "id"), text(&pane, "session_id"));
        eventually("the stub agent to start", || agent_argv(&env, id));

        env.ok(&["daemon", "restart"]);

        eventually("the agent to resume", || {
            agent_argv(&env, id).filter(|argv| argv == &["--resume", session])
        });
    }

    #[test]
    fn should_start_a_daemon_when_none_is_running() {
        let env = TestEnv::new();

        let restarted = env.json(&["daemon", "restart"]);

        let info = env.daemon_info().expect("daemon.json after the restart");
        assert!(alive(info.pid));
        assert_eq!(restarted["pid"], info.pid);
    }
}

/// A client that outlives the daemon it started, as the GUI does: `agentdeck mcp` connects on
/// every tool call, so the daemon its first call starts is its child for as long as it runs.
mod daemon_started_by_a_long_lived_client {
    use super::*;

    /// Starts `agentdeck mcp` and has it start the daemon; returns the client and that daemon.
    fn started(env: &TestEnv) -> (McpClient, DaemonInfo) {
        let mut mcp = McpClient::start(env, &[]);
        mcp.initialize();
        mcp.call_ok("prs_status", json!({}));
        let info = env.daemon_info().expect("daemon.json after the tool call");
        assert_eq!(
            parent_pid(info.pid),
            Some(mcp.pid()),
            "the daemon isn't the client's child, so the test proves nothing"
        );
        (mcp, info)
    }

    /// SIGKILL to a daemon this test's client started, as a crash would end it.
    fn kill(pid: u32) {
        let status = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
            .expect("running kill");
        assert!(status.success(), "kill -KILL {pid} failed");
    }

    #[test]
    fn should_reap_it_once_it_exits() {
        let env = TestEnv::new();
        let (_mcp, info) = started(&env);

        kill(info.pid);

        // A zombie (`Z`) still has its `/proc` entry; only a reaped process loses it.
        eventually("the killed daemon to be reaped", || {
            process_state(info.pid).is_none().then_some(())
        });
    }

    #[test]
    fn should_start_a_fresh_daemon_on_the_next_connect() {
        let env = TestEnv::new();
        let (mut mcp, old) = started(&env);
        kill(old.pid);
        // Exited, reaped or not: until then its lock may still be held, and the daemon the
        // next call starts would lose the race for it.
        eventually("the killed daemon to exit", || {
            matches!(process_state(old.pid), None | Some('Z')).then_some(())
        });

        mcp.call_ok("prs_status", json!({}));

        let new = env.daemon_info().expect("daemon.json after the next call");
        assert_ne!(new.pid, old.pid);
        assert!(alive(new.pid), "the fresh daemon isn't running");
    }
}
