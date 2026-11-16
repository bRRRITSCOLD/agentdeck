//! `$XDG_CONFIG_HOME/agentdeck/config.toml`: the settings, which the daemon owns (spec §2).
//!
//! The daemon reads the file at start and writes it on `PATCH /v1/settings`; the GUI, the CLI
//! and PR sync all read the settings through the daemon, so a change applies at once. A key
//! this daemon doesn't know is kept, so a file written for a newer one still loads, and a
//! write keeps the file's other keys and comments. A missing file or key gives the default; a
//! value of the wrong kind gives that key's default; a file that doesn't parse is logged and
//! every default applies, as a state rule file's is.
//!
//! An environment variable overrides a key ([`PR_MAX_AGE_VAR`]): the settings report the
//! variable's value and name the key in `overridden_by_env`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use agentdeck_core::opener::{self, FileOpener};
use chrono::TimeDelta;
use serde_json::Value as Json;
use toml_edit::{DocumentMut, Item};

use crate::model::{
    AppearanceSettings, GuiSettings, PrSyncSettings, Settings, SettingsPatch, TerminalSettings,
    ThemePreference,
};

/// The environment variable that overrides `[pr_sync] max_age_days` (tests use it). Empty is
/// the same as unset.
pub const PR_MAX_AGE_VAR: &str = "AGENTDECK_PR_MAX_AGE_DAYS";

/// The dotted key of the theme preference.
pub const THEME_KEY: &str = "appearance.theme";

/// The dotted key of the PR age cutoff.
pub const MAX_AGE_KEY: &str = "pr_sync.max_age_days";

/// The dotted key of the command a click on a file path in a pane runs.
pub const FILE_OPENER_KEY: &str = "terminal.file_opener";

/// The dotted key of whether the GUI asks before quitting while agents run.
pub const CONFIRM_QUIT_KEY: &str = "gui.confirm_quit";

/// The PR age cutoff when nothing sets one.
const DEFAULT_MAX_AGE_DAYS: u32 = 90;

/// The allowed themes, for messages.
const THEMES: &str = "system, dark or light";

/// The settings as `config.toml` gives them: each key's value, or its default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileValues {
    /// `appearance.theme`.
    pub theme: ThemePreference,
    /// `pr_sync.max_age_days`.
    pub max_age_days: u32,
    /// `terminal.file_opener`.
    pub file_opener: String,
    /// `gui.confirm_quit`.
    pub confirm_quit: bool,
}

impl Default for FileValues {
    fn default() -> Self {
        FileValues {
            theme: ThemePreference::System,
            max_age_days: DEFAULT_MAX_AGE_DAYS,
            file_opener: opener::DEFAULT.to_owned(),
            confirm_quit: true,
        }
    }
}

impl FileValues {
    /// The values in `config.toml`'s text; `None` when there is no file.
    pub fn read(text: Option<&str>) -> FileValues {
        let mut values = FileValues::default();
        let Some(text) = text else {
            return values;
        };
        let doc = match text.parse::<DocumentMut>() {
            Ok(doc) => doc,
            Err(err) => {
                tracing::warn!("config.toml: {err}; using the defaults");
                return values;
            }
        };
        if let Some(item) = lookup(&doc, THEME_KEY) {
            match item.as_str().and_then(theme_from_str) {
                Some(theme) => values.theme = theme,
                None => tracing::warn!("config.toml: {THEME_KEY} isn't {THEMES}; using system"),
            }
        }
        if let Some(item) = lookup(&doc, MAX_AGE_KEY) {
            match item.as_integer().and_then(|days| u32::try_from(days).ok()) {
                Some(days) => values.max_age_days = days,
                None => tracing::warn!(
                    "config.toml: {MAX_AGE_KEY} isn't a count of days; using {DEFAULT_MAX_AGE_DAYS}"
                ),
            }
        }
        if let Some(item) = lookup(&doc, FILE_OPENER_KEY) {
            match item
                .as_str()
                .map(|command| (command, FileOpener::parse(command)))
            {
                Some((command, Ok(_))) => values.file_opener = command.to_owned(),
                Some((_, Err(err))) => tracing::warn!(
                    "config.toml: {FILE_OPENER_KEY} {err}; using {}",
                    opener::DEFAULT
                ),
                None => tracing::warn!(
                    "config.toml: {FILE_OPENER_KEY} isn't a string; using {}",
                    opener::DEFAULT
                ),
            }
        }
        if let Some(item) = lookup(&doc, CONFIRM_QUIT_KEY) {
            match item.as_bool() {
                Some(confirm) => values.confirm_quit = confirm,
                None => tracing::warn!(
                    "config.toml: {CONFIRM_QUIT_KEY} isn't true or false; using true"
                ),
            }
        }
        values
    }

