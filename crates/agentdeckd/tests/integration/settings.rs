//! The settings in `config.toml`: `/v1/settings` (spec §2, §4), and PR sync reading the
//! cutoff they set without a restart (spec §8).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agentdeck_client::types::{
    AppearancePatch, GuiPatch, PrSyncPatch, Settings, SettingsPatch, TerminalPatch, ThemePreference,
};
use agentdeck_core::ws::{Entity, ServerMessage};
use httpmock::MockServer;
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::support::github::{
    Pr, days_ago, mock_fetch, mock_list, mock_updated_search, set_gh_token,
};
use crate::support::ws::{Frame, Ws};
use crate::support::{TestDaemon, error_response, set_daemon_env};

const TOKEN: &str = "gho_test";

/// `$XDG_CONFIG_HOME/agentdeck/config.toml` under `root`.
fn config_file(root: &Path) -> PathBuf {
    root.join("config/agentdeck/config.toml")
}

/// Temp dirs whose daemon reads `config.toml` alone: the harness's no-cutoff variable is
/// emptied, which is the same as unset.
fn dirs_without_env() -> TempDir {
    let dirs = tempfile::tempdir().expect("a temp dir");
    set_daemon_env(dirs.path(), &[("AGENTDECK_PR_MAX_AGE_DAYS", "")]);
    dirs
}

fn write_config(root: &Path, text: &str) {
    let path = config_file(root);
    std::fs::create_dir_all(path.parent().unwrap()).expect("creating the config dir");
    std::fs::write(path, text).expect("writing config.toml");
}

async fn get(daemon: &TestDaemon) -> Settings {
    let response = daemon.client.settings_get().send().await.expect("settings");
    assert_eq!(response.status().as_u16(), 200);
    response.into_inner()
}

fn theme(theme: ThemePreference) -> SettingsPatch {
    SettingsPatch {
        appearance: Some(AppearancePatch { theme: Some(theme) }),
        ..SettingsPatch::default()
    }
}

fn max_age(days: u32) -> SettingsPatch {
    SettingsPatch {
        pr_sync: Some(PrSyncPatch {
            max_age_days: Some(days),
        }),
        ..SettingsPatch::default()
    }
}

fn file_opener(command: &str) -> SettingsPatch {
    SettingsPatch {
        terminal: Some(TerminalPatch {
            file_opener: Some(command.into()),
        }),
        ..SettingsPatch::default()
    }
}

fn confirm_quit(confirm: bool) -> SettingsPatch {
    SettingsPatch {
        gui: Some(GuiPatch {
            confirm_quit: Some(confirm),
        }),
        ..SettingsPatch::default()
    }
}

async fn patch(daemon: &TestDaemon, body: SettingsPatch) -> Settings {
    let response = daemon
        .client
        .settings_update()
        .body(body)
        .send()
        .await
        .expect("update the settings");
    assert_eq!(response.status().as_u16(), 200);
    response.into_inner()
}

/// `PATCH /v1/settings` with a raw JSON body, for values the typed client can't send.
async fn patch_raw(daemon: &TestDaemon, body: Value) -> (u16, Value) {
    let response = reqwest::Client::new()
        .patch(format!("{}/v1/settings", daemon.base_url()))
        .bearer_auth(&daemon.info.token)
        .json(&body)
        .send()
        .await
        .expect("a response");
    let status = response.status().as_u16();
    (status, response.json().await.expect("a JSON body"))
}

mod get_v1_settings {
    use super::*;

    #[tokio::test]
    async fn should_report_the_defaults_without_a_config_file_200() {
        let daemon = TestDaemon::start_in(dirs_without_env()).await;

        let settings = get(&daemon).await;

        assert_eq!(settings.appearance.theme, ThemePreference::System);
        assert_eq!(settings.pr_sync.max_age_days, 90);
        assert_eq!(settings.terminal.file_opener, "xdg-open {path}");
        assert!(settings.gui.confirm_quit);
        assert!(settings.overridden_by_env.is_empty());
    }

    #[tokio::test]
    async fn should_report_what_config_toml_says_200() {
        let dirs = dirs_without_env();
        write_config(
            dirs.path(),
            "[appearance]\ntheme = \"light\"\n[pr_sync]\nmax_age_days = 14\n[terminal]\nfile_opener = \"code -g {path}:{line}\"\n[gui]\nconfirm_quit = false\n",
        );
        let daemon = TestDaemon::start_in(dirs).await;

        let settings = get(&daemon).await;

        assert_eq!(settings.appearance.theme, ThemePreference::Light);
        assert_eq!(settings.pr_sync.max_age_days, 14);
        assert_eq!(settings.terminal.file_opener, "code -g {path}:{line}");
        assert!(!settings.gui.confirm_quit);
    }

