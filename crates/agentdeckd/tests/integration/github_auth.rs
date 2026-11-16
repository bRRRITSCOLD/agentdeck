//! The GitHub token's sources, the saved token and client ID, and Sign in with GitHub:
//! `/v1/github` (spec §4, §8), against httpmock as GitHub and the in-memory keyring.

use std::time::Duration;

use agentdeck_client::types::{
    GhLogin, GithubAuthInfo, GithubClientIdBody, GithubTokenBody, TokenEnvVar, TokenSource,
};
use agentdeck_core::ws::{DeviceOutcome, ServerMessage};
use httpmock::MockServer;
use httpmock::prelude::*;
use serde_json::json;

use crate::support::github::{
    USER_CODE, mock_access_token, mock_device_code, mock_search, mock_unauthorized, mock_viewer,
    set_gh_token,
};
use crate::support::ws::{Frame, Ws};
use crate::support::{TestDaemon, error_response, eventually_within, set_daemon_env};

/// A classic token and one for another login.
const TOKEN: &str = "ghp_agentdeckTestToken0001";
const OTHER_TOKEN: &str = "ghp_agentdeckTestToken0002";
/// The token Sign in with GitHub gets.
const SIGNED_IN_TOKEN: &str = "gho_agentdeckSignedIn0003";
const CLIENT_ID: &str = "Iv1.agentdeckclient";

/// A daemon on the in-memory keyring, with GitHub's GraphQL API and device flow on `server`,
/// and `vars` in its environment.
async fn daemon_with(server: &MockServer, vars: &[(&str, &str)]) -> TestDaemon {
    let dirs = tempfile::tempdir().expect("a temp dir");
    let oauth = server.base_url();
    let mut env = vec![
        ("AGENTDECK_KEYRING", "memory"),
        ("AGENTDECK_GITHUB_OAUTH", oauth.as_str()),
    ];
    env.extend_from_slice(vars);
    set_daemon_env(dirs.path(), &env);
    let daemon = TestDaemon::start_with_github(dirs, &server.url("/graphql")).await;
    // The first sync, at start, reads the sources; wait for it.
    sync(&daemon).await;
    daemon
}

async fn daemon(server: &MockServer) -> TestDaemon {
    daemon_with(server, &[]).await
}

async fn sync(daemon: &TestDaemon) -> agentdeck_client::types::PrSyncStatus {
    daemon
        .client
        .prs_sync()
        .send()
        .await
        .expect("sync")
        .into_inner()
}

async fn auth(daemon: &TestDaemon) -> GithubAuthInfo {
    let response = daemon.client.github_auth_get().send().await.expect("auth");
    assert_eq!(response.status().as_u16(), 200);
    response.into_inner()
}

async fn put_token(daemon: &TestDaemon, token: &str) -> GithubAuthInfo {
    let response = daemon
        .client
        .github_token_put()
        .body(GithubTokenBody {
            token: token.into(),
        })
        .send()
        .await
        .expect("save the token");
    assert_eq!(response.status().as_u16(), 200);
    response.into_inner()
}

async fn put_client_id(daemon: &TestDaemon, client_id: &str) -> GithubAuthInfo {
    let response = daemon
        .client
        .github_client_id_put()
        .body(GithubClientIdBody {
            client_id: client_id.into(),
        })
        .send()
        .await
        .expect("save the client ID");
    assert_eq!(response.status().as_u16(), 200);
    response.into_inner()
}

/// The next `github_auth` event's `device` and `message`.
async fn github_auth_event(ws: &mut Ws) -> (Option<DeviceOutcome>, Option<String>) {
    ws.until(|frame| match frame {
        Frame::Message(ServerMessage::GithubAuth { device, message }) => Some((device, message)),
        _ => None,
    })
    .await
}

/// Everything the daemon has logged, to its log file and its stderr.
fn logged(daemon: &TestDaemon) -> String {
    let log =
        std::fs::read_to_string(daemon.state_dir().join("agentdeckd.log")).unwrap_or_default();
    let stderr =
        std::fs::read_to_string(daemon.root().join("agentdeckd.stderr")).unwrap_or_default();
    format!("{log}{stderr}")
}

