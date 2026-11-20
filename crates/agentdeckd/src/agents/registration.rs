//! Adding agentdeck's MCP server to the agent CLIs' own config (spec §6, §11 → Agents), so an
//! agent the user starts in a shell pane has its tools too, not only the agent panes agentdeck
//! launches with `<mcp flags>`.
//!
//! - **Claude Code:** `claude mcp add --scope user agentdeck -- <cli> mcp`, after `claude mcp
//!   remove --scope user agentdeck` when an entry runs another command. Whether it is there is
//!   read from its user config, `$CLAUDE_CONFIG_DIR/.claude.json`, else `$HOME/.claude.json`:
//!   `claude mcp get` would start the server to check it.
//! - **Codex:** a `[mcp_servers.agentdeck]` table in `$CODEX_HOME/config.toml` (default
//!   `$HOME/.codex`), with the `env_vars` the launch flags name. Only that table changes; the
//!   rest of the file stays byte for byte.
//!
//! Adding is idempotent: an agent whose entry already runs `<cli> mcp` is left alone. An agent
//! CLI that isn't on the daemon's `PATH` is `not_installed` and skipped, never an error for the
//! other one.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

use super::MCP_ENV_VARS;
use crate::model::{McpAgent, McpRegistration, McpState};
use crate::terminal::env::pane_env;

/// The name the server is registered under, in both CLIs.
const NAME: &str = "agentdeck";

/// How long one `claude mcp …` may take before it is killed and counted as failed.
const CLAUDE_TIMEOUT: Duration = Duration::from_secs(60);

/// One install at a time: two would race on the same files.
static INSTALLING: Mutex<()> = Mutex::new(());

/// Whether an agent's config holds agentdeck's entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// No `agentdeck` entry.
    Missing,
    /// An `agentdeck` entry that runs something else.
    Outdated,
    /// An `agentdeck` entry that runs `<cli> mcp`.
    Added,
}

/// Whether Claude Code's user config, `text` (`None` when there's no file), registers `cli`.
pub fn claude_found(text: Option<&str>, cli: &str) -> Result<Found, String> {
    let Some(text) = text.filter(|text| !text.trim().is_empty()) else {
        return Ok(Found::Missing);
    };
    let config: serde_json::Value = serde_json::from_str(text).map_err(|err| err.to_string())?;
    let entry = &config["mcpServers"][NAME];
    if entry.is_null() {
        return Ok(Found::Missing);
    }
    let added = entry["command"] == cli && entry["args"] == serde_json::json!(["mcp"]);
    Ok(if added { Found::Added } else { Found::Outdated })
}

