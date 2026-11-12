//! `agentdeck agent new | send` and `agentdeck tail`, against `sh` panes; never a real agent.

use crate::support::{TestEnv, eventually, shell_pane};

mod agent_send {
    use super::*;

    #[test]
    fn should_type_the_text_and_press_enter() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        let pane = shell_pane(&env, &ws, "read line; echo \"got:$line\"; sleep 60");

        let sent = env.json(&["agent", "send", &pane, "hello", "--now"]);

        assert_eq!(sent["queued"], false);
        eventually("the pane to echo the line", || {
            env.ok(&["tail", &pane]).contains("got:hello").then_some(())
        });
    }
}

mod tail {
    use super::*;

    #[test]
    fn should_print_only_the_last_bytes_asked_for() {
        let env = TestEnv::new();
        let ws = env.workspace("web");
        let pane = shell_pane(&env, &ws, "printf 'abcdef-done'; sleep 60");
        eventually("the pane's output", || {
            env.ok(&["tail", &pane])
                .contains("abcdef-done")
                .then_some(())
        });

        let stdout = env.ok(&["tail", &pane, "--bytes", "4"]);

        assert_eq!(stdout, "done");
    }

    #[test]
    fn should_fail_for_an_unknown_pane() {
        let env = TestEnv::new();

        let stderr = env.fails(&["tail", "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b"]);

        assert!(stderr.contains("not_found"), "{stderr}");
    }
}

mod agent_new {
    use super::*;

    #[test]
    fn should_say_it_isnt_available_yet() {
        let env = TestEnv::new();
        env.workspace("web");

        let stderr = env.fails(&["agent", "new", "--ws", "web"]);

        assert!(stderr.contains("isn't available yet"), "{stderr}");
    }
}