mod get_v1_github_auth {
    use super::*;

    #[tokio::test]
    async fn should_report_no_source_without_a_token_anywhere_200() {
        let github = MockServer::start_async().await;
        let daemon = daemon(&github).await;

        let auth = auth(&daemon).await;

        assert_eq!(auth.source, TokenSource::None);
        assert_eq!(auth.env_var, None);
        assert_eq!(auth.login, None);
        assert!(!auth.rejected);
        assert!(!auth.client_id_set);
    }

    #[tokio::test]
    async fn should_report_the_gh_login_and_its_scopes_200() {
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", Some("repo, read:org"));
        let dirs = tempfile::tempdir().expect("a temp dir");
        set_gh_token(dirs.path(), Some(TOKEN));
        set_daemon_env(dirs.path(), &[("AGENTDECK_KEYRING", "memory")]);
        let daemon = TestDaemon::start_with_github(dirs, &github.url("/graphql")).await;
        sync(&daemon).await;

        let auth = auth(&daemon).await;

        assert_eq!(auth.source, TokenSource::Gh);
        assert_eq!(auth.login.as_deref(), Some("octocat"));
        assert_eq!(auth.scopes, Some(vec!["repo".into(), "read:org".into()]));
    }

    #[tokio::test]
    async fn should_take_gh_token_from_the_environment_and_name_it_200() {
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", Some("repo"));
        let daemon = daemon_with(&github, &[("GH_TOKEN", TOKEN), ("GITHUB_TOKEN", "x")]).await;

        let auth = auth(&daemon).await;

        assert_eq!(auth.source, TokenSource::Env);
        assert_eq!(auth.env_var, Some(TokenEnvVar::GhToken));
        assert_eq!(auth.login.as_deref(), Some("octocat"));
    }

    #[tokio::test]
    async fn should_report_no_scopes_for_a_token_that_sends_none_200() {
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", None);
        let daemon = daemon_with(&github, &[("GITHUB_TOKEN", TOKEN)]).await;

        let auth = auth(&daemon).await;

        assert_eq!(auth.env_var, Some(TokenEnvVar::GithubToken));
        assert_eq!(auth.scopes, None);
    }

    #[tokio::test]
    async fn should_report_the_source_but_no_login_when_github_rejects_its_token_200() {
        let github = MockServer::start_async().await;
        mock_unauthorized(&github, TOKEN);
        let daemon = daemon_with(&github, &[("GH_TOKEN", TOKEN)]).await;

        let auth = auth(&daemon).await;
        let status = sync(&daemon).await;

        assert_eq!(auth.source, TokenSource::Env);
        assert_eq!(auth.login, None);
        assert!(auth.rejected);
        assert_eq!(status.login, GhLogin::LoggedOut);
        assert_eq!(
            status.last_error.as_deref(),
            Some("GitHub rejected the token")
        );
    }

    #[tokio::test]
    async fn should_keep_a_saved_token_github_rejects_and_tell_websocket_clients_200() {
        let github = MockServer::start_async().await;
        let viewer = mock_viewer(&github, TOKEN, "octocat", Some("repo"));
        let daemon = daemon(&github).await;
        put_token(&daemon, TOKEN).await;
        let mut ws = Ws::connect(&daemon).await;
        // The user revokes the token on GitHub.
        viewer.delete_async().await;
        mock_unauthorized(&github, TOKEN);

        let status = sync(&daemon).await;

        assert_eq!(status.login, GhLogin::LoggedOut);
        assert_eq!(github_auth_event(&mut ws).await, (None, None));
        let auth = auth(&daemon).await;
        assert_eq!(auth.source, TokenSource::Keyring);
        assert_eq!(auth.login, None);
        assert!(auth.rejected);
    }

