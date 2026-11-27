//! The session an agent hosted in a shell pane ran (spec §7), found once it leaves, so the
//! GUI can offer to resume it in that shell.
//!
//! The command line it was started with names the session when it was `claude --resume
//! <id>`, `claude --session-id <id>` or `codex resume <id>`. Otherwise it is the newest session
//! file written while it was hosted for the folder it ran in: Claude Code's
//! `<config dir>/projects/<folder with every character but A–Z, a–z and 0–9 as ->/<id>.jsonl`,
//! or a Codex `rollout-*.jsonl` (found as [`codex`] finds a new pane's) whose `session_meta`
//! records that folder as its `cwd`. Sessions another pane owns are never taken. Only a UUID
//! is ever returned, since the GUI types it into a shell.

use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use agentdeck_core::Agent;
use uuid::Uuid;

use super::codex;

/// What a hosted agent ran, recorded when the pane started hosting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedRun {
    /// The agent.
    pub agent: Agent,
    /// Its command line (`/proc/<pid>/cmdline`), the program (or its interpreter) first.
    pub args: Vec<String>,
    /// The folder it ran in.
    pub cwd: PathBuf,
    /// When it started being hosted, less a little slack for the tick that noticed it: a
    /// session file written since is its own.
    pub since: SystemTime,
}

/// Where each agent writes its sessions, from a pane's environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionHomes {
    /// Claude Code's `projects` dir: `$CLAUDE_CONFIG_DIR/projects`, by default
    /// `$HOME/.claude/projects`.
    pub claude_projects: Option<PathBuf>,
    /// Codex's sessions dir ([`codex::sessions_dir`]).
    pub codex_sessions: Option<PathBuf>,
}

impl SessionHomes {
    /// The dirs a pane launched with `env` writes its agents' sessions to.
    pub fn from_env(env: &BTreeMap<OsString, OsString>) -> SessionHomes {
        let absolute = |name: &str| {
            env.get(OsStr::new(name))
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        };
        let claude = absolute("CLAUDE_CONFIG_DIR")
            .or_else(|| absolute("HOME").map(|home| home.join(".claude")));
        SessionHomes {
            claude_projects: claude.map(|dir| dir.join("projects")),
            codex_sessions: codex::sessions_dir(env),
        }
    }
}

/// The session `run` worked on: the one its command line names, else the newest session
/// file of its agent for its folder written since it started being hosted that no other
/// pane has `claimed`.
pub fn session(run: &HostedRun, homes: &SessionHomes, claimed: &HashSet<String>) -> Option<String> {
    if let Some(id) = cmdline_session(run.agent, &run.args) {
        return Some(id);
    }
    match run.agent {
        Agent::Claude => newest_claude_session(
            homes.claude_projects.as_deref()?,
            &run.cwd,
            run.since,
            claimed,
        ),
        Agent::Codex => newest_codex_session(
            homes.codex_sessions.as_deref()?,
            &run.cwd,
            run.since,
            claimed,
        ),
        Agent::Other => None,
    }
}

/// The session `agent`'s command line `args` names: `--resume <id>` (`-r`, `--resume=<id>`)
/// or `--session-id <id>` for Claude Code, unless it forks a new session from it
/// (`--fork-session`); `resume <id>` for Codex. `None` without one, or when it isn't a UUID.
pub fn cmdline_session(agent: Agent, args: &[String]) -> Option<String> {
    let after = |flags: &[&str]| {
        args.windows(2)
            .find_map(|pair| flags.contains(&pair[0].as_str()).then(|| pair[1].clone()))
    };
    let id = match agent {
        Agent::Claude => {
            if args.iter().any(|arg| arg == "--fork-session") {
                return None;
            }
            let joined = args.iter().find_map(|arg| {
                (arg.strip_prefix("--resume=")).or_else(|| arg.strip_prefix("--session-id="))
            });
            joined
                .map(str::to_owned)
                .or_else(|| after(&["--resume", "-r", "--session-id"]))?
        }
        Agent::Codex => after(&["resume"])?,
        Agent::Other => return None,
    };
    uuid(&id)
}

/// `text` as a hyphenated UUID, when it is one.
fn uuid(text: &str) -> Option<String> {
    Uuid::parse_str(text)
        .ok()
        .map(|id| id.hyphenated().to_string())
}

