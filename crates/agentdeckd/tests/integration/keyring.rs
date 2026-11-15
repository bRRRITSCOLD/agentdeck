//! The keyring store (spec §8, §12): the same cases against the real Secret Service
//! (`gnome-keyring-daemon` on a private bus) and against the in-memory fake the other
//! integration tests use, so the fake keeps the real store's contract. Each daemon is driven
//! over `/v1/github`, with GitHub on httpmock.

use agentdeck_client::types::{GithubClientIdBody, GithubTokenBody, TokenSource};
use httpmock::MockServer;

use crate::support::github::mock_viewer;
use crate::support::keyring::SecretService;
use crate::support::{TestDaemon, error_response, set_daemon_env};

const TOKEN: &str = "ghp_keyringContractToken01";
const OTHER_TOKEN: &str = "ghp_keyringContractToken02";
const CLIENT_ID: &str = "Iv1.keyringcontract";

/// Which store a daemon keeps its credentials in.
enum Store<'a> {
    Memory,
    SecretService(&'a SecretService),
}

/// Starts a daemon on `dirs` keeping its credentials in `store`, with GitHub on `github`.
async fn start(dirs: tempfile::TempDir, store: &Store<'_>, github: &MockServer) -> TestDaemon {
    match store {
        Store::Memory => set_daemon_env(dirs.path(), &[("AGENTDECK_KEYRING", "memory")]),
        Store::SecretService(service) => set_daemon_env(
            dirs.path(),
            &[("DBUS_SESSION_BUS_ADDRESS", service.address())],
        ),
    }
    let daemon = TestDaemon::start_with_github(dirs, &github.url("/graphql")).await;
    daemon.client.prs_sync().send().await.expect("sync");
    daemon
}

async fn put_token(daemon: &TestDaemon, token: &str) {
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
}

async fn source_and_login(daemon: &TestDaemon) -> (TokenSource, Option<String>) {
    let auth = daemon
        .client
        .github_auth_get()
        .send()
        .await
        .expect("auth")
        .into_inner();
    (auth.source, auth.login)
}

/// The contract: saves the token, finds it, replaces it with another login's, deletes it;
/// saves and deletes the client ID.
async fn should_save_find_replace_and_delete(store: Store<'_>) {
    let github = MockServer::start_async().await;
    mock_viewer(&github, TOKEN, "octocat", Some("repo"));
    mock_viewer(&github, OTHER_TOKEN, "hubot", Some("repo"));
    let daemon = start(tempfile::tempdir().expect("a temp dir"), &store, &github).await;
    assert_eq!(source_and_login(&daemon).await, (TokenSource::None, None));

    put_token(&daemon, TOKEN).await;
    assert_eq!(
        source_and_login(&daemon).await,
        (TokenSource::Keyring, Some("octocat".into()))
    );

    put_token(&daemon, OTHER_TOKEN).await;
    assert_eq!(
        source_and_login(&daemon).await,
        (TokenSource::Keyring, Some("hubot".into()))
    );

    let client_id = daemon
        .client
        .github_client_id_put()
        .body(GithubClientIdBody {
            client_id: CLIENT_ID.into(),
        })
        .send()
        .await
        .expect("save the client ID")
        .into_inner();
    assert!(client_id.client_id_set);

    daemon
        .client
        .github_token_delete()
        .send()
        .await
        .expect("delete the token");
    assert_eq!(source_and_login(&daemon).await, (TokenSource::None, None));

    let cleared = daemon
        .client
        .github_client_id_delete()
        .send()
        .await
        .expect("delete the client ID")
        .into_inner();
    assert!(!cleared.client_id_set);
}

mod memory {
    use super::*;

    #[tokio::test]
    async fn should_save_find_replace_and_delete_the_token_and_client_id() {
        should_save_find_replace_and_delete(Store::Memory).await;
    }
}

mod secret_service {
    use super::*;

    #[tokio::test]
    async fn should_save_find_replace_and_delete_the_token_and_client_id() {
        let service = SecretService::start().await;

        should_save_find_replace_and_delete(Store::SecretService(&service)).await;

        assert!(service.agentdeck_entries().await.is_empty());
    }

    #[tokio::test]
    async fn should_keep_one_token_entry_named_after_its_login_and_the_client_id_apart() {
        let service = SecretService::start().await;
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", Some("repo"));
        mock_viewer(&github, OTHER_TOKEN, "hubot", Some("repo"));
        let store = Store::SecretService(&service);
        let daemon = start(tempfile::tempdir().expect("a temp dir"), &store, &github).await;

        put_token(&daemon, TOKEN).await;
        put_token(&daemon, OTHER_TOKEN).await;
        daemon
            .client
            .github_client_id_put()
            .body(GithubClientIdBody {
                client_id: CLIENT_ID.into(),
            })
            .send()
            .await
            .expect("save the client ID");

        assert_eq!(
            service.agentdeck_entries().await,
            ["github.com/hubot", "oauth-client-id@github.com"]
        );
    }

    #[tokio::test]
    async fn should_find_the_saved_token_and_client_id_after_a_restart() {
        let service = SecretService::start().await;
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", Some("repo"));
        let store = Store::SecretService(&service);
        let daemon = start(tempfile::tempdir().expect("a temp dir"), &store, &github).await;
        put_token(&daemon, TOKEN).await;
        daemon
            .client
            .github_client_id_put()
            .body(GithubClientIdBody {
                client_id: CLIENT_ID.into(),
            })
            .send()
            .await
            .expect("save the client ID");

        let (_, dirs) = daemon.terminate();
        let daemon = start(dirs, &store, &github).await;

        let auth = daemon
            .client
            .github_auth_get()
            .send()
            .await
            .expect("auth")
            .into_inner();
        assert_eq!(auth.source, TokenSource::Keyring);
        assert_eq!(auth.login.as_deref(), Some("octocat"));
        assert!(auth.client_id_set);
    }

    #[tokio::test]
    async fn should_find_none_and_refuse_to_save_with_no_secret_service_503() {
        let github = MockServer::start_async().await;
        mock_viewer(&github, TOKEN, "octocat", Some("repo"));
        let dirs = tempfile::tempdir().expect("a temp dir");
        // The harness's daemon has no session bus at all.
        let daemon = TestDaemon::start_with_github(dirs, &github.url("/graphql")).await;
        daemon.client.prs_sync().send().await.expect("sync");

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
        let (client_id_status, _) = error_response(
            daemon
                .client
                .github_client_id_put()
                .body(GithubClientIdBody {
                    client_id: CLIENT_ID.into(),
                })
                .send()
                .await,
        );
        let (device_status, _) = error_response(daemon.client.github_device_start().send().await);

        assert_eq!(source_and_login(&daemon).await, (TokenSource::None, None));
        assert_eq!(status, 503);
        assert_eq!(body.error_code.as_deref(), Some("no_keyring"));
        assert_eq!(
            body.message,
            "No system keyring is running, so agentdeck can't save a token. Set GH_TOKEN in \
             agentdeckd's environment instead."
        );
        assert_eq!(client_id_status, 503);
        assert_eq!(device_status, 503);
    }
}