    #[tokio::test]
    async fn should_say_github_rejected_the_saved_token_it_took_a_moment_ago_200() {
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", Some("repo"));
        // GitHub takes the token for `viewer` but refuses it for the sync's reads.
        github.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .header("authorization", format!("Bearer {TOKEN}"))
                .body_excludes("viewer { login }");
            then.status(401)
                .json_body(json!({ "message": "Bad credentials" }));
        });
        let daemon = daemon(&github).await;
        let mut ws = Ws::connect(&daemon).await;

        // Saving it starts a sync, which GitHub refuses after the re-read takes it.
        put_token(&daemon, TOKEN).await;

        assert_eq!(github_auth_event(&mut ws).await, (None, None));
        assert_eq!(github_auth_event(&mut ws).await, (None, None));
        let auth = auth(&daemon).await;
        assert_eq!(auth.source, TokenSource::Keyring);
        assert_eq!(auth.login, None);
        assert!(auth.rejected);
    }

    #[tokio::test]
    async fn should_not_call_a_token_github_could_not_be_asked_about_rejected_200() {
        let github = MockServer::start_async().await;
        github.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .header("authorization", format!("Bearer {TOKEN}"));
            then.status(403)
                .json_body(json!({ "message": "API rate limit exceeded" }));
        });
        let daemon = daemon_with(&github, &[("GH_TOKEN", TOKEN)]).await;

        let auth = auth(&daemon).await;

        assert_eq!(auth.source, TokenSource::Env);
        assert_eq!(auth.login, None);
        assert!(!auth.rejected);
    }

    #[tokio::test]
    async fn should_say_a_client_id_is_set_from_the_environment_200() {
        let github = MockServer::start_async().await;
        let daemon = daemon_with(&github, &[("AGENTDECK_GITHUB_CLIENT_ID", CLIENT_ID)]).await;

        assert!(auth(&daemon).await.client_id_set);
    }
}

mod put_v1_github_token {
    use super::*;

    #[tokio::test]
    async fn should_check_the_token_with_github_and_save_it_200() {
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", Some("repo"));
        let daemon = daemon(&github).await;

        let saved = put_token(&daemon, TOKEN).await;

        assert_eq!(saved.source, TokenSource::Keyring);
        assert_eq!(saved.login.as_deref(), Some("octocat"));
        assert_eq!(saved.scopes, Some(vec!["repo".into()]));
        assert_eq!(auth(&daemon).await, saved);
    }

    #[tokio::test]
    async fn should_sync_prs_with_the_saved_token_at_once_200() {
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", Some("repo"));
        let listing = mock_search(&github, TOKEN, &[crate::support::github::Pr::open(7)]);
        let daemon = daemon(&github).await;

        put_token(&daemon, TOKEN).await;

        eventually_within(
            "a sync with the saved token",
            Duration::from_secs(10),
            || (listing.list.calls() > 0).then_some(()),
        );
    }

    #[tokio::test]
    async fn should_save_a_token_without_repo_and_report_its_scopes_200() {
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", Some("read:org"));
        let daemon = daemon(&github).await;

        let saved = put_token(&daemon, TOKEN).await;

        assert_eq!(saved.scopes, Some(vec!["read:org".into()]));
    }

    #[tokio::test]
    async fn should_take_the_saved_token_over_gh_token_and_still_name_it_200() {
        let github = MockServer::start_async().await;
        mock_viewer(&github, OTHER_TOKEN, "hubot", Some("repo"));
        mock_viewer(&github, TOKEN, "octocat", Some("repo"));
        let daemon = daemon_with(&github, &[("GH_TOKEN", OTHER_TOKEN)]).await;

        let saved = put_token(&daemon, TOKEN).await;

        assert_eq!(saved.source, TokenSource::Keyring);
        assert_eq!(saved.env_var, Some(TokenEnvVar::GhToken));
        assert_eq!(saved.login.as_deref(), Some("octocat"));
    }

    #[tokio::test]
    async fn should_replace_the_token_saved_for_another_login_200() {
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", Some("repo"));
        mock_viewer(&github, OTHER_TOKEN, "hubot", Some("repo"));
        let daemon = daemon(&github).await;
        put_token(&daemon, TOKEN).await;

        let saved = put_token(&daemon, OTHER_TOKEN).await;
        let reread = sync(&daemon).await;

        assert_eq!(saved.login.as_deref(), Some("hubot"));
        assert_eq!(reread.login, GhLogin::LoggedIn);
        assert_eq!(auth(&daemon).await.login.as_deref(), Some("hubot"));
    }