/// The dir Claude Code keeps `cwd`'s sessions in under `projects`: the folder with every
/// character but ASCII letters and digits turned into `-`.
pub fn claude_project(projects: &Path, cwd: &Path) -> PathBuf {
    let name: String = (cwd.to_string_lossy().chars())
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    projects.join(name)
}

/// The newest Claude Code session for `cwd` under `projects` written since `since` and not
/// `claimed`: the `<uuid>.jsonl` in its project dir changed last.
pub fn newest_claude_session(
    projects: &Path,
    cwd: &Path,
    since: SystemTime,
    claimed: &HashSet<String>,
) -> Option<String> {
    let entries = std::fs::read_dir(claude_project(projects, cwd)).ok()?;
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension() != Some(OsStr::new("jsonl")) {
                return None;
            }
            let id = uuid(path.file_stem()?.to_str()?)?;
            let modified = entry.metadata().ok()?.modified().ok()?;
            (entry.file_type().ok()?.is_file() && modified >= since && !claimed.contains(&id))
                .then_some((modified, id))
        })
        .max()
        .map(|(_, id)| id)
}

/// The newest Codex session under `sessions` written since `since`, not `claimed`, whose
/// `session_meta` records `cwd` as its folder.
pub fn newest_codex_session(
    sessions: &Path,
    cwd: &Path,
    since: SystemTime,
    claimed: &HashSet<String>,
) -> Option<String> {
    let mut recent: Vec<(SystemTime, String, PathBuf)> = codex::session_files(sessions)
        .into_iter()
        .filter(|(_, modified)| *modified >= since)
        .filter_map(|(path, modified)| {
            let id = codex::session_id(path.file_name()?.to_str()?)?;
            (!claimed.contains(&id)).then_some((modified, id, path))
        })
        .collect();
    recent.sort();
    recent
        .into_iter()
        .rev()
        .find(|(_, _, path)| recorded_cwd(path).as_deref() == Some(cwd))
        .map(|(_, id, _)| id)
}