/// Whether Codex's `config.toml`, `text` (`None` when there's no file), registers `cli` with
/// the variables `agentdeck mcp` reads. Other keys in the table don't count.
pub fn codex_found(text: Option<&str>, cli: &str) -> Result<Found, String> {
    let doc = parse(text)?;
    let Some(entry) = doc.get("mcp_servers").and_then(|servers| servers.get(NAME)) else {
        return Ok(Found::Missing);
    };
    let strings = |key: &str| {
        entry.get(key).and_then(Item::as_array).map(|array| {
            array
                .iter()
                .map(|value| value.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
        })
    };
    let added = entry.get("command").and_then(Item::as_str) == Some(cli)
        && strings("args") == Some(Some(vec!["mcp".to_owned()]))
        && strings("env_vars") == Some(Some(MCP_ENV_VARS.iter().map(|&v| v.to_owned()).collect()));
    Ok(if added { Found::Added } else { Found::Outdated })
}

/// Codex's `config.toml`, `text` (`None` when there's no file), with `[mcp_servers.agentdeck]`
/// registering `cli`: an old table replaced where it stands, else a new one at the end. The
/// rest of the file is kept byte for byte.
pub fn codex_merged(text: Option<&str>, cli: &str) -> Result<String, String> {
    let mut doc = parse(text)?;
    let had_text = !doc.to_string().trim().is_empty();
    let servers = doc.entry("mcp_servers").or_insert_with(|| {
        let mut servers = Table::new();
        servers.set_implicit(true);
        Item::Table(servers)
    });
    match servers {
        Item::Table(servers) => match servers.get_mut(NAME) {
            Some(Item::Table(entry)) => {
                entry.clear();
                fill(entry, cli);
            }
            Some(Item::Value(Value::InlineTable(entry))) => {
                entry.clear();
                fill_inline(entry, cli);
            }
            Some(other) => *other = Item::Table(new_table(cli, false)),
            None => {
                servers.insert(NAME, Item::Table(new_table(cli, had_text)));
            }
        },
        Item::Value(Value::InlineTable(servers)) => {
            let mut entry = InlineTable::new();
            fill_inline(&mut entry, cli);
            servers.insert(NAME, Value::InlineTable(entry));
        }
        _ => return Err("mcp_servers isn't a table".to_owned()),
    }
    Ok(doc.to_string())
}

fn parse(text: Option<&str>) -> Result<DocumentMut, String> {
    text.unwrap_or_default()
        .parse::<DocumentMut>()
        .map_err(|err| one_line(&err.to_string()))
}

/// toml_edit's error on one line, without the source excerpt it draws under the position:
/// "TOML parse error at line 1, column 4, invalid table header, expected `.`, `]]`".
fn one_line(error: &str) -> String {
    error
        .lines()
        .map(str::trim)
        .filter(|line| {
            let excerpt = line.starts_with('|')
                || line
                    .split_once(" |")
                    .is_some_and(|(number, _)| number.chars().all(|c| c.is_ascii_digit()));
            !line.is_empty() && !excerpt
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `[mcp_servers.agentdeck]` registering `cli`, after a blank line when `spaced`.
fn new_table(cli: &str, spaced: bool) -> Table {
    let mut entry = Table::new();
    fill(&mut entry, cli);
    if spaced {
        entry.decor_mut().set_prefix("\n");
    }
    entry
}

/// Puts the keys registering `cli` in `entry`, a `[table]`.
fn fill(entry: &mut Table, cli: &str) {
    let mut inline = InlineTable::new();
    fill_inline(&mut inline, cli);
    for (key, value) in inline.iter() {
        entry.insert(key, Item::Value(value.clone()));
    }
}

/// Puts the keys registering `cli` in `entry`, an `{ inline = table }`: the command, `mcp`,
/// and the variables `agentdeck mcp` reads, which Codex otherwise doesn't pass (spec §6).
fn fill_inline(entry: &mut InlineTable, cli: &str) {
    entry.insert("command", Value::from(cli));
    entry.insert("args", Value::Array(Array::from_iter(["mcp"])));
    entry.insert(
        "env_vars",
        Value::Array(Array::from_iter(MCP_ENV_VARS.iter().copied())),
    );
}

/// Where the agent CLIs keep their config, and where the daemon finds them, from the
/// daemon's environment.
#[derive(Debug, Clone)]
pub struct Environment {
    vars: BTreeMap<OsString, OsString>,
}

impl Environment {
    /// The daemon's own environment.
    pub fn from_daemon() -> Environment {
        Environment::from_vars(std::env::vars_os())
    }

    /// An environment of `vars`.
    pub fn from_vars(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Environment {
        Environment {
            vars: vars.into_iter().collect(),
        }
    }

    /// An absolute path from `name`; `None` when unset, empty or relative.
    fn dir(&self, name: &str) -> Option<PathBuf> {
        self.vars
            .get(OsStr::new(name))
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    }

    fn home(&self) -> Option<PathBuf> {
        self.dir("HOME")
    }

    /// Claude Code's user config: `$CLAUDE_CONFIG_DIR/.claude.json`, else `$HOME/.claude.json`.
    pub fn claude_config(&self) -> Option<PathBuf> {
        let dir = self.dir("CLAUDE_CONFIG_DIR").or_else(|| self.home())?;
        Some(dir.join(".claude.json"))
    }

    /// Codex's config: `$CODEX_HOME/config.toml`, else `$HOME/.codex/config.toml`.
    pub fn codex_config(&self) -> Option<PathBuf> {
        let dir = self
            .dir("CODEX_HOME")
            .or_else(|| Some(self.home()?.join(".codex")))?;
        Some(dir.join("config.toml"))
    }

    /// `program` in the first of `$PATH`'s directories that has it.
    pub fn find(&self, program: &str) -> Option<PathBuf> {
        let path = self.vars.get(OsStr::new("PATH"))?;
        std::env::split_paths(path)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    }

    /// `path` with `~` for `$HOME`, as the GUI and the CLI show it.
    pub fn display(&self, path: &Path) -> String {
        match self.home().and_then(|home| path.strip_prefix(home).ok()) {
            Some(rest) => format!("~/{}", rest.display()),
            None => path.display().to_string(),
        }
    }

    /// What `claude` runs with: a pane's environment (spec §6), which carries
    /// `CLAUDE_CONFIG_DIR` when the daemon has it, so it writes the config this module reads.
    fn claude_env(&self) -> BTreeMap<OsString, OsString> {
        pane_env(self.vars.clone(), &[])
    }
}

/// Where agentdeck's entry stands in each agent's config, for `cli`.
pub fn status(env: &Environment, cli: &str) -> McpRegistration {
    McpRegistration {
        command: cli.to_owned(),
        claude: claude_status(env, cli),
        codex: codex_status(env, cli),
    }
}

/// Adds agentdeck's entry where it isn't yet, then answers with the [`status`] after, with
/// `error` on an agent whose entry couldn't be added.
pub fn install(env: &Environment, cli: &str) -> McpRegistration {
    let _one_at_a_time = INSTALLING.lock().unwrap_or_else(|err| err.into_inner());
    let before = status(env, cli);
    let claude_error = match before.claude.state {
        McpState::Missing => add_to_claude(env, cli, false).err(),
        McpState::Outdated => add_to_claude(env, cli, true).err(),
        McpState::Unreadable => before.claude.error.clone(),
        McpState::NotInstalled | McpState::Added => None,
    };
    let codex_error = match before.codex.state {
        McpState::Missing | McpState::Outdated | McpState::Unreadable => {
            add_to_codex(env, cli).err()
        }
        McpState::NotInstalled | McpState::Added => None,
    };
    let mut after = status(env, cli);
    if claude_error.is_some() {
        after.claude.error = claude_error;
    }
    if codex_error.is_some() {
        after.codex.error = codex_error;
    }
    after
}

fn claude_status(env: &Environment, cli: &str) -> McpAgent {
    let path = env.claude_config();
    let config = path
        .as_deref()
        .map_or_else(|| "~/.claude.json".to_owned(), |path| env.display(path));
    let found = match (env.find("claude"), &path) {
        (None, _) => return agent(McpState::NotInstalled, config, None),
        (Some(_), None) => Err("HOME isn't set to an absolute path".to_owned()),
        (Some(_), Some(path)) => read(path).and_then(|text| claude_found(text.as_deref(), cli)),
    };
    from_found(found, config)
}

fn codex_status(env: &Environment, cli: &str) -> McpAgent {
    let path = env.codex_config();
    let config = path.as_deref().map_or_else(
        || "~/.codex/config.toml".to_owned(),
        |path| env.display(path),
    );
    let found = match (env.find("codex"), &path) {
        (None, _) => return agent(McpState::NotInstalled, config, None),
        (Some(_), None) => Err("HOME isn't set to an absolute path".to_owned()),
        (Some(_), Some(path)) => read(path).and_then(|text| codex_found(text.as_deref(), cli)),
    };
    from_found(found, config)
}

fn from_found(found: Result<Found, String>, config: String) -> McpAgent {
    match found {
        Ok(Found::Missing) => agent(McpState::Missing, config, None),
        Ok(Found::Outdated) => agent(McpState::Outdated, config, None),
        Ok(Found::Added) => agent(McpState::Added, config, None),
        Err(why) => agent(McpState::Unreadable, config, Some(why)),
    }
}

fn agent(state: McpState, config: String, error: Option<String>) -> McpAgent {
    McpAgent {
        state,
        config,
        error,
    }
}

/// The file's text; `None` when there is no file.
fn read(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.to_string()),
    }
}

/// `claude mcp add --scope user agentdeck -- <cli> mcp`, after `claude mcp remove --scope user
/// agentdeck` when `replace`.
fn add_to_claude(env: &Environment, cli: &str, replace: bool) -> Result<(), String> {
    let claude = env
        .find("claude")
        .ok_or_else(|| "claude isn't on agentdeckd's PATH".to_owned())?;
    if replace {
        run_claude(env, &claude, &["mcp", "remove", "--scope", "user", NAME])?;
    }
    run_claude(
        env,
        &claude,
        &["mcp", "add", "--scope", "user", NAME, "--", cli, "mcp"],
    )
}

fn run_claude(env: &Environment, claude: &Path, args: &[&str]) -> Result<(), String> {
    let shown = format!("claude {}", args.join(" "));
    let mut child = Command::new(claude)
        .args(args)
        .env_clear()
        .envs(env.claude_env())
        .current_dir(env.home().unwrap_or_else(|| "/".into()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("{shown}: {err}"))?;
    let deadline = Instant::now() + CLAUDE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{shown} didn't finish in {}s",
                    CLAUDE_TIMEOUT.as_secs()
                ));
            }
            Err(err) => return Err(format!("{shown}: {err}")),
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("{shown}: {err}"))?;
    if output.status.success() {
        tracing::info!("ran {shown}");
        return Ok(());
    }
    let said = [&output.stderr, &output.stdout]
        .into_iter()
        .map(|bytes| String::from_utf8_lossy(bytes).trim().to_owned())
        .find(|text| !text.is_empty())
        .unwrap_or_else(|| format!("exited with {}", output.status));
    Err(format!("{shown}: {said}"))
}

