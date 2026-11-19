//! `GET` and `PUT /v1/agents/mcp`: agentdeck's MCP server in Claude Code's and Codex's own
//! config (spec §4, §6, §11), against the stub `claude` and the daemon's temp `$HOME`.

use std::path::{Path, PathBuf};

use agentdeck_client::types::{McpRegistration, McpState};

use crate::support::{TestDaemon, cli_binary, set_daemon_env};

/// The variables `agentdeck mcp` reads, as the Codex table names them (spec §6).
const ENV_VARS: &str = r#"env_vars = ["XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "AGENTDECK_PANE", "AGENTDECK_WORKSPACE", "AGENTDECK_TASK"]"#;

/// The daemon's `$HOME`, read from its own environment, after checking it is under the test's
/// temp root: nothing here may reach the real config.
fn temp_home(daemon: &TestDaemon) -> PathBuf {
    let environ = std::fs::read(format!("/proc/{}/environ", daemon.info.pid))
        .expect("reading the daemon's environment");
    let home = environ
        .split(|&byte| byte == 0)
        .find_map(|var| var.strip_prefix(b"HOME="))
        .map(|home| PathBuf::from(String::from_utf8_lossy(home).into_owned()))
        .expect("the daemon has HOME");
    assert!(
        home.starts_with(daemon.root()) && daemon.root().starts_with(std::env::temp_dir()),
        "the daemon's HOME {} isn't under the test's temp root {}",
        home.display(),
        daemon.root().display()
    );
    home
}

async fn get(daemon: &TestDaemon) -> McpRegistration {
    temp_home(daemon);
    daemon
        .client
        .agents_mcp_get()
        .send()
        .await
        .expect("GET /v1/agents/mcp")
        .into_inner()
}

async fn install(daemon: &TestDaemon) -> McpRegistration {
    temp_home(daemon);
    daemon
        .client
        .agents_mcp_install()
        .send()
        .await
        .expect("PUT /v1/agents/mcp")
        .into_inner()
}