    #[tokio::test]
    async fn should_report_the_env_value_and_say_it_overrides_the_file_200() {
        let dirs = tempfile::tempdir().expect("a temp dir");
        set_daemon_env(dirs.path(), &[("AGENTDECK_PR_MAX_AGE_DAYS", "7")]);
        write_config(dirs.path(), "[pr_sync]\nmax_age_days = 14\n");
        let daemon = TestDaemon::start_in(dirs).await;

        let settings = get(&daemon).await;

        assert_eq!(settings.pr_sync.max_age_days, 7);
        assert_eq!(settings.overridden_by_env, ["pr_sync.max_age_days"]);
    }
}

mod patch_v1_settings {
    use super::*;

    #[tokio::test]
    async fn should_answer_with_every_setting_after_the_change_200() {
        let daemon = TestDaemon::start_in(dirs_without_env()).await;

        let settings = patch(&daemon, theme(ThemePreference::Dark)).await;

        assert_eq!(settings.appearance.theme, ThemePreference::Dark);
        assert_eq!(settings.pr_sync.max_age_days, 90);
        assert_eq!(get(&daemon).await, settings);
    }

    #[tokio::test]
    async fn should_create_config_toml_when_there_is_none_200() {
        let daemon = TestDaemon::start_in(dirs_without_env()).await;

        patch(&daemon, max_age(30)).await;

        let text = std::fs::read_to_string(config_file(daemon.root())).expect("config.toml");
        assert_eq!(text, "[pr_sync]\nmax_age_days = 30\n");
    }

    #[tokio::test]
    async fn should_keep_unknown_keys_and_comments_in_config_toml_200() {
        let dirs = dirs_without_env();
        let before = "# my settings\n[pr_sync]\nmax_age_days = 90 # days\nlater = true\n\n[terminal]\nfont = \"x\"\n";
        write_config(dirs.path(), before);
        let daemon = TestDaemon::start_in(dirs).await;

        patch(&daemon, theme(ThemePreference::Light)).await;
        patch(&daemon, max_age(0)).await;

        let text = std::fs::read_to_string(config_file(daemon.root())).expect("config.toml");
        assert_eq!(
            text,
            format!(
                "{}\n[appearance]\ntheme = \"light\"\n",
                before.replace("= 90", "= 0")
            )
        );
    }

    #[tokio::test]
    async fn should_keep_the_settings_across_a_restart_200() {
        let daemon = TestDaemon::start_in(dirs_without_env()).await;
        patch(&daemon, theme(ThemePreference::Dark)).await;
        let (_, dirs) = daemon.terminate();

        let daemon = TestDaemon::start_in(dirs).await;

        assert_eq!(get(&daemon).await.appearance.theme, ThemePreference::Dark);
    }

    #[tokio::test]
    async fn should_write_an_overridden_key_and_say_it_stays_overridden_200() {
        let daemon = TestDaemon::start().await;

        let settings = patch(&daemon, max_age(30)).await;

        assert_eq!(settings.pr_sync.max_age_days, 0);
        assert_eq!(settings.overridden_by_env, ["pr_sync.max_age_days"]);
        let text = std::fs::read_to_string(config_file(daemon.root())).expect("config.toml");
        assert_eq!(text, "[pr_sync]\nmax_age_days = 30\n");
    }

    #[tokio::test]
    async fn should_tell_websocket_clients_the_settings_changed_200() {
        let daemon = TestDaemon::start_in(dirs_without_env()).await;
        let mut ws = Ws::connect(&daemon).await;

        patch(&daemon, theme(ThemePreference::Dark)).await;

        ws.expect(ServerMessage::Changed {
            entity: Entity::Settings,
            id: "settings".into(),
        })
        .await;
    }

    #[tokio::test]
    async fn should_tell_websocket_clients_the_sync_status_changed_with_the_cutoff_200() {
        let daemon = TestDaemon::start_in(dirs_without_env()).await;
        let mut ws = Ws::connect(&daemon).await;

        patch(&daemon, max_age(30)).await;

        ws.until(|frame| match frame {
            Frame::Message(ServerMessage::Changed {
                entity: Entity::PrSync,
                ..
            }) => Some(()),
            _ => None,
        })
        .await;
        let status = daemon.client.prs_status().send().await.expect("status");
        assert_eq!(status.into_inner().max_age_days, 30);
    }

    #[tokio::test]
    async fn should_reject_a_theme_it_does_not_know_and_name_it_400() {
        let daemon = TestDaemon::start_in(dirs_without_env()).await;

        let (status, body) = patch_raw(&daemon, json!({"appearance": {"theme": "blue"}})).await;

        assert_eq!(status, 400);
        assert_eq!(body["error_code"], "invalid_setting");
        let message = body["message"].as_str().expect("a message");
        assert!(message.starts_with("appearance.theme "), "{message}");
        assert!(!config_file(daemon.root()).exists());
    }