    #[tokio::test]
    async fn should_replace_a_saved_token_github_rejected_200() {
        let github = MockServer::start_async().await;
        let viewer = mock_viewer(&github, TOKEN, "octocat", Some("repo"));
        let daemon = daemon(&github).await;
        put_token(&daemon, TOKEN).await;
        viewer.delete_async().await;
        mock_unauthorized(&github, TOKEN);
        sync(&daemon).await;
        mock_viewer(&github, OTHER_TOKEN, "octocat", Some("repo"));

        let saved = put_token(&daemon, OTHER_TOKEN).await;

        assert_eq!(saved.source, TokenSource::Keyring);
        assert_eq!(saved.login.as_deref(), Some("octocat"));
        assert!(!saved.rejected);
        assert_eq!(auth(&daemon).await, saved);
    }

    #[tokio::test]
    async fn should_tell_websocket_clients_200() {
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", Some("repo"));
        let daemon = daemon(&github).await;
        let mut ws = Ws::connect(&daemon).await;

        put_token(&daemon, TOKEN).await;

        assert_eq!(github_auth_event(&mut ws).await, (None, None));
    }

    #[tokio::test]
    async fn should_refuse_a_token_github_does_not_accept_and_save_nothing_422() {
        let github = MockServer::start_async().await;
        mock_unauthorized(&github, TOKEN);
        let daemon = daemon(&github).await;

        let (status, body) = error_response(
            daemon
                .client
                .github_token_put()
                .body(GithubTokenBody {
                    token: TOKEN.into(),
                })
                .send()
                .await,
        );

        assert_eq!(status, 422);
        assert_eq!(body.error_code.as_deref(), Some("invalid_token"));
        assert_eq!(body.message, "GitHub didn't accept this token.");
        assert_eq!(auth(&daemon).await.source, TokenSource::None);
    }

    #[tokio::test]
    async fn should_refuse_a_blank_token_422() {
        let github = MockServer::start_async().await;
        let daemon = daemon(&github).await;

        let (status, body) = error_response(
            daemon
                .client
                .github_token_put()
                .body(GithubTokenBody { token: "  ".into() })
                .send()
                .await,
        );

        assert_eq!(status, 422);
        assert_eq!(body.error_code.as_deref(), Some("invalid_token"));
    }

    #[tokio::test]
    async fn should_pass_on_githubs_message_when_it_fails_otherwise_502() {
        let github = MockServer::start_async().await;
        github.mock(|when, then| {
            when.method(POST).path("/graphql");
            then.status(403)
                .json_body(json!({ "message": "API rate limit exceeded" }));
        });
        let daemon = daemon(&github).await;

        let (status, body) = error_response(
            daemon
                .client
                .github_token_put()
                .body(GithubTokenBody {
                    token: TOKEN.into(),
                })
                .send()
                .await,
        );

        assert_eq!(status, 502);
        assert_eq!(body.error_code.as_deref(), Some("github_error"));
        assert!(body.message.contains("API rate limit exceeded"), "{body:?}");
    }

    #[tokio::test]
    async fn should_never_put_the_token_or_client_id_in_a_log_response_or_event_200() {
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", Some("repo"));
        mock_unauthorized(&github, OTHER_TOKEN);
        let daemon = daemon(&github).await;
        let mut ws = Ws::connect(&daemon).await;

        let saved = put_token(&daemon, TOKEN).await;
        let refused = reqwest::Client::new()
            .put(format!("{}/v1/github/token", daemon.base_url()))
            .bearer_auth(&daemon.info.token)
            .json(&json!({ "token": OTHER_TOKEN }))
            .send()
            .await
            .expect("a response")
            .text()
            .await
            .expect("a body");
        let with_client_id = put_client_id(&daemon, CLIENT_ID).await;
        sync(&daemon).await;
        let event = github_auth_event(&mut ws).await;

        let shown = format!(
            "{saved:?}{refused}{with_client_id:?}{event:?}{}",
            logged(&daemon)
        );
        for secret in [TOKEN, OTHER_TOKEN, CLIENT_ID] {
            assert!(!shown.contains(secret), "{secret} leaked:\n{shown}");
        }
    }
}