    fn apply(&mut self, changes: &Changes) {
        if let Some(theme) = changes.theme {
            self.theme = theme;
        }
        if let Some(days) = changes.max_age_days {
            self.max_age_days = days;
        }
        if let Some(file_opener) = &changes.file_opener {
            self.file_opener.clone_from(file_opener);
        }
        if let Some(confirm) = changes.confirm_quit {
            self.confirm_quit = confirm;
        }
    }
}

/// The keys an environment variable sets, over the file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnvOverrides {
    /// [`PR_MAX_AGE_VAR`]'s value, when it holds a count of days.
    pub max_age_days: Option<u32>,
}

impl EnvOverrides {
    /// The overrides from [`PR_MAX_AGE_VAR`]'s value. Empty is the same as unset; a value that
    /// isn't a count of days is logged and ignored.
    pub fn from_var(max_age_days: Option<&str>) -> EnvOverrides {
        let max_age_days = max_age_days
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .and_then(|value| match value.parse() {
                Ok(days) => Some(days),
                Err(err) => {
                    tracing::warn!("{PR_MAX_AGE_VAR}={value}: {err}; ignoring it");
                    None
                }
            });
        EnvOverrides { max_age_days }
    }
}

/// The settings the daemon uses: the file's values with the overrides on top.
pub fn effective(file: FileValues, env: EnvOverrides) -> Settings {
    let mut overridden_by_env = Vec::new();
    if env.max_age_days.is_some() {
        overridden_by_env.push(MAX_AGE_KEY.to_owned());
    }
    Settings {
        appearance: AppearanceSettings { theme: file.theme },
        pr_sync: PrSyncSettings {
            max_age_days: env.max_age_days.unwrap_or(file.max_age_days),
        },
        terminal: TerminalSettings {
            file_opener: file.file_opener,
        },
        gui: GuiSettings {
            confirm_quit: file.confirm_quit,
        },
        overridden_by_env,
    }
}

/// What a valid [`SettingsPatch`] changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    /// A new `appearance.theme`.
    pub theme: Option<ThemePreference>,
    /// A new `pr_sync.max_age_days`.
    pub max_age_days: Option<u32>,
    /// A new `terminal.file_opener`.
    pub file_opener: Option<String>,
    /// A new `gui.confirm_quit`.
    pub confirm_quit: Option<bool>,
}

/// A PATCH named a key that isn't a setting, or gave one a value it can't take.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{key} {reason}")]
pub struct InvalidSetting {
    /// The dotted key, as `appearance.theme`.
    pub key: String,
    /// What is wrong with it, as the end of a sentence.
    pub reason: String,
}

impl InvalidSetting {
    fn new(key: impl Into<String>, reason: impl Into<String>) -> Self {
        InvalidSetting {
            key: key.into(),
            reason: reason.into(),
        }
    }
}

fn theme_from_str(value: &str) -> Option<ThemePreference> {
    match value {
        "system" => Some(ThemePreference::System),
        "dark" => Some(ThemePreference::Dark),
        "light" => Some(ThemePreference::Light),
        _ => None,
    }
}

fn theme_str(theme: ThemePreference) -> &'static str {
    match theme {
        ThemePreference::System => "system",
        ThemePreference::Dark => "dark",
        ThemePreference::Light => "light",
    }
}

