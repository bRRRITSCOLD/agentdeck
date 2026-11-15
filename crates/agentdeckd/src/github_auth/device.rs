//! GitHub's OAuth device flow (spec §8): ask for a device code, then poll for the token.
//!
//! Neither the client ID nor the device code nor the token is ever in an error, a log line or
//! a URL: each request carries them in its form body.

use std::time::Duration;

use serde::Deserialize;

/// The environment variable that points the device flow at another host (tests use it).
pub const OAUTH_VAR: &str = "AGENTDECK_GITHUB_OAUTH";

/// Where the device flow's endpoints live unless [`OAUTH_VAR`] says otherwise.
pub const DEFAULT_OAUTH: &str = "https://github.com";

/// The scope a sign-in asks for, once.
const SCOPE: &str = "repo";

/// What `POST /login/device/code` answers.
#[derive(Clone, Deserialize)]
pub struct DeviceCode {
    /// What the daemon polls with; never leaves the daemon.
    pub device_code: String,
    /// What the user types on GitHub.
    pub user_code: String,
    /// Where the user types it.
    pub verification_uri: String,
    /// Seconds until the codes expire.
    pub expires_in: u64,
    /// Seconds to wait between polls.
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_interval() -> u64 {
    5
}

/// One poll's answer.
#[derive(Debug, PartialEq, Eq)]
pub enum Poll {
    /// The user hasn't approved yet: poll again.
    Pending,
    /// Polling too fast: poll again, 5 s slower.
    SlowDown,
    /// The user approved: the token.
    Token(String),
    /// The codes expired.
    Expired,
    /// The user denied access.
    Denied,
    /// Any other error, with GitHub's message.
    Failed(String),
    /// The request failed in transit: poll again.
    Unreachable,
}

/// Why asking for a device code failed: GitHub's message, or why the request did.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct OAuthError(pub String);

/// A client for the device flow's two endpoints.
#[derive(Clone)]
pub struct OAuth {
    http: reqwest::Client,
    base: String,
}

impl OAuth {
    /// A client for the endpoints under `base` (`https://github.com`).
    pub fn new(base: String) -> anyhow::Result<OAuth> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("agentdeck/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(OAuth {
            http,
            base: base.trim_end_matches('/').to_owned(),
        })
    }

    /// Asks GitHub for a device code for `client_id`, with the `repo` scope.
    pub async fn device_code(&self, client_id: &str) -> Result<DeviceCode, OAuthError> {
        let response = self
            .http
            .post(format!("{}/login/device/code", self.base))
            .header("accept", "application/json")
            .form(&[("client_id", client_id), ("scope", SCOPE)])
            .send()
            .await
            .map_err(|err| OAuthError(format!("calling GitHub: {}", err.without_url())))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if let Some(message) = error_message(&body) {
            return Err(OAuthError(message));
        }
        if !status.is_success() {
            return Err(OAuthError(format!("GitHub answered {}", status.as_u16())));
        }
        serde_json::from_str(&body)
            .map_err(|err| OAuthError(format!("unexpected GitHub response: {err}")))
    }

    /// Asks once whether the user approved `device_code`.
    pub async fn poll(&self, client_id: &str, device_code: &str) -> Poll {
        let sent = self
            .http
            .post(format!("{}/login/oauth/access_token", self.base))
            .header("accept", "application/json")
            .form(&[
                ("client_id", client_id),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await;
        let Ok(response) = sent else {
            return Poll::Unreachable;
        };
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        poll_answer(status.as_u16(), &body)
    }
}

/// What a poll's response says.
fn poll_answer(status: u16, body: &str) -> Poll {
    #[derive(Deserialize)]
    struct Answer {
        access_token: Option<String>,
        error: Option<String>,
        error_description: Option<String>,
    }
    let Ok(answer) = serde_json::from_str::<Answer>(body) else {
        return Poll::Failed(format!("GitHub answered {status}"));
    };
    if let Some(token) = answer.access_token.filter(|token| !token.is_empty()) {
        return Poll::Token(token);
    }
    match answer.error.as_deref() {
        Some("authorization_pending") => Poll::Pending,
        Some("slow_down") => Poll::SlowDown,
        Some("expired_token") => Poll::Expired,
        Some("access_denied") => Poll::Denied,
        Some(error) => Poll::Failed(answer.error_description.unwrap_or_else(|| error.to_owned())),
        None => Poll::Failed(format!("GitHub answered {status}")),
    }
}

/// GitHub's `error_description` (else its `error`) in an error body.
fn error_message(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Error {
        error: String,
        error_description: Option<String>,
    }
    let error: Error = serde_json::from_str(body).ok()?;
    Some(error.error_description.unwrap_or(error.error))
}

/// The wait before the next poll: GitHub's `interval` (at least 1 s, so a 0 never makes a
/// busy loop), 5 s more for each `slow_down` so far.
pub fn poll_wait(interval: u64, slow_downs: u32) -> Duration {
    Duration::from_secs(interval.max(1) + 5 * u64::from(slow_downs))
}

#[cfg(test)]
mod tests {
    use super::*;

    mod poll_answer {
        use super::*;

        #[test]
        fn should_read_the_token() {
            let body = r#"{"access_token":"gho_x","token_type":"bearer","scope":"repo"}"#;
            assert_eq!(poll_answer(200, body), Poll::Token("gho_x".into()));
        }

        #[test]
        fn should_read_each_error_github_names() {
            let cases = [
                ("authorization_pending", Poll::Pending),
                ("slow_down", Poll::SlowDown),
                ("expired_token", Poll::Expired),
                ("access_denied", Poll::Denied),
            ];
            for (error, poll) in cases {
                let body = format!(r#"{{"error":"{error}","error_description":"x"}}"#);
                assert_eq!(poll_answer(200, &body), poll, "{error}");
            }
        }

        #[test]
        fn should_fail_with_githubs_description_of_any_other_error() {
            let body = r#"{"error":"incorrect_client_credentials","error_description":"The client_id is not valid."}"#;
            assert_eq!(
                poll_answer(200, body),
                Poll::Failed("The client_id is not valid.".into())
            );
        }

        #[test]
        fn should_fail_with_the_status_when_the_body_is_not_json() {
            assert_eq!(
                poll_answer(502, "<html>"),
                Poll::Failed("GitHub answered 502".into())
            );
        }
    }

    mod error_message {
        use super::*;

        #[test]
        fn should_prefer_the_description() {
            let body = r#"{"error":"device_flow_disabled","error_description":"Device Flow must be explicitly enabled for this App"}"#;
            assert_eq!(
                error_message(body).as_deref(),
                Some("Device Flow must be explicitly enabled for this App")
            );
        }

        #[test]
        fn should_fall_back_to_the_error() {
            assert_eq!(
                error_message(r#"{"error":"unauthorized_client"}"#).as_deref(),
                Some("unauthorized_client")
            );
        }

        #[test]
        fn should_find_none_in_a_device_code() {
            let body = r#"{"device_code":"d","user_code":"ABCD-1234","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#;
            assert_eq!(error_message(body), None);
        }
    }

    mod poll_wait {
        use super::*;

        #[test]
        fn should_wait_githubs_interval() {
            assert_eq!(poll_wait(5, 0), Duration::from_secs(5));
        }

        #[test]
        fn should_wait_five_seconds_more_after_each_slow_down() {
            assert_eq!(poll_wait(5, 2), Duration::from_secs(15));
        }

        #[test]
        fn should_never_wait_less_than_a_second() {
            assert_eq!(poll_wait(0, 0), Duration::from_secs(1));
        }
    }
}