mod delete_v1_github_token {
    use super::*;

    #[tokio::test]
    async fn should_sign_out_to_the_next_source_200() {
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", Some("repo"));
        mock_viewer(&github, OTHER_TOKEN, "hubot", Some("repo"));
        let daemon = daemon_with(&github, &[("GH_TOKEN", OTHER_TOKEN)]).await;
        put_token(&daemon, TOKEN).await;
        let mut ws = Ws::connect(&daemon).await;

        let response = daemon
            .client
            .github_token_delete()
            .send()
            .await
            .expect("sign out");

        assert_eq!(response.status().as_u16(), 200);
        let after = response.into_inner();
        assert_eq!(after.source, TokenSource::Env);
        assert_eq!(after.login.as_deref(), Some("hubot"));
        assert_eq!(github_auth_event(&mut ws).await, (None, None));
    }

    #[tokio::test]
    async fn should_leave_no_source_when_the_saved_token_was_the_only_one_200() {
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", Some("repo"));
        let daemon = daemon(&github).await;
        put_token(&daemon, TOKEN).await;

        daemon
            .client
            .github_token_delete()
            .send()
            .await
            .expect("sign out");

        assert_eq!(auth(&daemon).await.source, TokenSource::None);
        assert_eq!(sync(&daemon).await.login, GhLogin::LoggedOut);
    }
}

mod put_v1_github_client_id {
    use super::*;

    #[tokio::test]
    async fn should_save_it_and_say_only_that_it_is_set_200() {
        let github = MockServer::start_async().await;
        let daemon = daemon(&github).await;
        let mut ws = Ws::connect(&daemon).await;

        let saved = put_client_id(&daemon, CLIENT_ID).await;

        assert!(saved.client_id_set);
        assert_eq!(github_auth_event(&mut ws).await, (None, None));
    }

    #[tokio::test]
    async fn should_refuse_a_blank_client_id_400() {
        let github = MockServer::start_async().await;
        let daemon = daemon(&github).await;

        let (status, body) = error_response(
            daemon
                .client
                .github_client_id_put()
                .body(GithubClientIdBody {
                    client_id: " ".into(),
                })
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(body.error_code.as_deref(), Some("invalid_client_id"));
    }

    #[tokio::test]
    async fn should_refuse_the_sign_in_code_pasted_in_its_place_and_save_nothing_400() {
        let github = MockServer::start_async().await;
        let daemon = daemon(&github).await;

        let (status, body) = error_response(
            daemon
                .client
                .github_client_id_put()
                .body(GithubClientIdBody {
                    client_id: format!(" {USER_CODE} "),
                })
                .send()
                .await,
        );

        assert_eq!(status, 400);
        assert_eq!(body.error_code.as_deref(), Some("invalid_client_id"));
        assert_eq!(
            body.message,
            "That looks like the sign-in code, not a client ID."
        );
        assert!(!auth(&daemon).await.client_id_set);
    }
}

mod delete_v1_github_client_id {
    use super::*;

    #[tokio::test]
    async fn should_delete_the_saved_client_id_200() {
        let github = MockServer::start_async().await;
        let daemon = daemon(&github).await;
        put_client_id(&daemon, CLIENT_ID).await;

        let response = daemon
            .client
            .github_client_id_delete()
            .send()
            .await
            .expect("clear the client ID");

        assert!(!response.into_inner().client_id_set);
    }

    #[tokio::test]
    async fn should_still_be_set_by_the_environment_200() {
        let github = MockServer::start_async().await;
        let daemon = daemon_with(&github, &[("AGENTDECK_GITHUB_CLIENT_ID", "Iv1.env")]).await;
        put_client_id(&daemon, CLIENT_ID).await;

        let response = daemon
            .client
            .github_client_id_delete()
            .send()
            .await
            .expect("clear the client ID");

        assert!(response.into_inner().client_id_set);
    }
}

mod post_v1_github_device {
    use super::*;

