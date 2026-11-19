//! `agentdeck mcp install`: agentdeck's MCP server added to Claude Code's and Codex's own
//! config in the test's temp `$HOME`, through the stub `claude` (spec §10).

use std::path::PathBuf;

use crate::support::TestEnv;
use crate::support::pty::run_answering;

/// The test's `$HOME`, after checking it is under its temp root: nothing here may reach the
/// real config.
fn home(env: &TestEnv) -> PathBuf {
    let home = env.root().join("home");
    assert!(
        env.root().starts_with(std::env::temp_dir()),
        "{} isn't a temp dir",
        env.root().display()
    );
    home
}

/// How many `claude mcp …` calls the stub ran.
fn claude_calls(env: &TestEnv) -> usize {
    std::fs::read(env.root().join("claude-mcp"))
        .unwrap_or_default()
        .iter()
        .filter(|&&byte| byte == 0x1e)
        .count()
}

fn codex_config(env: &TestEnv) -> String {
    std::fs::read_to_string(home(env).join(".codex/config.toml")).unwrap_or_default()
}

mod agentdeck_mcp_install {
    use super::*;

    #[test]
    fn should_add_agentdeck_to_both_with_yes_and_say_so() {
        let env = TestEnv::new();
        home(&env);

        let stdout = env.ok(&["mcp", "install", "--yes"]);

        assert_eq!(
            stdout,
            "Claude Code: added agentdeck (~/.claude.json)\nCodex: added agentdeck (~/.codex/config.toml)\n"
        );
        assert_eq!(claude_calls(&env), 1);
        assert!(codex_config(&env).starts_with("[mcp_servers.agentdeck]\n"));
    }

    #[test]
    fn should_change_nothing_when_run_again() {
        let env = TestEnv::new();
        home(&env);
        env.ok(&["mcp", "install", "--yes"]);
        let codex = codex_config(&env);

        let stdout = env.ok(&["mcp", "install"]);

        assert_eq!(stdout, "Claude Code: already added\nCodex: already added\n");
        assert_eq!(claude_calls(&env), 1);
        assert_eq!(codex_config(&env), codex);
    }

    #[test]
    fn should_print_the_registration_with_json() {
        let env = TestEnv::new();
        home(&env);

        let registration = env.json(&["mcp", "install", "--yes"]);

        assert_eq!(registration["claude"]["state"], "added");
        assert_eq!(registration["codex"]["state"], "added");
        assert_eq!(registration["codex"]["config"], "~/.codex/config.toml");
    }

    #[test]
    fn should_fail_naming_yes_without_a_terminal_and_change_nothing() {
        let env = TestEnv::new();
        home(&env);

        let stderr = env.fails(&["mcp", "install"]);

        assert!(stderr.contains("pass --yes"), "{stderr}");
        assert_eq!(claude_calls(&env), 0);
        assert_eq!(codex_config(&env), "");
    }

    #[test]
    fn should_show_the_changes_and_add_on_y() {
        let env = TestEnv::new();
        home(&env);

        let run = run_answering(&env, &["mcp", "install"], "[y/N] ", "y");

        assert!(run.success, "{}", run.output);
        assert!(
            run.output
                .contains("Claude Code: run claude mcp add --scope user agentdeck -- "),
            "{}",
            run.output
        );
        assert!(
            run.output
                .contains("Codex: add [mcp_servers.agentdeck] to ~/.codex/config.toml"),
            "{}",
            run.output
        );
        assert!(
            run.output.contains("Codex: added agentdeck"),
            "{}",
            run.output
        );
        assert_eq!(claude_calls(&env), 1);
    }

    #[test]
    fn should_change_nothing_on_any_answer_but_y() {
        let env = TestEnv::new();
        home(&env);

        let run = run_answering(&env, &["mcp", "install"], "[y/N] ", "yes please");

        assert!(run.success, "{}", run.output);
        assert!(run.output.contains("Changed nothing"), "{}", run.output);
        assert_eq!(claude_calls(&env), 0);
        assert_eq!(codex_config(&env), "");
    }
}