/// Writes `[mcp_servers.agentdeck]` into Codex's config, through a symlink to where it points,
/// keeping the file's mode.
fn add_to_codex(env: &Environment, cli: &str) -> Result<(), String> {
    let path = env
        .codex_config()
        .ok_or_else(|| "HOME isn't set to an absolute path".to_owned())?;
    let shown = env.display(&path);
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let text = read(&path).map_err(|why| format!("reading {shown}: {why}"))?;
    let merged = codex_merged(text.as_deref(), cli).map_err(|why| format!("{shown}: {why}"))?;
    write_atomically(&path, &merged).map_err(|err| format!("writing {shown}: {err}"))?;
    tracing::info!("added [mcp_servers.{NAME}] to {shown}");
    Ok(())
}

/// Writes `text` to a temp file beside `path`, with `path`'s mode when it exists, then renames
/// it over `path`, so a reader never sees half a file.
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path.parent().expect("config.toml has a parent dir");
    std::fs::create_dir_all(dir)?;
    let temp = path.with_extension("toml.agentdeck-tmp");
    let mut file = std::fs::File::create(&temp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    if let Ok(meta) = std::fs::metadata(path) {
        std::fs::set_permissions(&temp, meta.permissions())?;
    }
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLI: &str = "/opt/agentdeck/bin/agentdeck";

    /// The table [`codex_merged`] writes for [`CLI`].
    const TABLE: &str = r#"[mcp_servers.agentdeck]
command = "/opt/agentdeck/bin/agentdeck"
args = ["mcp"]
env_vars = ["XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "AGENTDECK_PANE", "AGENTDECK_WORKSPACE", "AGENTDECK_TASK"]
"#;

    mod codex_merged {
        use super::*;

        #[test]
        fn should_write_only_the_table_without_a_file() {
            assert_eq!(codex_merged(None, CLI).unwrap(), TABLE);
        }

        #[test]
        fn should_add_the_table_after_the_rest_of_the_file_kept_byte_for_byte() {
            let text = "# my codex\nmodel = \"o3\"  # the default\n\n[mcp_servers.penpot]\nurl = \"http://penpot.localhost/mcp\"\n";

            let merged = codex_merged(Some(text), CLI).unwrap();

            assert_eq!(merged, format!("{text}\n{TABLE}"));
        }

        #[test]
        fn should_replace_an_old_table_where_it_stands() {
            let before = "model = \"o3\"\n\n# agentdeck's tools\n";
            let after = "\n[profiles.fast]\nmodel = \"o4-mini\" # quick\n";
            let old = "[mcp_servers.agentdeck]\ncommand = \"/old/agentdeck\"\nargs = [\"mcp\"]\nstartup_timeout_sec = 5\n";
            let text = format!("{before}{old}{after}");

            let merged = codex_merged(Some(&text), CLI).unwrap();

            assert_eq!(merged, format!("{before}{TABLE}{after}"));
        }

        #[test]
        fn should_keep_every_comment_outside_the_table() {
            let text = "# top comment\n[tui]\n# inside tui\nnotifications = true # trailing\n\n[mcp_servers.agentdeck] # mine\ncommand = \"agentdeck\"\n\n# after it\n[history]\npersistence = \"none\"\n";

            let merged = codex_merged(Some(text), CLI).unwrap();

            for comment in [
                "# top comment\n",
                "# inside tui\n",
                "# trailing\n",
                "# mine\n",
                "# after it\n",
            ] {
                assert!(merged.contains(comment), "{comment:?} kept in:\n{merged}");
            }
            assert!(merged.starts_with("# top comment\n[tui]\n# inside tui\nnotifications = true # trailing\n\n[mcp_servers.agentdeck] # mine\ncommand = \"/opt/agentdeck/bin/agentdeck\"\n"), "{merged}");
            assert!(
                merged.ends_with("\n# after it\n[history]\npersistence = \"none\"\n"),
                "{merged}"
            );
        }

        #[test]
        fn should_change_nothing_when_run_again() {
            let once = codex_merged(Some("model = \"o3\"\n"), CLI).unwrap();

            assert_eq!(codex_merged(Some(&once), CLI).unwrap(), once);
        }

        #[test]
        fn should_fill_an_inline_table_in_place() {
            let text = "mcp_servers = { agentdeck = { command = \"/old\" } }\n";

            let merged = codex_merged(Some(text), CLI).unwrap();

            assert_eq!(codex_found(Some(&merged), CLI), Ok(Found::Added));
            assert!(
                merged.starts_with("mcp_servers = { agentdeck = {"),
                "{merged}"
            );
        }

        #[test]
        fn should_refuse_a_file_that_does_not_parse() {
            let err = codex_merged(Some("[[["), CLI).unwrap_err();

            assert!(err.starts_with("TOML parse error at line 1"), "{err}");
            assert!(!err.contains('\n'), "{err}");
        }

        #[test]
        fn should_refuse_mcp_servers_that_is_not_a_table() {
            assert_eq!(
                codex_merged(Some("mcp_servers = 3\n"), CLI),
                Err("mcp_servers isn't a table".to_owned())
            );
        }
    }

    mod codex_found {
        use super::*;

        #[test]
        fn should_be_missing_without_a_file_or_a_table() {
            assert_eq!(codex_found(None, CLI), Ok(Found::Missing));
            assert_eq!(
                codex_found(Some("model = \"o3\"\n"), CLI),
                Ok(Found::Missing)
            );
        }

        #[test]
        fn should_be_added_once_merged_whatever_else_the_table_holds() {
            let text = TABLE.replace("args", "startup_timeout_sec = 5\nargs");

            assert_eq!(codex_found(Some(TABLE), CLI), Ok(Found::Added));
            assert_eq!(codex_found(Some(&text), CLI), Ok(Found::Added));
        }

        #[test]
        fn should_be_outdated_for_another_command_or_fewer_variables() {
            let other = TABLE.replace(CLI, "/old/agentdeck");
            let fewer = TABLE.replace(", \"AGENTDECK_TASK\"", "");

            assert_eq!(codex_found(Some(&other), CLI), Ok(Found::Outdated));
            assert_eq!(codex_found(Some(&fewer), CLI), Ok(Found::Outdated));
        }
    }

    mod claude_found {
        use super::*;

        #[test]
        fn should_be_missing_without_a_file_or_an_entry() {
            assert_eq!(claude_found(None, CLI), Ok(Found::Missing));
            let text = r#"{"mcpServers":{"penpot":{"type":"http","url":"http://x"}}}"#;
            assert_eq!(claude_found(Some(text), CLI), Ok(Found::Missing));
        }

        #[test]
        fn should_be_added_for_the_entry_claude_mcp_add_writes() {
            let text = r#"{"userID":"u","mcpServers":{"agentdeck":{"type":"stdio","command":"/opt/agentdeck/bin/agentdeck","args":["mcp"],"env":{}}}}"#;

            assert_eq!(claude_found(Some(text), CLI), Ok(Found::Added));
        }

        #[test]
        fn should_be_outdated_for_another_command() {
            let text =
                r#"{"mcpServers":{"agentdeck":{"command":"/old/agentdeck","args":["mcp"]}}}"#;

            assert_eq!(claude_found(Some(text), CLI), Ok(Found::Outdated));
        }

        #[test]
        fn should_refuse_a_file_that_does_not_parse() {
            assert!(claude_found(Some("{"), CLI).is_err());
        }
    }

    mod environment {
        use super::*;

        fn env(vars: &[(&str, &str)]) -> Environment {
            Environment::from_vars(vars.iter().map(|(k, v)| ((*k).into(), (*v).into())))
        }

        #[test]
        fn should_find_the_configs_under_home_by_default() {
            let env = env(&[("HOME", "/home/u")]);

            assert_eq!(env.claude_config(), Some("/home/u/.claude.json".into()));
            assert_eq!(
                env.codex_config(),
                Some("/home/u/.codex/config.toml".into())
            );
        }

        #[test]
        fn should_follow_claude_config_dir_and_codex_home() {
            let env = env(&[
                ("HOME", "/home/u"),
                ("CLAUDE_CONFIG_DIR", "/cfg/claude"),
                ("CODEX_HOME", "/cfg/codex"),
            ]);

            assert_eq!(env.claude_config(), Some("/cfg/claude/.claude.json".into()));
            assert_eq!(env.codex_config(), Some("/cfg/codex/config.toml".into()));
        }

        #[test]
        fn should_ignore_a_relative_value() {
            let env = env(&[("HOME", "/home/u"), ("CODEX_HOME", "codex")]);

            assert_eq!(
                env.codex_config(),
                Some("/home/u/.codex/config.toml".into())
            );
        }

        #[test]
        fn should_show_paths_under_home_with_a_tilde() {
            let env = env(&[("HOME", "/home/u")]);

            assert_eq!(
                env.display(Path::new("/home/u/.codex/config.toml")),
                "~/.codex/config.toml"
            );
            assert_eq!(
                env.display(Path::new("/cfg/codex/config.toml")),
                "/cfg/codex/config.toml"
            );
        }

        #[test]
        fn should_pass_claude_config_dir_to_claude_but_not_the_rest() {
            let env = env(&[
                ("HOME", "/home/u"),
                ("PATH", "/bin"),
                ("CLAUDE_CONFIG_DIR", "/cfg/claude"),
                ("CLAUDECODE", "1"),
            ]);

            let passed = env.claude_env();

            assert_eq!(passed[OsStr::new("CLAUDE_CONFIG_DIR")], "/cfg/claude");
            assert_eq!(passed[OsStr::new("HOME")], "/home/u");
            assert!(!passed.contains_key(OsStr::new("CLAUDECODE")));
        }
    }
}