    async fn start(daemon: &TestDaemon) -> agentdeck_client::types::DeviceSignIn {
        let response = daemon
            .client
            .github_device_start()
            .send()
            .await
            .expect("start a sign-in");
        assert_eq!(response.status().as_u16(), 200);
        response.into_inner()
    }

    #[tokio::test]
    async fn should_refuse_without_a_client_id_409() {
        let github = MockServer::start_async().await;
        let daemon = daemon(&github).await;

        let (status, body) = error_response(daemon.client.github_device_start().send().await);

        assert_eq!(status, 409);
        assert_eq!(body.error_code.as_deref(), Some("no_client_id"));
    }

    #[tokio::test]
    async fn should_pass_on_githubs_refusal_502() {
        let github = MockServer::start_async().await;
        github.mock(|when, then| {
            when.method(POST).path("/login/device/code");
            then.status(400).json_body(json!({
                "error": "device_flow_disabled",
                "error_description": "Device Flow must be explicitly enabled for this App"
            }));
        });
        let daemon = daemon(&github).await;
        put_client_id(&daemon, CLIENT_ID).await;

        let (status, body) = error_response(daemon.client.github_device_start().send().await);

        assert_eq!(status, 502);
        assert_eq!(body.error_code.as_deref(), Some("github_error"));
        assert_eq!(
            body.message,
            "Device Flow must be explicitly enabled for this App"
        );
    }

    #[tokio::test]
    async fn should_say_github_does_not_know_the_client_id_502() {
        let github = MockServer::start_async().await;
        github.mock(|when, then| {
            when.method(POST).path("/login/device/code");
            then.status(404).json_body(json!({ "error": "Not Found" }));
        });
        let daemon = daemon(&github).await;
        put_client_id(&daemon, CLIENT_ID).await;

        let (status, body) = error_response(daemon.client.github_device_start().send().await);

        assert_eq!(status, 502);
        assert_eq!(body.error_code.as_deref(), Some("github_error"));
        assert_eq!(
            body.message,
            "GitHub doesn't know this OAuth client ID. Check it in Settings → GitHub."
        );
        assert!(!logged(&daemon).contains(CLIENT_ID));
    }

    #[tokio::test]
    async fn should_answer_with_the_code_to_enter_on_github_200() {
        let github = MockServer::start_async().await;
        mock_device_code(&github, CLIENT_ID, 900);
        mock_access_token(
            &github,
            CLIENT_ID,
            json!({ "error": "authorization_pending" }),
        );
        let daemon = daemon(&github).await;
        put_client_id(&daemon, CLIENT_ID).await;

        let sign_in = start(&daemon).await;

        assert_eq!(sign_in.user_code, USER_CODE);
        assert_eq!(sign_in.verification_uri, "https://github.com/login/device");
        let expires_at =
            chrono::DateTime::parse_from_rfc3339(&sign_in.expires_at).expect("an RFC 3339 time");
        let left = expires_at.to_utc() - chrono::Utc::now();
        assert!(left > chrono::TimeDelta::minutes(14), "{left}");
    }

    #[tokio::test]
    async fn should_save_the_token_once_the_user_approves_200() {
        let github = MockServer::start_async().await;
        mock_device_code(&github, CLIENT_ID, 900);
        mock_access_token(
            &github,
            CLIENT_ID,
            json!({ "access_token": SIGNED_IN_TOKEN, "token_type": "bearer", "scope": "repo" }),
        );
        mock_viewer(&github, SIGNED_IN_TOKEN, "octocat", Some("repo"));
        let daemon = daemon_with(&github, &[("AGENTDECK_GITHUB_CLIENT_ID", CLIENT_ID)]).await;
        let mut ws = Ws::connect(&daemon).await;

        start(&daemon).await;
        let event = github_auth_event(&mut ws).await;

        assert_eq!(event, (Some(DeviceOutcome::SignedIn), None));
        let auth = auth(&daemon).await;
        assert_eq!(auth.source, TokenSource::Keyring);
        assert_eq!(auth.login.as_deref(), Some("octocat"));
        let logged = logged(&daemon);
        assert!(logged.contains("Sign in with GitHub ended"), "{logged}");
        for secret in [
            SIGNED_IN_TOKEN,
            CLIENT_ID,
            crate::support::github::DEVICE_CODE,
        ] {
            assert!(!logged.contains(secret), "{secret} leaked:\n{logged}");
        }
    }

