//! The GUI's own state, kept across restarts in `$XDG_STATE_HOME/agentdeck/gui.json`, beside
//! `window.json` (spec §11): GUI-only, never the daemon's. Today it holds which Repository
//! groups are collapsed on the Pull requests view.
//!
//! Every field is optional in the file, and a field the GUI doesn't know is skipped, so a later
//! version adds its own (each Workspace's last side, Show finished, dismissed Suggestions)
//! without breaking an older file, and an older GUI reads a newer file. A missing or broken
//! file reads as the defaults.
//!
//! The state is a GPUI [`Global`] that `main` sets with [`init`]; a view reads it with
//! [`read`] and changes it with [`update`], which writes the file at once. Without [`init`]
//! (a view test that doesn't ask for it) the state lives in memory only.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use agentdeck_core::RepositoryId;
use gpui::{App, Global};
use serde::{Deserialize, Serialize};

/// The file's name in the GUI's state dir.
pub const FILE: &str = "gui.json";

/// What the GUI remembers of itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiState {
    /// The Repository groups collapsed, per view.
    pub collapsed: Collapsed,
}

/// The Repository groups collapsed in each view that groups PRs by Repository. A group is
/// open unless named here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Collapsed {
    /// On the Pull requests view.
    pub pull_requests: BTreeSet<RepositoryId>,
}

impl Collapsed {
    /// Collapses `repository`'s group on the Pull requests view, or opens it.
    pub fn set_pull_requests(&mut self, repository: RepositoryId, collapsed: bool) {
        if collapsed {
            self.pull_requests.insert(repository);
        } else {
            self.pull_requests.remove(&repository);
        }
    }
}

/// The state in `text`; the defaults for anything it doesn't hold, or when it isn't JSON.
pub fn parse(text: &str) -> GuiState {
    serde_json::from_str(text).unwrap_or_else(|err| {
        tracing::warn!("{FILE} isn't the GUI's state, so it starts afresh: {err}");
        GuiState::default()
    })
}

/// Reads `path`; a missing or unreadable file is the defaults.
pub fn load(path: &Path) -> GuiState {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(_) => GuiState::default(),
    }
}

/// Writes `state` to `path` through a temp file, so a crash mid-write leaves the old file.
pub fn save(path: &Path, state: &GuiState) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, path)
}

/// The state, and where it is saved.
struct Store {
    /// `None`: memory only.
    path: Option<PathBuf>,
    state: GuiState,
}

impl Global for Store {}

/// Reads the state from `path` and saves every change there from now on; with no `path`, it
/// is kept in memory only.
pub fn init(path: Option<PathBuf>, cx: &mut App) {
    let state = path.as_deref().map(load).unwrap_or_default();
    cx.set_global(Store { path, state });
}

/// What `f` makes of the state.
pub fn read<R>(cx: &App, f: impl FnOnce(&GuiState) -> R) -> R {
    match cx.try_global::<Store>() {
        Some(store) => f(&store.state),
        None => f(&GuiState::default()),
    }
}

/// Changes the state with `f`, then saves it. A failed write is logged, not shown: the next
/// start just reads the defaults for what it lost.
pub fn update(cx: &mut App, f: impl FnOnce(&mut GuiState)) {
    if !cx.has_global::<Store>() {
        init(None, cx);
    }
    let store = cx.global_mut::<Store>();
    f(&mut store.state);
    if let Some(path) = &store.path
        && let Err(err) = save(path, &store.state)
    {
        tracing::warn!("saving the GUI's state to {}: {err}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    fn collapsed(ids: &[u64]) -> GuiState {
        GuiState {
            collapsed: Collapsed {
                pull_requests: ids.iter().copied().map(RepositoryId::from).collect(),
            },
        }
    }

    mod parse {
        use super::*;

        #[test]
        fn should_read_the_collapsed_groups_by_repository_id() {
            let state = parse(r#"{"collapsed":{"pull_requests":[1296269,42]}}"#);

            assert_eq!(state, collapsed(&[42, 1296269]));
        }

        #[test]
        fn should_default_what_the_file_leaves_out() {
            assert_eq!(parse("{}"), GuiState::default());
            assert_eq!(parse(r#"{"collapsed":{}}"#), GuiState::default());
        }

        #[test]
        fn should_skip_what_a_later_version_adds() {
            let state =
                parse(r#"{"show_finished":true,"collapsed":{"pull_requests":[7],"ws":{}}}"#);

            assert_eq!(state, collapsed(&[7]));
        }

        #[test]
        fn should_start_afresh_from_a_broken_file() {
            assert_eq!(parse("{not json"), GuiState::default());
            assert_eq!(
                parse(r#"{"collapsed":{"pull_requests":"x"}}"#),
                GuiState::default()
            );
        }
    }

    mod save {
        use super::*;

        #[test]
        fn should_be_read_back_by_load() {
            let dir = tempfile::tempdir().expect("a temp dir");
            let path = dir.path().join("agentdeck").join(FILE);

            save(&path, &collapsed(&[7, 9])).expect("save");

            assert_eq!(load(&path), collapsed(&[7, 9]));
        }

        #[test]
        fn should_load_the_defaults_without_a_file() {
            let dir = tempfile::tempdir().expect("a temp dir");

            assert_eq!(load(&dir.path().join(FILE)), GuiState::default());
        }
    }

    mod store {
        use super::*;

        #[gpui::test]
        fn should_write_each_change_for_the_next_start(cx: &mut TestAppContext) {
            let dir = tempfile::tempdir().expect("a temp dir");
            let path = dir.path().join(FILE);
            cx.update(|cx| init(Some(path.clone()), cx));

            cx.update(|cx| {
                update(cx, |state| {
                    state
                        .collapsed
                        .set_pull_requests(RepositoryId::from(7), true)
                })
            });

            assert_eq!(load(&path), collapsed(&[7]));
            cx.update(|cx| init(Some(path.clone()), cx));
            let read_back = cx.read(|cx| read(cx, Clone::clone));
            assert_eq!(read_back, collapsed(&[7]));
        }

        #[gpui::test]
        fn should_keep_changes_in_memory_without_a_file(cx: &mut TestAppContext) {
            cx.update(|cx| {
                update(cx, |state| {
                    state
                        .collapsed
                        .set_pull_requests(RepositoryId::from(7), true)
                })
            });

            let state = cx.read(|cx| read(cx, Clone::clone));
            assert_eq!(state, collapsed(&[7]));
        }
    }
}
