//! Codex's session id (spec §6). Codex has no flag to start on a session id agentdeck
//! chooses, so the daemon finds the one it chose: Codex records each session in
//! `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<timestamp>-<session id>.jsonl`, and a new
//! pane's session is the newest such file that wasn't there when the pane launched.
//!
//! Checked against codex-cli 0.169.0 on 2026-11-14: it writes that file once the first
//! prompt is sent (none for a session that never gets one), its id is a UUIDv7, and it is the
//! id `codex resume` takes and Codex prints on exit.

use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use agentdeck_core::ws::Entity;
use agentdeck_core::{Agent, AgentState, PaneId};
use uuid::Uuid;

use crate::db::{self, Db};
use crate::events::Events;
use crate::terminal::Terminals;
use crate::terminal::pane::Launch;

/// How often the sessions dir is looked at while waiting for a new pane's file.
const POLL: Duration = Duration::from_millis(250);

/// How long a new pane's file is waited for. Codex writes it at the latest once its first
/// prompt is sent; a pane nobody types into for this long resumes as a shell.
const WAIT: Duration = Duration::from_secs(120);

/// How deep session files sit under the sessions dir: `YYYY/MM/DD/<file>`.
const DEPTH: usize = 4;

/// The sessions dir a Codex pane writes to, from the environment it was launched with:
/// `$CODEX_HOME/sessions`, by default `$HOME/.codex/sessions`.
pub fn sessions_dir(env: &BTreeMap<OsString, OsString>) -> Option<PathBuf> {
    let absolute = |name: &str| {
        env.get(OsStr::new(name))
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let home =
        absolute("CODEX_HOME").or_else(|| absolute("HOME").map(|home| home.join(".codex")))?;
    Some(home.join("sessions"))
}

/// The session id in a session file's name, `rollout-<timestamp>-<uuid>.jsonl`, optionally
/// with `_<rollout id>` after the uuid.
pub fn session_id(file_name: &str) -> Option<String> {
    let stem = file_name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    let stem = stem.split('_').next()?;
    let start = stem.len().checked_sub(36)?;
    let id = Uuid::parse_str(stem.get(start..)?).ok()?;
    Some(id.hyphenated().to_string())
}

/// Every session file under `dir`, with when it last changed. A missing dir has none.
pub fn session_files(dir: &Path) -> Vec<(PathBuf, SystemTime)> {
    let mut files = Vec::new();
    walk(dir, DEPTH, &mut files);
    files
}

fn walk(dir: &Path, depth: usize, files: &mut Vec<(PathBuf, SystemTime)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() && depth > 1 {
            walk(&path, depth - 1, files);
        } else if kind.is_file()
            && entry.file_name().to_str().and_then(session_id).is_some()
            && let Ok(modified) = entry.metadata().and_then(|meta| meta.modified())
        {
            files.push((path, modified));
        }
    }
}

/// The session id of the newest file in `now` that isn't in `before` and that no other pane
/// has `claimed`.
pub fn new_session(
    before: &HashSet<PathBuf>,
    now: &[(PathBuf, SystemTime)],
    claimed: &HashSet<String>,
) -> Option<String> {
    now.iter()
        .filter(|(path, _)| !before.contains(path))
        .filter_map(|(path, modified)| {
            let id = session_id(path.file_name()?.to_str()?)?;
            (!claimed.contains(&id)).then_some((modified, id))
        })
        .max()
        .map(|(_, id)| id)
}

/// For a Codex launch, its sessions dir and the session files already there, to find the one
/// it writes ([`watch`]). `None` for any other agent, and for a shell.
pub fn sessions_before(
    agent: Option<Agent>,
    launch: &Launch,
) -> Option<(PathBuf, HashSet<PathBuf>)> {
    (agent == Some(Agent::Codex))
        .then(|| sessions_dir(&launch.env))
        .flatten()
        .map(|dir| {
            let before = session_files(&dir)
                .into_iter()
                .map(|(path, _)| path)
                .collect();
            (dir, before)
        })
}