/// Every `claude mcp …` the stub ran, oldest first, its program name left out.
fn claude_calls(daemon: &TestDaemon) -> Vec<Vec<String>> {
    let bytes = std::fs::read(daemon.root().join("claude-mcp")).unwrap_or_default();
    String::from_utf8_lossy(&bytes)
        .split('\u{1e}')
        .filter(|call| !call.is_empty())
        .map(|call| {
            call.split('\0')
                .filter(|arg| !arg.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .collect()
}

fn add_call() -> Vec<String> {
    let cli = cli_binary();
    let cli = cli.to_str().unwrap();
    [
        "mcp",
        "add",
        "--scope",
        "user",
        "agentdeck",
        "--",
        cli,
        "mcp",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// A daemon whose `PATH` has no `program`: the stubs' dir without its stub, then only the
/// system dirs, so a real `claude` or `codex` further along the developer's `PATH` can't stand
/// in for the stub removed.
async fn start_without(program: &str) -> TestDaemon {
    const SYSTEM: &str = "/usr/bin:/bin";
    for dir in std::env::split_paths(SYSTEM) {
        assert!(
            !dir.join(program).exists(),
            "{program} is in {}: this test needs a PATH without it",
            dir.display()
        );
    }
    let dirs = tempfile::tempdir().expect("a temp dir");
    let path = format!("{}:{SYSTEM}", dirs.path().join("bin").display());
    set_daemon_env(dirs.path(), &[("PATH", &path)]);
    let daemon = TestDaemon::start_in(dirs).await;
    std::fs::remove_file(daemon.root().join("bin").join(program)).expect("removing the stub");
    daemon
}

fn codex_config(home: &Path) -> PathBuf {
    home.join(".codex/config.toml")
}

mod get {
    use super::*;

    #[tokio::test]
    async fn should_find_agentdeck_in_neither_on_a_fresh_home_200() {
        let daemon = TestDaemon::start().await;

        let registration = get(&daemon).await;

        assert_eq!(registration.command, cli_binary().to_str().unwrap());
        assert_eq!(registration.claude.state, McpState::Missing);
        assert_eq!(registration.claude.config, "~/.claude.json");
        assert_eq!(registration.codex.state, McpState::Missing);
        assert_eq!(registration.codex.config, "~/.codex/config.toml");
        assert!(claude_calls(&daemon).is_empty(), "a read runs no claude");
    }

    #[tokio::test]
    async fn should_say_an_agent_not_on_the_path_is_not_installed_200() {
        let daemon = start_without("codex").await;

        let registration = get(&daemon).await;

        assert_eq!(registration.codex.state, McpState::NotInstalled);
        assert_eq!(registration.claude.state, McpState::Missing);
    }

    #[tokio::test]
    async fn should_say_why_a_config_that_does_not_parse_is_unreadable_200() {
        let daemon = TestDaemon::start().await;
        let home = temp_home(&daemon);
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(codex_config(&home), "[[[").unwrap();

        let registration = get(&daemon).await;

        assert_eq!(registration.codex.state, McpState::Unreadable);
        let why = registration.codex.error.expect("why");
        assert!(why.starts_with("TOML parse error at line 1"), "{why}");
    }

    #[tokio::test]
    async fn should_follow_codex_home_and_claude_config_dir_200() {
        let dirs = tempfile::tempdir().expect("a temp dir");
        let codex_home = dirs.path().join("home/codex-home");
        let claude_dir = dirs.path().join("home/claude-dir");
        set_daemon_env(
            dirs.path(),
            &[
                ("CODEX_HOME", codex_home.to_str().unwrap()),
                ("CLAUDE_CONFIG_DIR", claude_dir.to_str().unwrap()),
            ],
        );
        let daemon = TestDaemon::start_in(dirs).await;
        std::fs::create_dir_all(&claude_dir).unwrap();

        let before = get(&daemon).await;
        let after = install(&daemon).await;

        assert_eq!(before.codex.config, "~/codex-home/config.toml");
        assert_eq!(before.claude.config, "~/claude-dir/.claude.json");
        assert_eq!(after.codex.state, McpState::Added);
        assert_eq!(after.claude.state, McpState::Added);
        assert!(codex_home.join("config.toml").is_file());
        assert!(claude_dir.join(".claude.json").is_file());
        assert!(!temp_home(&daemon).join(".codex").exists());
    }
}

mod install {
    use super::*;

    #[tokio::test]
    async fn should_add_agentdeck_to_claude_code_and_codex_200() {
        let daemon = TestDaemon::start().await;
        let home = temp_home(&daemon);

        let registration = install(&daemon).await;

        assert_eq!(registration.claude.state, McpState::Added);
        assert_eq!(registration.claude.error, None);
        assert_eq!(registration.codex.state, McpState::Added);
        assert_eq!(registration.codex.error, None);
        assert_eq!(claude_calls(&daemon), [add_call()]);
        let codex = std::fs::read_to_string(codex_config(&home)).expect("config.toml");
        let cli = cli_binary();
        assert_eq!(
            codex,
            format!(
                "[mcp_servers.agentdeck]\ncommand = \"{}\"\nargs = [\"mcp\"]\n{ENV_VARS}\n",
                cli.display()
            )
        );
    }

    #[tokio::test]
    async fn should_change_nothing_when_run_again_200() {
        let daemon = TestDaemon::start().await;
        let home = temp_home(&daemon);
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(codex_config(&home), "# mine\nmodel = \"o3\"\n").unwrap();
        install(&daemon).await;
        let codex = std::fs::read(codex_config(&home)).unwrap();
        let claude = std::fs::read(home.join(".claude.json")).unwrap();

        let again = install(&daemon).await;

        assert_eq!(again.claude.state, McpState::Added);
        assert_eq!(again.codex.state, McpState::Added);
        assert_eq!(claude_calls(&daemon), [add_call()], "claude ran once");
        assert_eq!(std::fs::read(codex_config(&home)).unwrap(), codex);
        assert_eq!(std::fs::read(home.join(".claude.json")).unwrap(), claude);
        assert!(
            String::from_utf8(codex)
                .unwrap()
                .starts_with("# mine\nmodel = \"o3\"\n\n[mcp_servers.agentdeck]\n")
        );
    }

    #[tokio::test]
    async fn should_replace_an_entry_that_runs_another_agentdeck_200() {
        let daemon = TestDaemon::start().await;
        let home = temp_home(&daemon);
        std::fs::write(
            home.join(".claude.json"),
            r#"{"mcpServers":{"agentdeck":{"command":"/old/agentdeck","args":["mcp"]}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(
            codex_config(&home),
            "[mcp_servers.agentdeck]\ncommand = \"/old/agentdeck\"\n\n[tui]\nnotifications = true\n",
        )
        .unwrap();
        let before = get(&daemon).await;

        let after = install(&daemon).await;

        assert_eq!(before.claude.state, McpState::Outdated);
        assert_eq!(before.codex.state, McpState::Outdated);
        assert_eq!(after.claude.state, McpState::Added);
        assert_eq!(after.codex.state, McpState::Added);
        let remove = ["mcp", "remove", "--scope", "user", "agentdeck"].map(str::to_owned);
        assert_eq!(claude_calls(&daemon), [remove.to_vec(), add_call()]);
        let codex = std::fs::read_to_string(codex_config(&home)).unwrap();
        assert!(!codex.contains("/old/agentdeck"), "{codex}");
        assert!(
            codex.ends_with("\n[tui]\nnotifications = true\n"),
            "{codex}"
        );
    }

    #[tokio::test]
    async fn should_skip_an_agent_that_is_not_installed_200() {
        let daemon = start_without("claude").await;
        let home = temp_home(&daemon);

        let registration = install(&daemon).await;

        assert_eq!(registration.claude.state, McpState::NotInstalled);
        assert_eq!(registration.claude.error, None);
        assert_eq!(registration.codex.state, McpState::Added);
        assert!(!home.join(".claude.json").exists());
    }

    #[tokio::test]
    async fn should_say_why_claude_failed_and_still_add_codex_200() {
        let daemon = TestDaemon::start().await;
        std::fs::write(daemon.root().join("claude-mcp-fails"), "not logged in").unwrap();

        let registration = install(&daemon).await;

        assert_eq!(registration.claude.state, McpState::Missing);
        let why = registration.claude.error.expect("why claude failed");
        assert!(why.contains("not logged in"), "{why}");
        assert!(
            why.starts_with("claude mcp add --scope user agentdeck"),
            "{why}"
        );
        assert_eq!(registration.codex.state, McpState::Added);
    }

    #[tokio::test]
    async fn should_leave_a_codex_config_that_does_not_parse_as_it_is_200() {
        let daemon = TestDaemon::start().await;
        let home = temp_home(&daemon);
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(codex_config(&home), "[[[").unwrap();

        let registration = install(&daemon).await;

        assert_eq!(registration.codex.state, McpState::Unreadable);
        assert!(registration.codex.error.is_some());
        assert_eq!(std::fs::read_to_string(codex_config(&home)).unwrap(), "[[[");
        assert_eq!(registration.claude.state, McpState::Added);
    }
}
