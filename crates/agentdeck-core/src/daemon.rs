//! `daemon.json` (spec §2): how a client finds the running daemon.

use serde::{Deserialize, Serialize};

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
}

impl DaemonInfo {
    /// `http://127.0.0.1:<port>`, the base URL of the daemon's API.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                }
            );
        }

        #[test]
        fn should_serve_on_localhost_at_its_port() {
            let info = DaemonInfo {
                port: 4242,
                token: String::new(),
                pid: 1,
                version: String::new(),
            };

            assert_eq!(info.base_url(), "http://127.0.0.1:4242");
        }
    }
}