/// Watches `dir` for pane `id`'s session file, which `before` (taken just before launch)
/// doesn't hold, and stores its session id. Gives up when the pane is gone or has exited, or
/// after [`WAIT`].
pub fn watch(
    db: Db,
    terminals: Terminals,
    events: Events,
    id: PaneId,
    dir: PathBuf,
    before: HashSet<PathBuf>,
) {
    let before = Arc::new(before);
    tokio::spawn(async move {
        let deadline = Instant::now() + WAIT;
        loop {
            tokio::time::sleep(POLL).await;
            let live = terminals
                .get(id)
                .is_some_and(|pane| pane.status().state != AgentState::Exited);
            if !live {
                return;
            }
            let scan = dir.clone();
            let Ok(now) = tokio::task::spawn_blocking(move || session_files(&scan)).await else {
                return;
            };
            let before = Arc::clone(&before);
            let found = db
                .call(move |c| {
                    let claimed = db::panes::session_ids(c)?;
                    let Some(session) = new_session(&before, &now, &claimed) else {
                        return Ok(None);
                    };
                    db::panes::set_session_id(c, id, &session)?;
                    Ok(Some(session))
                })
                .await;
            match found {
                Ok(Some(_)) => {
                    events.changed(Entity::Pane, id);
                    return;
                }
                Ok(None) if Instant::now() < deadline => {}
                Ok(None) => {
                    tracing::warn!("no Codex session file for pane {id} in {}", dir.display());
                    return;
                }
                Err(err) => {
                    tracing::warn!("saving pane {id}'s Codex session: {err}");
                    return;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "5973b6c0-94b8-487b-a530-2aeb6098ae0e";

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<OsString, OsString> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into()))
            .collect()
    }

    mod sessions_dir {
        use super::*;

        #[test]
        fn should_be_under_codex_home() {
            let env = env(&[("CODEX_HOME", "/codex"), ("HOME", "/home/me")]);
            assert_eq!(sessions_dir(&env).unwrap(), Path::new("/codex/sessions"));
        }

        #[test]
        fn should_default_to_dot_codex_in_home() {
            let env = env(&[("HOME", "/home/me")]);
            assert_eq!(
                sessions_dir(&env).unwrap(),
                Path::new("/home/me/.codex/sessions")
            );
        }

        #[test]
        fn should_ignore_a_relative_codex_home() {
            let env = env(&[("CODEX_HOME", "codex"), ("HOME", "/home/me")]);
            assert_eq!(
                sessions_dir(&env).unwrap(),
                Path::new("/home/me/.codex/sessions")
            );
        }
    }

    mod session_id {
        use super::*;

        #[test]
        fn should_read_the_uuid_after_the_timestamp() {
            let name = format!("rollout-2025-05-07T17-24-21-{ID}.jsonl");
            assert_eq!(session_id(&name).as_deref(), Some(ID));
        }

        #[test]
        fn should_read_the_v7_uuid_codex_0_169_0_writes() {
            let id = "019a8a1c-5b0e-7d3f-9c41-2f6e8b7a1d05";
            let name = format!("rollout-2026-11-14T17-05-55-{id}.jsonl");
            assert_eq!(session_id(&name).as_deref(), Some(id));
        }

        #[test]
        fn should_ignore_a_rollout_id_suffix() {
            let name = format!("rollout-2025-05-07T17-24-21-{ID}_0198a1b2.jsonl");
            assert_eq!(session_id(&name).as_deref(), Some(ID));
        }

        #[test]
        fn should_reject_other_files() {
            assert_eq!(session_id("notes.txt"), None);
            assert_eq!(session_id("rollout-short.jsonl"), None);
            assert_eq!(session_id(&format!("rollout-x-{ID}.json")), None);
        }
    }

    mod new_session {
        use super::*;

        fn file(id: &str, secs: u64) -> (PathBuf, SystemTime) {
            (
                PathBuf::from(format!(
                    "/s/2026/11/11/rollout-2026-11-11T00-00-00-{id}.jsonl"
                )),
                SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            )
        }

        const OLD: &str = "00000000-0000-4000-8000-000000000001";
        const NEW: &str = "00000000-0000-4000-8000-000000000002";
        const NEWER: &str = "00000000-0000-4000-8000-000000000003";

        #[test]
        fn should_pick_the_newest_file_that_was_not_there_before() {
            let before: HashSet<PathBuf> = [file(OLD, 9).0].into();
            let now = [file(OLD, 9), file(NEW, 1), file(NEWER, 2)];

            assert_eq!(
                new_session(&before, &now, &HashSet::new()).as_deref(),
                Some(NEWER)
            );
        }

        #[test]
        fn should_skip_a_session_another_pane_claimed() {
            let now = [file(NEW, 1), file(NEWER, 2)];
            let claimed: HashSet<String> = [NEWER.to_owned()].into();

            assert_eq!(
                new_session(&HashSet::new(), &now, &claimed).as_deref(),
                Some(NEW)
            );
        }

        #[test]
        fn should_find_nothing_until_a_new_file_appears() {
            let before: HashSet<PathBuf> = [file(OLD, 9).0].into();

            assert_eq!(new_session(&before, &[file(OLD, 9)], &HashSet::new()), None);
        }
    }
}