    #[tokio::test]
    async fn should_reject_a_cutoff_that_is_not_a_count_of_days_400() {
        let daemon = TestDaemon::start_in(dirs_without_env()).await;

        let (status, body) = patch_raw(&daemon, json!({"pr_sync": {"max_age_days": -1}})).await;

        assert_eq!(status, 400);
        assert_eq!(body["error_code"], "invalid_setting");
        let message = body["message"].as_str().expect("a message");
        assert!(message.starts_with("pr_sync.max_age_days "), "{message}");
        assert_eq!(get(&daemon).await.pr_sync.max_age_days, 90);
    }

    #[tokio::test]
    async fn should_set_the_file_opener_200() {
        let daemon = TestDaemon::start_in(dirs_without_env()).await;

        let settings = patch(&daemon, file_opener("code -g '{path}:{line}'")).await;

        assert_eq!(settings.terminal.file_opener, "code -g '{path}:{line}'");
        let text = std::fs::read_to_string(config_file(daemon.root())).expect("config.toml");
        assert_eq!(
            text,
            "[terminal]\nfile_opener = \"code -g '{path}:{line}'\"\n"
        );
    }

    #[tokio::test]
    async fn should_set_confirm_quit_and_back_200() {
        let daemon = TestDaemon::start_in(dirs_without_env()).await;

        let off = patch(&daemon, confirm_quit(false)).await;
        let text = std::fs::read_to_string(config_file(daemon.root())).expect("config.toml");
        let on = patch(&daemon, confirm_quit(true)).await;

        assert!(!off.gui.confirm_quit);
        assert_eq!(text, "[gui]\nconfirm_quit = false\n");
        assert!(on.gui.confirm_quit);
        assert!(get(&daemon).await.gui.confirm_quit);
    }

    #[tokio::test]
    async fn should_reject_a_confirm_quit_that_is_not_a_bool_and_name_it_400() {
        let daemon = TestDaemon::start_in(dirs_without_env()).await;

        let (status, body) = patch_raw(&daemon, json!({"gui": {"confirm_quit": "no"}})).await;

        assert_eq!(status, 400);
        assert_eq!(body["error_code"], "invalid_setting");
        let message = body["message"].as_str().expect("a message");
        assert!(message.starts_with("gui.confirm_quit "), "{message}");
        assert!(get(&daemon).await.gui.confirm_quit);
    }

    #[tokio::test]
    async fn should_reject_a_file_opener_that_is_not_one_and_name_it_400() {
        let daemon = TestDaemon::start_in(dirs_without_env()).await;

        for opener in [json!(""), json!("code -g"), json!("code '{path}"), json!(1)] {
            let (status, body) =
                patch_raw(&daemon, json!({"terminal": {"file_opener": opener}})).await;

            assert_eq!(status, 400, "{opener}");
            assert_eq!(body["error_code"], "invalid_setting");
            let message = body["message"].as_str().expect("a message");
            assert!(message.starts_with("terminal.file_opener "), "{message}");
        }
        assert_eq!(get(&daemon).await.terminal.file_opener, "xdg-open {path}");
        assert!(!config_file(daemon.root()).exists());
    }

    #[tokio::test]
    async fn should_reject_a_key_that_is_not_a_setting_400() {
        let daemon = TestDaemon::start_in(dirs_without_env()).await;

        let (status, body) = patch_raw(&daemon, json!({"appearance": {"font": "x"}})).await;

        assert_eq!(status, 400);
        assert_eq!(body["error_code"], "invalid_setting");
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.starts_with("appearance.font ")),
            "{body}"
        );
    }

    #[tokio::test]
    async fn should_leave_a_config_toml_that_does_not_parse_as_it_is_409() {
        let dirs = dirs_without_env();
        write_config(dirs.path(), "[pr_sync\n");
        let daemon = TestDaemon::start_in(dirs).await;

        let (status, body) = error_response(
            daemon
                .client
                .settings_update()
                .body(max_age(30))
                .send()
                .await,
        );

        assert_eq!(status, 409);
        assert_eq!(body.error_code.as_deref(), Some("unparsable_config"));
        let text = std::fs::read_to_string(config_file(daemon.root())).expect("config.toml");
        assert_eq!(text, "[pr_sync\n");
    }
}

mod pr_sync_cutoff {
    use super::*;

    /// Open PR `number`, last updated on GitHub `days` ago.
    fn aged(number: i64, days: i64) -> Pr {
        Pr {
            updated_at: days_ago(days),
            ..Pr::open(number)
        }
    }

