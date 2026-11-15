//! `daemon.json` (spec §2): how a client finds the running daemon, and [`API_VERSION`], which
//! tells a client whether that daemon predates it.

use serde::{Deserialize, Serialize};

/// The version of what the daemon offers its clients: the HTTP API, the WebSocket protocol and
/// `daemon.json` itself. The daemon writes it to `daemon.json`; the GUI and the CLI compare it
/// with their own on connect, and a daemon with a lower one is older than they are (spec §2).
///
/// **Bump it whenever a route, a field or a message is added or changed**, in the same PR:
/// an endpoint or its request or response in `openapi.json`, a [`crate::ws`] message, a
/// [`DaemonInfo`] field. The daemon's `should_bump_api_version_when_openapi_json_changes` test
/// fails when `openapi.json` changes without a bump.
///
/// `0` is every daemon from before it existed, whose `daemon.json` has no `api_version`.
pub const API_VERSION: u32 = 5;

/// What `$XDG_RUNTIME_DIR/agentdeck/daemon.json` holds. The daemon writes it once it serves;
/// the GUI and the CLI read it to reach the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonInfo {
    /// The port on 127.0.0.1.
    pub port: u16,
    /// The bearer token every request carries.
    pub token: String,
    /// The daemon's process id.
    pub pid: u32,
    /// The daemon's version.
    pub version: String,
    /// The daemon's [`API_VERSION`]; `0` when `daemon.json` has none, as an older daemon's.
    #[serde(default)]
    pub api_version: u32,
}

impl DaemonInfo {
    /// `http://127.0.0.1:<port>`, the base URL of the daemon's API.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Whether the daemon is older than this client: its [`API_VERSION`] is lower, so it may
    /// lack routes and fields the client uses. A newer daemon still serves an older client.
    pub fn predates_client(&self) -> bool {
        self.api_version < API_VERSION
    }

    /// `ws://127.0.0.1:<port>/v1/ws`, the daemon's WebSocket (spec §5).
    pub fn ws_url(&self) -> String {
        format!("ws://127.0.0.1:{}/v1/ws", self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod api_version {
        use super::*;

        #[test]
        fn should_be_above_0_so_a_daemon_json_without_one_is_older() {
            const { assert!(API_VERSION > 0) };
        }
    }

    mod predates_client {
        use super::*;

        fn info(api_version: u32) -> DaemonInfo {
            DaemonInfo {
                port: 1,
                token: String::new(),
                pid: 1,
                version: String::new(),
                api_version,
            }
        }

        #[test]
        fn should_be_true_for_a_daemon_older_than_the_client() {
            assert!(info(API_VERSION - 1).predates_client());
        }

        #[test]
        fn should_be_true_for_a_daemon_from_before_the_api_version() {
            assert!(info(0).predates_client());
        }

        #[test]
        fn should_be_false_for_a_daemon_as_new_as_the_client() {
            assert!(!info(API_VERSION).predates_client());
        }

        #[test]
        fn should_be_false_for_a_daemon_newer_than_the_client() {
            assert!(!info(API_VERSION + 1).predates_client());
        }
    }

    mod daemon_info {
        use super::*;

        #[test]
        fn should_read_the_fields_the_spec_names() {
            let info: DaemonInfo = serde_json::from_str(
                r#"{ "port": 4242, "token": "t0k", "pid": 7, "version": "0.1.0" }"#,
            )
            .unwrap();

            assert_eq!(
                info,
                DaemonInfo {
                    port: 4242,
                    token: "t0k".into(),
                    pid: 7,
                    version: "0.1.0".into(),
                    api_version: 0,
                }
            );
        }

        #[test]
        fn should_read_a_daemon_json_without_api_version_as_api_version_0() {
            let info: DaemonInfo = serde_json::from_str(
                r#"{ "port": 4242, "token": "t0k", "pid": 7, "version": "0.1.0" }"#,
            )
            .unwrap();

            assert_eq!(info.api_version, 0);
        }

        #[test]
        fn should_write_its_api_version() {
            let info = DaemonInfo {
                port: 4242,
                token: String::new(),
                pid: 1,
                version: String::new(),
                api_version: 3,
            };

            let json: serde_json::Value = serde_json::to_value(&info).unwrap();

            assert_eq!(json["api_version"], 3);
        }

        #[test]
        fn should_serve_on_localhost_at_its_port() {
            let info = DaemonInfo {
                port: 4242,
                token: String::new(),
                pid: 1,
                version: String::new(),
                api_version: API_VERSION,
            };

            assert_eq!(info.base_url(), "http://127.0.0.1:4242");
        }
    }
}