/// The folder a Codex session file's first line, its `session_meta`, records: `payload.cwd`
/// (codex-cli 0.169.0), or a top-level `cwd`.
fn recorded_cwd(path: &Path) -> Option<PathBuf> {
    let mut line = String::new();
    BufReader::new(std::fs::File::open(path).ok()?)
        .read_line(&mut line)
        .ok()?;
    let meta: serde_json::Value = serde_json::from_str(&line).ok()?;
    let cwd = meta
        .pointer("/payload/cwd")
        .or_else(|| meta.get("cwd"))?
        .as_str()?;
    Some(PathBuf::from(cwd))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const ID: &str = "5973b6c0-94b8-487b-a530-2aeb6098ae0e";
    const OTHER: &str = "019a8a1c-5b0e-7d3f-9c41-2f6e8b7a1d05";
    const NEWEST: &str = "00000000-0000-4000-8000-000000000003";

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    /// Writes `text` to `path`, making its dir, last changed at `secs`.
    fn write(path: &Path, text: &str, secs: u64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(at(secs)).unwrap();
    }

    mod cmdline_session {
        use super::*;

        #[test]
        fn should_read_claudes_resume_flag() {
            let args = args(&["claude", "--resume", ID]);
            assert_eq!(cmdline_session(Agent::Claude, &args).as_deref(), Some(ID));
        }

        #[test]
        fn should_read_claudes_short_and_joined_resume_flags() {
            let short = args(&["claude", "-r", ID]);
            let joined = args(&["claude", &format!("--resume={ID}")]);
            assert_eq!(cmdline_session(Agent::Claude, &short).as_deref(), Some(ID));
            assert_eq!(cmdline_session(Agent::Claude, &joined).as_deref(), Some(ID));
        }

        #[test]
        fn should_read_claudes_session_id_flag_after_an_interpreter() {
            let args = args(&["node", "/usr/lib/claude/cli.js", "--session-id", ID]);
            assert_eq!(cmdline_session(Agent::Claude, &args).as_deref(), Some(ID));
        }

        #[test]
        fn should_skip_a_claude_resume_that_forks_a_new_session() {
            let args = args(&["claude", "--resume", ID, "--fork-session"]);
            assert_eq!(cmdline_session(Agent::Claude, &args), None);
        }

        #[test]
        fn should_read_codexs_resume_subcommand() {
            let args = args(&["/opt/codex/codex", "resume", OTHER]);
            assert_eq!(cmdline_session(Agent::Codex, &args).as_deref(), Some(OTHER));
        }

        #[test]
        fn should_find_none_without_a_session() {
            assert_eq!(cmdline_session(Agent::Claude, &args(&["claude"])), None);
            let picker = args(&["claude", "--resume"]);
            assert_eq!(cmdline_session(Agent::Claude, &picker), None);
            let last = args(&["codex", "resume", "--last"]);
            assert_eq!(cmdline_session(Agent::Codex, &last), None);
        }

        #[test]
        fn should_take_only_a_uuid_since_it_is_typed_into_a_shell() {
            let args = args(&["claude", "--resume", "x; rm -rf ~"]);
            assert_eq!(cmdline_session(Agent::Claude, &args), None);
        }

        #[test]
        fn should_write_the_uuid_hyphenated() {
            let simple = ID.replace('-', "");
            let args = args(&["claude", "--resume", &simple]);
            assert_eq!(cmdline_session(Agent::Claude, &args).as_deref(), Some(ID));
        }
    }

    mod session_homes {
        use super::*;

        fn env(pairs: &[(&str, &str)]) -> BTreeMap<OsString, OsString> {
            (pairs.iter())
                .map(|(name, value)| ((*name).into(), (*value).into()))
                .collect()
        }

        #[test]
        fn should_find_claudes_projects_under_its_config_dir() {
            let homes = SessionHomes::from_env(&env(&[
                ("CLAUDE_CONFIG_DIR", "/claude"),
                ("HOME", "/home/me"),
            ]));
            assert_eq!(
                homes.claude_projects.unwrap(),
                Path::new("/claude/projects")
            );
        }

        #[test]
        fn should_default_to_dot_claude_in_home() {
            let homes = SessionHomes::from_env(&env(&[("HOME", "/home/me")]));
            assert_eq!(
                homes.claude_projects.unwrap(),
                Path::new("/home/me/.claude/projects")
            );
            assert_eq!(
                homes.codex_sessions.unwrap(),
                Path::new("/home/me/.codex/sessions")
            );
        }
    }

    mod claude_project {
        use super::*;

        #[test]
        fn should_turn_every_character_but_letters_and_digits_into_a_dash() {
            let dir = claude_project(
                Path::new("/p"),
                Path::new("/home/me/.local/share/my_app v2"),
            );
            assert_eq!(dir, Path::new("/p/-home-me--local-share-my-app-v2"));
        }
    }

    mod newest_claude_session {
        use super::*;

        const CWD: &str = "/work/app";

        fn session(projects: &Path, id: &str, secs: u64) {
            let dir = claude_project(projects, Path::new(CWD));
            write(&dir.join(format!("{id}.jsonl")), "{}\n", secs);
        }

        #[test]
        fn should_pick_the_newest_session_written_since_it_was_hosted() {
            let projects = tempfile::tempdir().unwrap();
            session(projects.path(), OTHER, 5);
            session(projects.path(), ID, 20);
            session(projects.path(), NEWEST, 30);

            let found =
                newest_claude_session(projects.path(), Path::new(CWD), at(10), &HashSet::new());

            assert_eq!(found.as_deref(), Some(NEWEST));
        }

        #[test]
        fn should_skip_sessions_written_before_it_was_hosted() {
            let projects = tempfile::tempdir().unwrap();
            session(projects.path(), ID, 5);

            let found =
                newest_claude_session(projects.path(), Path::new(CWD), at(10), &HashSet::new());

            assert_eq!(found, None);
        }

        #[test]
        fn should_skip_a_session_another_pane_owns() {
            let projects = tempfile::tempdir().unwrap();
            session(projects.path(), ID, 20);
            session(projects.path(), NEWEST, 30);

            let claimed = HashSet::from([NEWEST.to_owned()]);
            let found = newest_claude_session(projects.path(), Path::new(CWD), at(10), &claimed);

            assert_eq!(found.as_deref(), Some(ID));
        }

        #[test]
        fn should_skip_other_folders_and_other_files() {
            let projects = tempfile::tempdir().unwrap();
            let elsewhere = claude_project(projects.path(), Path::new("/work/other"));
            write(&elsewhere.join(format!("{NEWEST}.jsonl")), "{}\n", 30);
            let here = claude_project(projects.path(), Path::new(CWD));
            write(&here.join("notes.jsonl"), "{}\n", 30);
            write(&here.join(format!("{OTHER}.json")), "{}\n", 30);

            let found =
                newest_claude_session(projects.path(), Path::new(CWD), at(10), &HashSet::new());

            assert_eq!(found, None);
        }

        #[test]
        fn should_find_none_without_a_project_dir() {
            let projects = tempfile::tempdir().unwrap();
            let found =
                newest_claude_session(projects.path(), Path::new(CWD), at(10), &HashSet::new());
            assert_eq!(found, None);
        }
    }

    mod newest_codex_session {
        use super::*;

        fn rollout(sessions: &Path, id: &str, cwd: &str, secs: u64) {
            let path = sessions.join(format!("2026/11/27/rollout-2026-11-27T10-00-00-{id}.jsonl"));
            let meta = serde_json::json!({
                "timestamp": "2026-11-27T10:00:00.000Z",
                "type": "session_meta",
                "payload": { "id": id, "cwd": cwd },
            });
            write(&path, &format!("{meta}\n{{\"type\":\"event\"}}\n"), secs);
        }

        #[test]
        fn should_pick_the_newest_session_recorded_in_the_folder() {
            let sessions = tempfile::tempdir().unwrap();
            rollout(sessions.path(), ID, "/work/app", 20);
            rollout(sessions.path(), NEWEST, "/work/other", 30);
            rollout(sessions.path(), OTHER, "/work/app", 5);

            let found = newest_codex_session(
                sessions.path(),
                Path::new("/work/app"),
                at(10),
                &HashSet::new(),
            );

            assert_eq!(found.as_deref(), Some(ID));
        }

        #[test]
        fn should_skip_a_session_another_pane_owns() {
            let sessions = tempfile::tempdir().unwrap();
            rollout(sessions.path(), ID, "/work/app", 20);

            let claimed = HashSet::from([ID.to_owned()]);
            let found =
                newest_codex_session(sessions.path(), Path::new("/work/app"), at(10), &claimed);

            assert_eq!(found, None);
        }

        #[test]
        fn should_skip_a_file_whose_first_line_records_no_folder() {
            let sessions = tempfile::tempdir().unwrap();
            let path = sessions
                .path()
                .join(format!("2026/11/27/rollout-2026-11-27T10-00-00-{ID}.jsonl"));
            write(&path, "", 20);

            let found = newest_codex_session(
                sessions.path(),
                Path::new("/work/app"),
                at(10),
                &HashSet::new(),
            );

            assert_eq!(found, None);
        }
    }

    mod session {
        use super::*;

        fn run(agent: Agent, args: Vec<String>) -> HostedRun {
            HostedRun {
                agent,
                args,
                cwd: PathBuf::from("/work/app"),
                since: at(10),
            }
        }

        #[test]
        fn should_prefer_the_session_the_command_line_names() {
            let projects = tempfile::tempdir().unwrap();
            let dir = claude_project(projects.path(), Path::new("/work/app"));
            write(&dir.join(format!("{NEWEST}.jsonl")), "{}\n", 30);
            let homes = SessionHomes {
                claude_projects: Some(projects.path().to_owned()),
                codex_sessions: None,
            };

            let run = run(Agent::Claude, args(&["claude", "--resume", ID]));

            assert_eq!(session(&run, &homes, &HashSet::new()).as_deref(), Some(ID));
        }

        #[test]
        fn should_fall_back_to_the_newest_session_file() {
            let projects = tempfile::tempdir().unwrap();
            let dir = claude_project(projects.path(), Path::new("/work/app"));
            write(&dir.join(format!("{NEWEST}.jsonl")), "{}\n", 30);
            let homes = SessionHomes {
                claude_projects: Some(projects.path().to_owned()),
                codex_sessions: None,
            };

            let run = run(Agent::Claude, args(&["claude"]));

            assert_eq!(
                session(&run, &homes, &HashSet::new()).as_deref(),
                Some(NEWEST)
            );
        }

        #[test]
        fn should_find_none_without_a_sessions_dir() {
            let run = run(Agent::Codex, args(&["codex"]));
            assert_eq!(
                session(&run, &SessionHomes::default(), &HashSet::new()),
                None
            );
        }
    }
}