    #[tokio::test]
    async fn should_apply_a_new_cutoff_to_the_next_sync_without_a_restart_200() {
        let github = MockServer::start_async().await;
        mock_list(&github, TOKEN, &[aged(7, 1), aged(8, 40)]);
        let dirs = tempfile::tempdir().expect("a temp dir");
        set_daemon_env(
            dirs.path(),
            &[
                ("AGENTDECK_PR_MAX_AGE_DAYS", ""),
                ("AGENTDECK_PR_FULL_SYNC_SECS", "0"),
            ],
        );
        write_config(dirs.path(), "[pr_sync]\nmax_age_days = 0\n");
        // Logged out until the cutoff is changed, so the sync at start stores nothing.
        let daemon = TestDaemon::start_with_github(dirs, &github.url("/graphql")).await;
        assert_eq!(get(&daemon).await.pr_sync.max_age_days, 0);

        patch(&daemon, max_age(30)).await;
        set_gh_token(daemon.root(), Some(TOKEN));
        let status = daemon.client.prs_sync().send().await.expect("sync");

        assert_eq!(status.into_inner().max_age_days, 30);
        let numbers: Vec<i64> = daemon
            .client
            .pr_list()
            .finished(true)
            .send()
            .await
            .expect("list PRs")
            .into_inner()
            .into_iter()
            .map(|pr| i64::from(pr.number))
            .collect();
        assert_eq!(numbers, [7]);
    }

    /// The numbers of the stored PRs, sorted.
    async fn numbers(daemon: &TestDaemon) -> Vec<i64> {
        let mut numbers: Vec<i64> = daemon
            .client
            .pr_list()
            .finished(true)
            .send()
            .await
            .expect("list PRs")
            .into_inner()
            .into_iter()
            .map(|pr| i64::from(pr.number))
            .collect();
        numbers.sort_unstable();
        numbers
    }

    /// Waits up to 10 s, with no sync asked for, until #7 and #8 are both stored.
    async fn until_both_synced(daemon: &TestDaemon) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while numbers(daemon).await != [7, 8] {
            assert!(Instant::now() < deadline, "#8 never synced");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// A signed-in daemon reading `max_age_days` from `config.toml`, with a full sync an hour
    /// apart, past its first sync. GitHub lists #7 (updated a day ago) and #8 (40 days ago);
    /// the incremental search shows nothing, so only a full sync can add #8.
    async fn synced(github: &MockServer, max_age_days: u32) -> TestDaemon {
        let dirs = dirs_without_env();
        write_config(
            dirs.path(),
            &format!("[pr_sync]\nmax_age_days = {max_age_days}\n"),
        );
        set_gh_token(dirs.path(), Some(TOKEN));
        let daemon = TestDaemon::start_with_github(dirs, &github.url("/graphql")).await;
        daemon.client.prs_sync().send().await.expect("sync");
        daemon
    }

    #[tokio::test]
    async fn should_start_a_full_sync_at_once_when_the_cutoff_widens_200() {
        let github = MockServer::start_async().await;
        let listing = mock_list(&github, TOKEN, &[aged(7, 1), aged(8, 40)]);
        mock_fetch(&github, &[aged(8, 40)]);
        mock_updated_search(&github, TOKEN, &[]);
        let daemon = synced(&github, 30).await;
        assert_eq!(numbers(&daemon).await, [7]);

        patch(&daemon, max_age(365)).await;

        until_both_synced(&daemon).await;
        listing.list.assert_calls(2);
    }

    #[tokio::test]
    async fn should_start_a_full_sync_at_once_when_the_cutoff_is_lifted_200() {
        let github = MockServer::start_async().await;
        let listing = mock_list(&github, TOKEN, &[aged(7, 1), aged(8, 40)]);
        mock_fetch(&github, &[aged(8, 40)]);
        mock_updated_search(&github, TOKEN, &[]);
        let daemon = synced(&github, 30).await;

        patch(&daemon, max_age(0)).await;

        until_both_synced(&daemon).await;
        listing.list.assert_calls(2);
    }

    #[tokio::test]
    async fn should_not_start_a_full_sync_when_the_cutoff_narrows_200() {
        let github = MockServer::start_async().await;
        let listing = mock_list(&github, TOKEN, &[aged(7, 1), aged(8, 40)]);
        let updated = mock_updated_search(&github, TOKEN, &[]);
        let daemon = synced(&github, 365).await;
        assert_eq!(numbers(&daemon).await, [7, 8]);

        patch(&daemon, max_age(30)).await;
        // Queues behind any sync the PATCH started.
        daemon.client.prs_sync().send().await.expect("sync");

        listing.list.assert_calls(1);
        assert!(updated.calls() >= 1);
        assert_eq!(numbers(&daemon).await, [7, 8]);
    }
}