/// Checks every key and value in `patch`. A `null` value leaves its key as it is.
pub fn validate(patch: &SettingsPatch) -> Result<Changes, InvalidSetting> {
    if let Some(section) = patch.unknown.keys().next() {
        return Err(InvalidSetting::new(section, "isn't a settings section"));
    }
    let mut changes = Changes::default();
    for (key, value) in section_entries("appearance", patch.appearance.as_ref())? {
        match key.as_str() {
            THEME_KEY => {
                let theme = value.as_str().and_then(theme_from_str).ok_or_else(|| {
                    InvalidSetting::new(THEME_KEY, format!("must be {THEMES}, not {value}"))
                })?;
                changes.theme = Some(theme);
            }
            _ => return Err(InvalidSetting::new(key, "isn't a setting")),
        }
    }
    for (key, value) in section_entries("pr_sync", patch.pr_sync.as_ref())? {
        match key.as_str() {
            MAX_AGE_KEY => {
                let days = value
                    .as_u64()
                    .and_then(|days| u32::try_from(days).ok())
                    .ok_or_else(|| {
                        InvalidSetting::new(
                            MAX_AGE_KEY,
                            format!("must be a whole number of days, 0 or more, not {value}"),
                        )
                    })?;
                changes.max_age_days = Some(days);
            }
            _ => return Err(InvalidSetting::new(key, "isn't a setting")),
        }
    }
    for (key, value) in section_entries("terminal", patch.terminal.as_ref())? {
        match key.as_str() {
            FILE_OPENER_KEY => {
                let command = value.as_str().ok_or_else(|| {
                    InvalidSetting::new(
                        FILE_OPENER_KEY,
                        format!("must be a command as a string, not {value}"),
                    )
                })?;
                FileOpener::parse(command)
                    .map_err(|err| InvalidSetting::new(FILE_OPENER_KEY, err.to_string()))?;
                changes.file_opener = Some(command.to_owned());
            }
            _ => return Err(InvalidSetting::new(key, "isn't a setting")),
        }
    }
    for (key, value) in section_entries("gui", patch.gui.as_ref())? {
        match key.as_str() {
            CONFIRM_QUIT_KEY => {
                let confirm = value.as_bool().ok_or_else(|| {
                    InvalidSetting::new(
                        CONFIRM_QUIT_KEY,
                        format!("must be true or false, not {value}"),
                    )
                })?;
                changes.confirm_quit = Some(confirm);
            }
            _ => return Err(InvalidSetting::new(key, "isn't a setting")),
        }
    }
    Ok(changes)
}

/// A section's non-null entries, each with its dotted key.
fn section_entries<'a>(
    section: &str,
    value: Option<&'a Json>,
) -> Result<Vec<(String, &'a Json)>, InvalidSetting> {
    match value {
        None | Some(Json::Null) => Ok(Vec::new()),
        Some(Json::Object(entries)) => Ok(entries
            .iter()
            .filter(|(_, value)| !value.is_null())
            .map(|(key, value)| (format!("{section}.{key}"), value))
            .collect()),
        Some(other) => Err(InvalidSetting::new(
            section,
            format!("must be an object of settings, not {other}"),
        )),
    }
}

/// `config.toml`'s `text` with `changes` made, every other key, table and comment kept.
pub fn write_changes(text: &str, changes: &Changes) -> Result<String, toml_edit::TomlError> {
    let mut doc = text.parse::<DocumentMut>()?;
    if let Some(theme) = changes.theme {
        set(&mut doc, THEME_KEY, theme_str(theme).into());
    }
    if let Some(days) = changes.max_age_days {
        set(&mut doc, MAX_AGE_KEY, i64::from(days).into());
    }
    if let Some(file_opener) = &changes.file_opener {
        set(&mut doc, FILE_OPENER_KEY, file_opener.as_str().into());
    }
    if let Some(confirm) = changes.confirm_quit {
        set(&mut doc, CONFIRM_QUIT_KEY, confirm.into());
    }
    Ok(doc.to_string())
}

