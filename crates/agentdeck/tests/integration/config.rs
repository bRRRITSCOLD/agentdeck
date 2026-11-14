//! `agentdeck config ls | get | set`: the settings in `config.toml`, through the daemon.

use crate::support::TestEnv;

fn config_toml(env: &TestEnv) -> String {
    std::fs::read_to_string(env.root().join("config/agentdeck/config.toml")).expect("config.toml")
}

mod config_ls {
    use super::*;

    #[test]
    fn should_list_every_setting_with_its_default() {
        let env = TestEnv::new();

        let stdout = env.ok(&["config", "ls"]);

        assert_eq!(
            stdout,
            "appearance.theme      system\npr_sync.max_age_days  90\nterminal.file_opener  xdg-open {path}\n"
        );
    }

    #[test]
    fn should_print_the_settings_object_with_json() {
        let env = TestEnv::new();

        let settings = env.json(&["config", "ls"]);

        assert_eq!(settings["appearance"]["theme"], "system");
        assert_eq!(settings["pr_sync"]["max_age_days"], 90);
        assert_eq!(settings["overridden_by_env"], serde_json::json!([]));
    }
}

mod config_get {
    use super::*;

    #[test]
    fn should_print_the_value_alone() {
        let env = TestEnv::new();

        assert_eq!(env.ok(&["config", "get", "pr_sync.max_age_days"]), "90\n");
        assert_eq!(env.ok(&["config", "get", "appearance.theme"]), "system\n");
    }

    #[test]
    fn should_print_the_key_and_value_with_json() {
        let env = TestEnv::new();

        let got = env.json(&["config", "get", "appearance.theme"]);

        assert_eq!(
            got,
            serde_json::json!({"key": "appearance.theme", "value": "system"})
        );
    }

    #[test]
    fn should_fail_for_a_key_that_is_not_a_setting() {
        let env = TestEnv::new();

        let stderr = env.fails(&["config", "get", "appearance.font"]);

        assert_eq!(
            stderr,
            "agentdeck: appearance.font isn't a setting; `agentdeck config ls` lists them\n"
        );
    }
}

mod config_set {
    use super::*;

    #[test]
    fn should_write_config_toml_and_apply_at_once() {
        let env = TestEnv::new();

        let stdout = env.ok(&["config", "set", "pr_sync.max_age_days", "30"]);

        assert_eq!(stdout, "pr_sync.max_age_days  30\n");
        assert_eq!(config_toml(&env), "[pr_sync]\nmax_age_days = 30\n");
        assert_eq!(env.json(&["pr", "status"])["max_age_days"], 30);
    }

    #[test]
    fn should_set_the_file_opener_as_one_value() {
        let env = TestEnv::new();

        let stdout = env.ok(&[
            "config",
            "set",
            "terminal.file_opener",
            "code -g {path}:{line}",
        ]);

        assert_eq!(stdout, "terminal.file_opener  code -g {path}:{line}\n");
        assert_eq!(
            config_toml(&env),
            "[terminal]\nfile_opener = \"code -g {path}:{line}\"\n"
        );
    }

    #[test]
    fn should_print_the_settings_object_with_json() {
        let env = TestEnv::new();

        let settings = env.json(&["config", "set", "appearance.theme", "dark"]);

        assert_eq!(settings["appearance"]["theme"], "dark");
        assert_eq!(env.ok(&["config", "get", "appearance.theme"]), "dark\n");
    }

    #[test]
    fn should_fail_with_the_daemons_reason_for_a_value_it_does_not_take() {
        let env = TestEnv::new();

        let stderr = env.fails(&["config", "set", "appearance.theme", "blue"]);

        assert_eq!(
            stderr,
            "agentdeck: appearance.theme must be system, dark or light, not \"blue\" (invalid_setting)\n"
        );
    }

    #[test]
    fn should_say_when_an_environment_variable_keeps_the_old_value() {
        let env = TestEnv::new();
        let days = std::ffi::OsString::from("7");

        let run = env.run_with(
            &["config", "set", "pr_sync.max_age_days", "30"],
            &[("AGENTDECK_PR_MAX_AGE_DAYS", &days)],
        );

        assert!(run.success, "{}", run.stderr);
        assert_eq!(
            run.stdout,
            "pr_sync.max_age_days  7  (an environment variable overrides config.toml)\n"
        );
        assert_eq!(config_toml(&env), "[pr_sync]\nmax_age_days = 30\n");
    }
}
