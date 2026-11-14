//! `$XDG_CONFIG_HOME/agentdeck/config.toml`: the settings the daemon reads at start (spec §2).
//!
//! Only `[pr_sync]` for now (spec §8). A key this daemon doesn't know is ignored, so a file
//! written for a newer one still loads. A missing file or key gives the default; a file that
//! doesn't parse is logged and every default applies, as a state rule file's is.

use std::path::Path;

use chrono::TimeDelta;
use serde::Deserialize;

/// The environment variable that overrides `[pr_sync] max_age_days` (tests use it). Empty is
/// the same as unset.
pub const PR_MAX_AGE_VAR: &str = "AGENTDECK_PR_MAX_AGE_DAYS";

/// `[pr_sync]`: which open PRs PR sync keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrSyncConfig {
    /// Only open PRs GitHub updated in the last this many days are synced; `0` for all.
    pub max_age_days: u32,
}

impl Default for PrSyncConfig {
    fn default() -> Self {
        PrSyncConfig { max_age_days: 90 }
    }
}

impl PrSyncConfig {
    /// Reads `config.toml` at `path`, then `env` (the value of [`PR_MAX_AGE_VAR`]) over it.
    pub fn load(path: &Path, env: Option<&str>) -> PrSyncConfig {
        let file = match std::fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                tracing::warn!("reading {}: {err}; using the defaults", path.display());
                None
            }
        };
        PrSyncConfig::resolve(file.as_deref(), env)
    }

    /// The config from `config.toml`'s text, if there is a file, and the variable's value.
    pub fn resolve(file: Option<&str>, env: Option<&str>) -> PrSyncConfig {
        let mut config = PrSyncConfig::default();
        if let Some(text) = file {
            match toml::from_str::<File>(text) {
                Ok(file) => {
                    if let Some(days) = file.pr_sync.max_age_days {
                        config.max_age_days = days;
                    }
                }
                Err(err) => tracing::warn!("config.toml: {err}; using the defaults"),
            }
        }
        if let Some(value) = env.map(str::trim).filter(|value| !value.is_empty()) {
            match value.parse() {
                Ok(days) => config.max_age_days = days,
                Err(err) => tracing::warn!("{PR_MAX_AGE_VAR}={value}: {err}; ignoring it"),
            }
        }
        config
    }

    /// How old an open PR may be and still sync; `None` when there is no cutoff.
    pub fn max_age(&self) -> Option<TimeDelta> {
        (self.max_age_days > 0).then(|| TimeDelta::days(i64::from(self.max_age_days)))
    }
}

/// The parts of `config.toml` this daemon reads.
#[derive(Debug, Default, Deserialize)]
struct File {
    #[serde(default)]
    pr_sync: PrSyncSection,
}

#[derive(Debug, Default, Deserialize)]
struct PrSyncSection {
    max_age_days: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    mod pr_sync_config {
        use super::*;

        fn days(file: Option<&str>, env: Option<&str>) -> u32 {
            PrSyncConfig::resolve(file, env).max_age_days
        }

        #[test]
        fn should_default_to_90_days_without_a_file() {
            assert_eq!(days(None, None), 90);
        }

        #[test]
        fn should_default_when_the_file_has_no_pr_sync_key() {
            assert_eq!(days(Some("[pr_sync]\n"), None), 90);
            assert_eq!(days(Some(""), None), 90);
        }

        #[test]
        fn should_read_max_age_days() {
            assert_eq!(days(Some("[pr_sync]\nmax_age_days = 30\n"), None), 30);
        }

        #[test]
        fn should_ignore_unknown_keys() {
            let file =
                "theme = \"dark\"\n[pr_sync]\nmax_age_days = 14\nlater = true\n[gui]\nx = 1\n";
            assert_eq!(days(Some(file), None), 14);
        }

        #[test]
        fn should_use_the_default_when_the_file_does_not_parse() {
            assert_eq!(days(Some("[pr_sync\nmax_age_days = 30"), None), 90);
        }

        #[test]
        fn should_use_the_default_when_max_age_days_is_not_a_count_of_days() {
            assert_eq!(days(Some("[pr_sync]\nmax_age_days = -1\n"), None), 90);
            assert_eq!(days(Some("[pr_sync]\nmax_age_days = \"30\"\n"), None), 90);
        }

        #[test]
        fn should_let_the_env_var_override_the_file() {
            assert_eq!(days(Some("[pr_sync]\nmax_age_days = 30\n"), Some("7")), 7);
            assert_eq!(days(None, Some("0")), 0);
        }

        #[test]
        fn should_ignore_an_empty_env_var() {
            assert_eq!(days(Some("[pr_sync]\nmax_age_days = 30\n"), Some("")), 30);
        }

        #[test]
        fn should_ignore_an_env_var_that_is_not_a_count_of_days() {
            assert_eq!(
                days(Some("[pr_sync]\nmax_age_days = 30\n"), Some("soon")),
                30
            );
        }
    }

    mod max_age {
        use super::*;

        #[test]
        fn should_be_none_for_zero_days() {
            assert_eq!(PrSyncConfig { max_age_days: 0 }.max_age(), None);
        }

        #[test]
        fn should_be_that_many_days() {
            assert_eq!(
                PrSyncConfig { max_age_days: 90 }.max_age(),
                Some(TimeDelta::days(90))
            );
        }
    }
}