/// The item at a `section.key`, if the file has one.
fn lookup<'a>(doc: &'a DocumentMut, dotted: &str) -> Option<&'a Item> {
    let (section, key) = dotted.split_once('.').expect("a dotted key");
    doc.get(section)?.get(key).filter(|item| !item.is_none())
}

/// Sets `section.key`, creating the section as a `[table]` when there is none and keeping a
/// replaced value's spacing and comment.
fn set(doc: &mut DocumentMut, dotted: &str, value: toml_edit::Value) {
    let (section, key) = dotted.split_once('.').expect("a dotted key");
    let item = doc.entry(section).or_insert_with(toml_edit::table);
    if item.as_table_like().is_none() {
        // Not a table: the key was unreadable anyway, and read as its default.
        *item = toml_edit::table();
    }
    let table = item.as_table_like_mut().expect("a table");
    match table.get_mut(key) {
        Some(Item::Value(old)) => {
            let decor = old.decor().clone();
            *old = value;
            *old.decor_mut() = decor;
        }
        Some(other) => *other = Item::Value(value),
        None => {
            table.insert(key, Item::Value(value));
        }
    }
}

/// Why [`Config::update`] changed nothing.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// The PATCH named a key that isn't a setting or a value it can't take.
    #[error(transparent)]
    Invalid(#[from] InvalidSetting),
    /// `config.toml` doesn't parse, so writing it would lose what the user wrote.
    #[error("{} doesn't parse, so it was left as it is: {message}", path.display())]
    Unparsable {
        /// The file.
        path: PathBuf,
        /// The parser's complaint.
        message: String,
    },
    /// Reading or writing the file failed.
    #[error("writing {}: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// What failed.
        source: std::io::Error,
    },
}

/// The settings, shared: clones see each other's updates.
#[derive(Clone)]
pub struct Config {
    inner: Arc<Inner>,
}

struct Inner {
    /// `config.toml`.
    path: PathBuf,
    env: EnvOverrides,
    /// What the file holds; held across a whole update, so updates never interleave.
    file: Mutex<FileValues>,
}

impl Config {
    /// Reads `config.toml` at `path`, with `max_age_var` ([`PR_MAX_AGE_VAR`]'s value) over it.
    pub fn load(path: PathBuf, max_age_var: Option<&str>) -> Config {
        let text = read_file(&path).unwrap_or_else(|err| {
            tracing::warn!("reading {}: {err}; using the defaults", path.display());
            None
        });
        Config {
            inner: Arc::new(Inner {
                env: EnvOverrides::from_var(max_age_var),
                file: Mutex::new(FileValues::read(text.as_deref())),
                path,
            }),
        }
    }

    /// Every setting, as the daemon uses it now.
    pub fn settings(&self) -> Settings {
        effective(self.file().clone(), self.inner.env)
    }

    /// `pr_sync.max_age_days` as PR sync uses it now; `0` for no cutoff.
    pub fn max_age_days(&self) -> u32 {
        self.settings().pr_sync.max_age_days
    }

    /// How old an open PR may be and still sync; `None` when there is no cutoff.
    pub fn max_age(&self) -> Option<TimeDelta> {
        max_age(self.max_age_days())
    }

    /// Validates `patch`, writes it to `config.toml` (creating the file and its dir if need
    /// be) and returns the settings it leaves. The file is re-read first, so an edit made by
    /// hand since the daemon started is kept.
    pub fn update(&self, patch: &SettingsPatch) -> Result<Settings, UpdateError> {
        let changes = validate(patch)?;
        let path = &self.inner.path;
        let io = |source| UpdateError::Io {
            path: path.clone(),
            source,
        };
        let mut file = self.file();
        let text = read_file(path).map_err(io)?.unwrap_or_default();
        let written = write_changes(&text, &changes).map_err(|err| UpdateError::Unparsable {
            path: path.clone(),
            message: err.message().to_owned(),
        })?;
        write_atomically(path, &written).map_err(io)?;
        *file = FileValues::read(Some(&text));
        file.apply(&changes);
        Ok(effective(file.clone(), self.inner.env))
    }

