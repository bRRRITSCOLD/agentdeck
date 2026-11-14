//! Settings by dotted key (`appearance.theme`), as `agentdeck config` names them (spec §10).
//!
//! Nothing here knows which settings exist: the keys come from the daemon's
//! [`Settings`] object, and [`set`] leaves checking the key and value to the daemon, so a new
//! setting needs no change here.

use progenitor_client::ClientInfo as _;
use serde_json::{Map, Value, json};

use crate::types::{self, Settings};
use crate::{ApiError, Client};

/// The field of [`Settings`] that isn't a section of settings.
const OVERRIDDEN: &str = "overridden_by_env";

/// Every setting as its dotted key and value, sections in the order the daemon gives them.
pub fn entries(settings: &Settings) -> Vec<(String, Value)> {
    let Ok(Value::Object(sections)) = serde_json::to_value(settings) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for (section, keys) in sections {
        if section == OVERRIDDEN {
            continue;
        }
        if let Value::Object(keys) = keys {
            for (key, value) in keys {
                entries.push((format!("{section}.{key}"), value));
            }
        }
    }
    entries
}

/// One setting's value; `None` when no setting has that key.
pub fn value(settings: &Settings, key: &str) -> Option<Value> {
    entries(settings)
        .into_iter()
        .find_map(|(name, value)| (name == key).then_some(value))
}

/// The JSON a value typed on the command line stands for: a number, `true` or `false` as
/// themselves, anything else as a string.
pub fn parse_value(text: &str) -> Value {
    match serde_json::from_str::<Value>(text) {
        Ok(value @ (Value::Number(_) | Value::Bool(_))) => value,
        _ => Value::String(text.to_owned()),
    }
}

/// The `PATCH /v1/settings` body that sets one dotted key.
fn patch_body(key: &str, value: Value) -> Value {
    match key.split_once('.') {
        Some((section, name)) => json!({ section: { name: value } }),
        None => Value::Object(Map::from_iter([(key.to_owned(), value)])),
    }
}

/// Sets one setting with `PATCH /v1/settings` and returns them all. The daemon checks the key
/// and the value: one it doesn't take is `invalid_setting`, naming the key.
pub async fn set(client: &Client, key: &str, value: Value) -> Result<Settings, ApiError> {
    let transport = |err: reqwest::Error| ApiError {
        message: format!("talking to agentdeckd: {err}"),
        code: None,
    };
    let response = client
        .client()
        .patch(format!("{}/v1/settings", client.baseurl()))
        .json(&patch_body(key, value))
        .send()
        .await
        .map_err(transport)?;
    if response.status().is_success() {
        return response.json().await.map_err(transport);
    }
    let error: types::Error = response.json().await.map_err(transport)?;
    Err(ApiError {
        message: error.message,
        code: error.error_code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AppearanceSettings, PrSyncSettings, TerminalSettings, ThemePreference};

    fn settings() -> Settings {
        Settings {
            appearance: AppearanceSettings {
                theme: ThemePreference::Dark,
            },
            pr_sync: PrSyncSettings { max_age_days: 30 },
            terminal: TerminalSettings {
                file_opener: "code {path}".into(),
            },
            overridden_by_env: vec!["pr_sync.max_age_days".into()],
        }
    }

    mod entries {
        use super::*;

        #[test]
        fn should_list_every_setting_by_dotted_key() {
            assert_eq!(
                entries(&settings()),
                [
                    ("appearance.theme".to_owned(), json!("dark")),
                    ("pr_sync.max_age_days".to_owned(), json!(30)),
                    ("terminal.file_opener".to_owned(), json!("code {path}")),
                ]
            );
        }
    }

    mod value {
        use super::*;

        #[test]
        fn should_find_a_setting_by_dotted_key() {
            assert_eq!(value(&settings(), "pr_sync.max_age_days"), Some(json!(30)));
        }

        #[test]
        fn should_be_none_for_a_key_that_is_not_a_setting() {
            assert_eq!(value(&settings(), "pr_sync"), None);
            assert_eq!(value(&settings(), "overridden_by_env"), None);
            assert_eq!(value(&settings(), "appearance.font"), None);
        }
    }

    mod parse_value {
        use super::*;

        #[test]
        fn should_read_numbers_and_booleans_as_themselves() {
            assert_eq!(parse_value("30"), json!(30));
            assert_eq!(parse_value("-1"), json!(-1));
            assert_eq!(parse_value("true"), json!(true));
        }

        #[test]
        fn should_read_anything_else_as_a_string() {
            assert_eq!(parse_value("dark"), json!("dark"));
            assert_eq!(parse_value("code {path}"), json!("code {path}"));
            assert_eq!(parse_value("[1]"), json!("[1]"));
        }
    }

    mod patch_body {
        use super::*;

        #[test]
        fn should_nest_the_key_under_its_section() {
            assert_eq!(
                patch_body("appearance.theme", json!("dark")),
                json!({"appearance": {"theme": "dark"}})
            );
        }

        #[test]
        fn should_send_a_key_without_a_section_as_is_for_the_daemon_to_refuse() {
            assert_eq!(patch_body("theme", json!("dark")), json!({"theme": "dark"}));
        }
    }
}
