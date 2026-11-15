//! Reaching the daemon (spec §2): `daemon.json`, and starting `agentdeckd` when nothing
//! answers there.

use std::ffi::OsString;
use std::net::TcpListener;
use std::process::Command;

use agentdeck_core::DaemonInfo;

use crate::support::{TestEnv, agent_argv, alive, eventually, text};

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