    fn file(&self) -> MutexGuard<'_, FileValues> {
        self.inner
            .file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// How old an open PR may be and still sync, for a cutoff of `days`; `None` for `0`.
pub fn max_age(days: u32) -> Option<TimeDelta> {
    (days > 0).then(|| TimeDelta::days(i64::from(days)))
}

/// The file's text; `None` when there is no file.
fn read_file(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Writes `text` to a temp file beside `path`, then renames it over `path`, so a reader never
/// sees half a file.
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path.parent().expect("config.toml has a parent dir");
    std::fs::create_dir_all(dir)?;
    let temp = path.with_extension("toml.tmp");
    let mut file = std::fs::File::create(&temp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(json: serde_json::Value) -> SettingsPatch {
        serde_json::from_value(json).expect("a patch body")
    }

    mod file_values {
        use super::*;

        fn read(text: Option<&str>) -> FileValues {
            FileValues::read(text)
        }

        #[test]
        fn should_default_to_system_90_days_xdg_open_and_asking_without_a_file() {
            assert_eq!(
                read(None),
                FileValues {
                    theme: ThemePreference::System,
                    max_age_days: 90,
                    file_opener: "xdg-open {path}".into(),
                    confirm_quit: true,
                }
            );
        }

        #[test]
        fn should_default_when_the_file_has_no_keys() {
            assert_eq!(read(Some("[pr_sync]\n")), FileValues::default());
            assert_eq!(read(Some("")), FileValues::default());
        }

        #[test]
        fn should_read_every_key() {
            let values = read(Some(
                "[appearance]\ntheme = \"dark\"\n[pr_sync]\nmax_age_days = 30\n[terminal]\nfile_opener = \"code -g {path}:{line}\"\n[gui]\nconfirm_quit = false\n",
            ));

            assert_eq!(values.theme, ThemePreference::Dark);
            assert_eq!(values.max_age_days, 30);
            assert_eq!(values.file_opener, "code -g {path}:{line}");
            assert!(!values.confirm_quit);
        }

        #[test]
        fn should_ask_before_quitting_when_confirm_quit_is_not_a_bool() {
            for value in ["\"no\"", "0"] {
                let text = format!("[gui]\nconfirm_quit = {value}\n");
                assert!(read(Some(&text)).confirm_quit, "{value}");
            }
        }

        #[test]
        fn should_default_an_opener_that_is_not_one() {
            for opener in ["\"\"", "\"code\"", "\"code '{path}\"", "1"] {
                let text = format!("[terminal]\nfile_opener = {opener}\n");
                assert_eq!(read(Some(&text)).file_opener, opener::DEFAULT, "{opener}");
            }
        }

        #[test]
        fn should_read_an_inline_table() {
            assert_eq!(
                read(Some("pr_sync = { max_age_days = 7 }\n")).max_age_days,
                7
            );
        }

        #[test]
        fn should_ignore_unknown_keys() {
            let file =
                "theme = \"dark\"\n[pr_sync]\nmax_age_days = 14\nlater = true\n[gui]\nx = 1\n";
            assert_eq!(read(Some(file)).max_age_days, 14);
        }

        #[test]
        fn should_use_the_defaults_when_the_file_does_not_parse() {
            assert_eq!(
                read(Some("[pr_sync\nmax_age_days = 30")),
                FileValues::default()
            );
        }

        #[test]
        fn should_default_only_the_key_whose_value_is_the_wrong_kind() {
            let values = read(Some(
                "[appearance]\ntheme = \"blue\"\n[pr_sync]\nmax_age_days = 30\n",
            ));
            assert_eq!(values.theme, ThemePreference::System);
            assert_eq!(values.max_age_days, 30);

            assert_eq!(
                read(Some("[pr_sync]\nmax_age_days = -1\n")).max_age_days,
                90
            );
            assert_eq!(
                read(Some("[pr_sync]\nmax_age_days = \"30\"\n")).max_age_days,
                90
            );
        }
    }

    mod env_overrides {
        use super::*;

        #[test]
        fn should_read_a_count_of_days() {
            assert_eq!(EnvOverrides::from_var(Some("7")).max_age_days, Some(7));
            assert_eq!(EnvOverrides::from_var(Some(" 0 ")).max_age_days, Some(0));
        }

        #[test]
        fn should_ignore_an_empty_or_unset_variable() {
            assert_eq!(EnvOverrides::from_var(Some("")), EnvOverrides::default());
            assert_eq!(EnvOverrides::from_var(None), EnvOverrides::default());
        }

        #[test]
        fn should_ignore_a_value_that_is_not_a_count_of_days() {
            assert_eq!(
                EnvOverrides::from_var(Some("soon")),
                EnvOverrides::default()
            );
            assert_eq!(EnvOverrides::from_var(Some("-1")), EnvOverrides::default());
        }
    }

    mod effective {
        use super::*;

        fn file(days: u32) -> FileValues {
            FileValues {
                max_age_days: days,
                ..FileValues::default()
            }
        }

        #[test]
        fn should_report_the_files_values_with_nothing_overridden() {
            let settings = effective(file(30), EnvOverrides::default());

            assert_eq!(settings.pr_sync.max_age_days, 30);
            assert_eq!(settings.appearance.theme, ThemePreference::System);
            assert!(settings.gui.confirm_quit);
            assert!(settings.overridden_by_env.is_empty());
        }

        #[test]
        fn should_let_the_env_var_win_and_name_the_key() {
            let env = EnvOverrides {
                max_age_days: Some(0),
            };

            let settings = effective(file(30), env);

            assert_eq!(settings.pr_sync.max_age_days, 0);
            assert_eq!(settings.overridden_by_env, [MAX_AGE_KEY]);
        }
    }

    mod validate {
        use super::*;
        use serde_json::json;

        fn invalid_key(json: serde_json::Value) -> String {
            validate(&patch(json)).expect_err("an invalid patch").key
        }

        #[test]
        fn should_take_every_setting() {
            let changes = validate(&patch(json!({
                "appearance": { "theme": "light" },
                "pr_sync": { "max_age_days": 0 },
                "terminal": { "file_opener": "code -g {path}:{line}" },
                "gui": { "confirm_quit": false },
            })))
            .unwrap();

            assert_eq!(
                changes,
                Changes {
                    theme: Some(ThemePreference::Light),
                    max_age_days: Some(0),
                    file_opener: Some("code -g {path}:{line}".into()),
                    confirm_quit: Some(false),
                }
            );
        }

        #[test]
        fn should_name_a_confirm_quit_that_is_not_a_bool() {
            for value in [json!("no"), json!(0), json!("false")] {
                assert_eq!(
                    invalid_key(json!({"gui": {"confirm_quit": value}})),
                    CONFIRM_QUIT_KEY
                );
            }
        }

        #[test]
        fn should_name_an_opener_that_is_not_one() {
            for opener in [json!(""), json!("code"), json!("code '{path}"), json!(1)] {
                assert_eq!(
                    invalid_key(json!({"terminal": {"file_opener": opener}})),
                    FILE_OPENER_KEY
                );
            }
        }

        #[test]
        fn should_say_what_an_opener_needs() {
            let err =
                validate(&patch(json!({"terminal": {"file_opener": "code -g"}}))).unwrap_err();

            assert_eq!(
                err.to_string(),
                "terminal.file_opener must pass {path} to the command, as in `code -g {path}:{line}`"
            );
        }

        #[test]
        fn should_change_nothing_for_an_empty_or_null_patch() {
            assert_eq!(validate(&patch(json!({}))).unwrap(), Changes::default());
            assert_eq!(
                validate(&patch(
                    json!({"appearance": null, "pr_sync": {"max_age_days": null}})
                ))
                .unwrap(),
                Changes::default()
            );
        }

        #[test]
        fn should_name_a_theme_it_does_not_know() {
            assert_eq!(
                invalid_key(json!({"appearance": {"theme": "blue"}})),
                THEME_KEY
            );
            assert_eq!(invalid_key(json!({"appearance": {"theme": 1}})), THEME_KEY);
        }

        #[test]
        fn should_name_a_cutoff_that_is_not_a_count_of_days() {
            for days in [
                json!(-1),
                json!(1.5),
                json!("30"),
                json!(u64::from(u32::MAX) + 1),
            ] {
                assert_eq!(
                    invalid_key(json!({"pr_sync": {"max_age_days": days}})),
                    MAX_AGE_KEY
                );
            }
        }

        #[test]
        fn should_name_a_key_that_is_not_a_setting() {
            assert_eq!(
                invalid_key(json!({"appearance": {"colour": "red"}})),
                "appearance.colour"
            );
            assert_eq!(invalid_key(json!({"terminal": {"x": 1}})), "terminal.x");
            assert_eq!(invalid_key(json!({"gui": {"x": 1}})), "gui.x");
            assert_eq!(invalid_key(json!({"editor": {"x": 1}})), "editor");
            assert_eq!(invalid_key(json!({"pr_sync": 30})), "pr_sync");
        }

        #[test]
        fn should_say_what_a_key_takes() {
            let err = validate(&patch(json!({"appearance": {"theme": "blue"}}))).unwrap_err();

            assert_eq!(
                err.to_string(),
                "appearance.theme must be system, dark or light, not \"blue\""
            );
        }
    }

    mod write_changes {
        use super::*;

        fn theme(theme: ThemePreference) -> Changes {
            Changes {
                theme: Some(theme),
                ..Changes::default()
            }
        }

        fn days(days: u32) -> Changes {
            Changes {
                max_age_days: Some(days),
                ..Changes::default()
            }
        }

        #[test]
        fn should_create_the_sections_in_an_empty_file() {
            let text = write_changes(
                "",
                &Changes {
                    theme: Some(ThemePreference::Dark),
                    max_age_days: Some(30),
                    file_opener: Some("code {path}".into()),
                    confirm_quit: Some(false),
                },
            )
            .unwrap();

            assert_eq!(
                text,
                "[appearance]\ntheme = \"dark\"\n\n[pr_sync]\nmax_age_days = 30\n\n[terminal]\nfile_opener = \"code {path}\"\n\n[gui]\nconfirm_quit = false\n"
            );
        }

        #[test]
        fn should_keep_other_keys_tables_and_comments() {
            let file = "# mine\n[pr_sync]\n# how far back\nmax_age_days = 90 # days\nlater = true\n\n[terminal]\nfont = \"x\"\n";

            let text = write_changes(file, &days(14)).unwrap();

            assert_eq!(text, file.replace("= 90", "= 14"));
        }

        #[test]
        fn should_add_a_key_to_an_existing_section() {
            let text = write_changes(
                "[appearance]\nfont = \"x\"\n",
                &theme(ThemePreference::Light),
            )
            .unwrap();

            assert_eq!(text, "[appearance]\nfont = \"x\"\ntheme = \"light\"\n");
        }

        #[test]
        fn should_write_into_an_inline_table() {
            let text = write_changes("pr_sync = { max_age_days = 7 }\n", &days(0)).unwrap();

            assert_eq!(FileValues::read(Some(&text)).max_age_days, 0);
            assert!(text.starts_with("pr_sync = {"), "{text}");
        }

        #[test]
        fn should_refuse_a_file_that_does_not_parse() {
            assert!(write_changes("[pr_sync\n", &days(1)).is_err());
        }

        #[test]
        fn should_read_back_what_it_wrote() {
            let text = write_changes("", &theme(ThemePreference::Light)).unwrap();

            assert_eq!(FileValues::read(Some(&text)).theme, ThemePreference::Light);
        }

        #[test]
        fn should_write_an_opener_holding_quotes_so_it_reads_back() {
            let opener = r#"kitty --title "it's" nvim +{line} {path}"#;
            let changes = Changes {
                file_opener: Some(opener.into()),
                ..Changes::default()
            };

            let text =
                write_changes("[terminal]\nfile_opener = \"x {path}\" # mine\n", &changes).unwrap();

            assert_eq!(FileValues::read(Some(&text)).file_opener, opener);
            assert!(text.ends_with(" # mine\n"), "{text}");
        }
    }

    mod config {
        use super::*;
        use serde_json::json;

        fn config_in(dir: &Path, env: Option<&str>) -> Config {
            Config::load(dir.join("agentdeck/config.toml"), env)
        }

        fn file_in(dir: &Path) -> PathBuf {
            dir.join("agentdeck/config.toml")
        }

        #[test]
        fn should_create_the_file_and_its_dir() {
            let dir = tempfile::tempdir().unwrap();
            let config = config_in(dir.path(), None);

            let settings = config
                .update(&patch(json!({"appearance": {"theme": "dark"}})))
                .unwrap();

            assert_eq!(settings.appearance.theme, ThemePreference::Dark);
            let text = std::fs::read_to_string(file_in(dir.path())).unwrap();
            assert_eq!(text, "[appearance]\ntheme = \"dark\"\n");
        }

        #[test]
        fn should_apply_a_new_cutoff_at_once() {
            let dir = tempfile::tempdir().unwrap();
            let config = config_in(dir.path(), None);
            let shared = config.clone();

            config
                .update(&patch(json!({"pr_sync": {"max_age_days": 7}})))
                .unwrap();

            assert_eq!(shared.max_age(), Some(TimeDelta::days(7)));
        }

        #[test]
        fn should_keep_an_edit_made_by_hand_since_start() {
            let dir = tempfile::tempdir().unwrap();
            let config = config_in(dir.path(), None);
            let path = file_in(dir.path());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "[pr_sync]\nmax_age_days = 5\n").unwrap();

            let settings = config
                .update(&patch(json!({"appearance": {"theme": "light"}})))
                .unwrap();

            assert_eq!(settings.pr_sync.max_age_days, 5);
        }

        #[test]
        fn should_write_an_overridden_key_but_keep_reporting_the_env_value() {
            let dir = tempfile::tempdir().unwrap();
            let config = config_in(dir.path(), Some("0"));

            let settings = config
                .update(&patch(json!({"pr_sync": {"max_age_days": 30}})))
                .unwrap();

            assert_eq!(settings.pr_sync.max_age_days, 0);
            assert_eq!(settings.overridden_by_env, [MAX_AGE_KEY]);
            let text = std::fs::read_to_string(file_in(dir.path())).unwrap();
            assert_eq!(FileValues::read(Some(&text)).max_age_days, 30);
        }

        #[test]
        fn should_leave_an_unparsable_file_as_it_is() {
            let dir = tempfile::tempdir().unwrap();
            let path = file_in(dir.path());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "[pr_sync\n").unwrap();
            let config = config_in(dir.path(), None);

            let err = config
                .update(&patch(json!({"pr_sync": {"max_age_days": 30}})))
                .unwrap_err();

            assert!(matches!(err, UpdateError::Unparsable { .. }), "{err}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "[pr_sync\n");
        }

        #[test]
        fn should_change_nothing_for_an_invalid_patch() {
            let dir = tempfile::tempdir().unwrap();
            let config = config_in(dir.path(), None);

            let err = config
                .update(&patch(json!({"appearance": {"theme": "blue"}})))
                .unwrap_err();

            assert!(matches!(err, UpdateError::Invalid(_)), "{err}");
            assert!(!dir.path().join("agentdeck").exists());
        }
    }

    mod max_age {
        use super::*;

        #[test]
        fn should_be_none_for_zero_days() {
            assert_eq!(max_age(0), None);
        }

        #[test]
        fn should_be_that_many_days() {
            assert_eq!(max_age(90), Some(TimeDelta::days(90)));
        }
    }
}