    #[tokio::test]
    async fn should_say_the_user_denied_access() {
        let github = MockServer::start_async().await;
        mock_device_code(&github, CLIENT_ID, 900);
        mock_access_token(&github, CLIENT_ID, json!({ "error": "access_denied" }));
        let daemon = daemon_with(&github, &[("AGENTDECK_GITHUB_CLIENT_ID", CLIENT_ID)]).await;
        let mut ws = Ws::connect(&daemon).await;

        start(&daemon).await;

        assert_eq!(
            github_auth_event(&mut ws).await,
            (Some(DeviceOutcome::Denied), None)
        );
        assert_eq!(auth(&daemon).await.source, TokenSource::None);
    }

    #[tokio::test]
    async fn should_say_the_code_expired() {
        let github = MockServer::start_async().await;
        mock_device_code(&github, CLIENT_ID, 2);
        mock_access_token(
            &github,
            CLIENT_ID,
            json!({ "error": "authorization_pending" }),
        );
        let daemon = daemon_with(&github, &[("AGENTDECK_GITHUB_CLIENT_ID", CLIENT_ID)]).await;
        let mut ws = Ws::connect(&daemon).await;

        start(&daemon).await;

        assert_eq!(
            github_auth_event(&mut ws).await,
            (Some(DeviceOutcome::Expired), None)
        );
    }

    #[tokio::test]
    async fn should_fail_with_githubs_message() {
        let github = MockServer::start_async().await;
        mock_device_code(&github, CLIENT_ID, 900);
        mock_access_token(
            &github,
            CLIENT_ID,
            json!({ "error": "incorrect_client_credentials", "error_description": "The client_id is not valid." }),
        );
        let daemon = daemon_with(&github, &[("AGENTDECK_GITHUB_CLIENT_ID", CLIENT_ID)]).await;
        let mut ws = Ws::connect(&daemon).await;

        start(&daemon).await;

        assert_eq!(
            github_auth_event(&mut ws).await,
            (
                Some(DeviceOutcome::Failed),
                Some("The client_id is not valid.".into())
            )
        );
    }
}

mod delete_v1_github_device {
    use super::*;

    #[tokio::test]
    async fn should_stop_polling_github_204() {
        let github = MockServer::start_async().await;
        mock_device_code(&github, CLIENT_ID, 900);
        let polls = mock_access_token(
            &github,
            CLIENT_ID,
            json!({ "error": "authorization_pending" }),
        );
        let daemon = daemon_with(&github, &[("AGENTDECK_GITHUB_CLIENT_ID", CLIENT_ID)]).await;
        daemon
            .client
            .github_device_start()
            .send()
            .await
            .expect("start a sign-in");
        eventually_within("a first poll", Duration::from_secs(10), || {
            (polls.calls() > 0).then_some(())
        });

        let response = daemon
            .client
            .github_device_cancel()
            .send()
            .await
            .expect("cancel the sign-in");

        assert_eq!(response.status().as_u16(), 204);
        let after_cancel = polls.calls();
        // Two poll intervals.
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(polls.calls(), after_cancel);
    }

    #[tokio::test]
    async fn should_answer_with_no_sign_in_running_204() {
        let github = MockServer::start_async().await;
        let daemon = daemon(&github).await;

        let response = daemon
            .client
            .github_device_cancel()
            .send()
            .await
            .expect("cancel");

        assert_eq!(response.status().as_u16(), 204);
    }
}
