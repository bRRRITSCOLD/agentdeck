//! Starting an exited pane's process again in place (spec §4, §6): its agent's session
//! resumed, its agent afresh with a new session, or a shell, and where each starts
//! ([`place`]). A Claude Code
//! or Codex resume, by `POST /v1/panes/{id}/respawn` or after a restart, that exits non-zero
//! within [`FAILED_RESUME_WITHIN`] is taken for a session the agent can no longer find, and
//! the agent starts afresh, once (#237).

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agentdeck_core::ws::{Entity, ServerMessage};
use agentdeck_core::{Agent, AgentState, PaneId, WorkspaceId};
use tokio::sync::broadcast::{self, error::RecvError};

use super::{codex, launch};
use crate::db::{self, Db, DbError};
use crate::events::Events;
use crate::model::Pane;
use crate::terminal::Terminals;
use crate::terminal::env::default_shell;
use crate::terminal::pane::{Launch, SpawnError};

/// How soon after starting a resumed agent may exit non-zero for the daemon to start it
/// afresh instead. Claude Code prints `No conversation found with session ID: <id>` for a
/// session it can't find and exits 1 at once; Codex (codex-cli 0.169.0, #262) draws its TUI's
/// first frame, prints `ERROR: No saved session found with ID <id>. …` and exits 1 within a
/// second too. The exit counts, not the message, which either CLI may reword.
pub const FAILED_RESUME_WITHIN: Duration = Duration::from_secs(10);

/// What a respawn runs in a pane, decided from its row.
#[derive(Debug, PartialEq, Eq)]
pub enum Respawn {
    /// Claude Code's or Codex's resume command on its session: the row's `argv` and
    /// `session_id` stay.
    Resume(Agent, Vec<String>),
    /// An `other` agent's argv, run again: the row's `argv` stays.
    Again(Agent, Vec<String>),
    /// Claude Code or Codex with no known session: a fresh launch, as a new pane's.
    Fresh(Agent),
    /// A shell, which the pane becomes.
    Shell,
}

/// Where a respawn of a pane launched in `launched_in` (its `cwd`), whose last known folder
/// is `folder`, starts, and what it runs there (spec §4):
/// - a resume runs where its session started, `launched_in`: Claude Code finds a session by
///   the folder it started in, and Codex, which finds one anywhere, asks which folder to
///   work in when it isn't the one the session recorded. Once `launched_in` is gone, the
///   session can't be resumed there, so the agent starts afresh where a fresh start does;
/// - anything else (a fresh start, an `other` agent again, a shell) starts in `folder`, else
///   `launched_in`, else `home`.
///
/// `None` when none of them is an existing directory.
pub fn place(
    respawn: Respawn,
    launched_in: &Path,
    folder: &Path,
    home: Option<&Path>,
) -> Option<(Respawn, PathBuf)> {
    let elsewhere = || existing_dir([Some(folder), Some(launched_in), home]);
    match respawn {
        Respawn::Resume(agent, argv) => match existing_dir([Some(launched_in)]) {
            Some(dir) => Some((Respawn::Resume(agent, argv), dir)),
            None => Some((Respawn::Fresh(agent), elsewhere()?)),
        },
        respawn => Some((respawn, elsewhere()?)),
    }
}

/// The first of `folders` that is an existing absolute directory.
pub fn existing_dir<'a>(folders: impl IntoIterator<Item = Option<&'a Path>>) -> Option<PathBuf> {
    folders
        .into_iter()
        .flatten()
        .find(|dir| dir.is_absolute() && dir.is_dir())
        .map(Path::to_path_buf)
}

/// The daemon's `$HOME`, which a pane's environment passes on too.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// Why a pane couldn't start again.
#[derive(Debug)]
pub enum RestartError {
    /// Reading or writing its row.
    Db(DbError),
    /// Its program; the pane stays exited.
    Spawn(SpawnError),
}

impl fmt::Display for RestartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RestartError::Db(err) => err.fmt(f),
            RestartError::Spawn(err) => f.write_str(&err.detail()),
        }
    }
}

impl From<DbError> for RestartError {
    fn from(err: DbError) -> Self {
        RestartError::Db(err)
    }
}

/// Starts panes' processes again. Clones share everything.
#[derive(Clone)]
pub struct Restarter {
    /// The database actor.
    pub db: Db,
    /// The live panes.
    pub terminals: Terminals,
    /// The events every WebSocket client gets.
    pub events: Events,
}

impl Restarter {
    /// Runs `respawn` in exited pane `id`, in `cwd` ([`place`]), and returns its row as it is
    /// now. `cwd` becomes its `cwd` and its `folder`, and its state starts over; `changed`
    /// goes out for it. A resume that fails at once is followed by a fresh start
    /// ([`Restarter::fall_back_if_resume_fails`]).
    pub async fn respawn(
        &self,
        id: PaneId,
        respawn: Respawn,
        cwd: PathBuf,
    ) -> Result<Pane, RestartError> {
        let (agent, argv, resumed) = match respawn {
            Respawn::Fresh(agent) => return self.fresh(id, agent, cwd).await,
            Respawn::Shell => (None, vec![default_shell()], None),
            Respawn::Resume(agent, argv) => (Some(agent), argv, Some(agent)),
            Respawn::Again(agent, argv) => (Some(agent), argv, None),
        };
        let shell = agent.is_none().then(|| argv.clone());
        let folder = cwd.to_string_lossy().into_owned();
        let (pane, workspace) = self
            .db
            .call(move |c| {
                if let Some(shell) = shell {
                    db::panes::become_shell(c, id, &shell)?;
                }
                db::panes::respawned(c, id, &folder, None)?;
                Ok((db::panes::get(c, id)?, db::panes::workspace_of(c, id)?))
            })
            .await?;
        let feed = self.events.subscribe();
        self.start(&pane, workspace, agent, argv, cwd.clone(), false)
            .await?;
        if let Some(agent) = resumed {
            self.fall_back_if_resume_fails(id, agent, cwd, feed);
        }
        Ok(pane)
    }

    /// Starts `agent` afresh in exited pane `id`, as a new pane starts: a new session, with
    /// no prompt.
    async fn fresh(&self, id: PaneId, agent: Agent, cwd: PathBuf) -> Result<Pane, RestartError> {
        let folder = cwd.to_string_lossy().into_owned();
        let (pane, workspace, argv) = self
            .db
            .call(move |c| {
                let workspace = db::panes::workspace_of(c, id)?;
                let launch = launch(agent).expect("claude and codex launch");
                let started = (launch.argv.as_slice(), launch.session_id.as_deref());
                db::panes::respawned(c, id, &folder, Some(started))?;
                Ok((db::panes::get(c, id)?, workspace, launch.argv))
            })
            .await?;
        self.start(&pane, workspace, Some(agent), argv, cwd, true)
            .await?;
        Ok(pane)
    }

    /// Spawns `argv` in `pane`, whose row is written, and tells clients. A `fresh` Codex has
    /// its session found. A program that can't start leaves the pane exited.
    async fn start(
        &self,
        pane: &Pane,
        workspace: WorkspaceId,
        agent: Option<Agent>,
        argv: Vec<String>,
        cwd: PathBuf,
        fresh: bool,
    ) -> Result<(), RestartError> {
        let id = pane.id;
        let codex_agent = agent.filter(|_| fresh);
        let launch = Launch::pane(id, workspace, argv, cwd);
        let codex_sessions = codex::sessions_before(codex_agent, &launch);
        let spawned = match agent {
            Some(agent) => self.terminals.spawn(id, launch, agent.as_str()),
            None => self.terminals.spawn_shell(id, launch),
        };
        if let Err(err) = spawned {
            tracing::warn!("pane {id} couldn't start again: {}", err.detail());
            let title = pane.title.clone();
            self.db
                .call(move |c| db::panes::set_exited(c, id, None, &title))
                .await?;
            self.events.changed(Entity::Pane, id);
            return Err(RestartError::Spawn(err));
        }
        tracing::info!("respawned pane {id}");
        self.events.changed(Entity::Pane, id);
        if let Some((dir, before)) = codex_sessions {
            codex::watch(
                self.db.clone(),
                self.terminals.clone(),
                self.events.clone(),
                id,
                dir,
                before,
            );
        }
        Ok(())
    }

    /// Starts `agent` afresh in pane `id`, in `cwd`, when the resume just spawned there exits
    /// non-zero within [`FAILED_RESUME_WITHIN`], as a resume of a session the agent can no
    /// longer find does. `feed`, subscribed before the spawn, carries its exit. The fresh
    /// start isn't watched, so a pane whose agent fails either way stays exited after it.
    /// Nothing starts for a pane closed or stopped meanwhile, or while the daemon stops.
    pub fn fall_back_if_resume_fails(
        &self,
        id: PaneId,
        agent: Agent,
        cwd: PathBuf,
        mut feed: broadcast::Receiver<ServerMessage>,
    ) {
        let restarter = self.clone();
        tokio::spawn(async move {
            let exit = tokio::time::timeout(FAILED_RESUME_WITHIN, restarter.exit(id, &mut feed));
            let Ok(Some(Some(code))) = exit.await else {
                return;
            };
            // A pane stopped on purpose (`POST /v1/panes/{id}/stop`) failed nothing.
            let live = (restarter.terminals.get(id)).is_some_and(|pane| !pane.stopped());
            if code == 0 || !live || restarter.terminals.is_closing() {
                return;
            }
            let session = restarter
                .db
                .call(move |c| db::panes::get(c, id))
                .await
                .ok()
                .and_then(|pane| pane.session_id)
                .unwrap_or_default();
            tracing::info!(
                "pane {id}'s {agent} exited {code} within {} s of resuming session {session}, \
                 which it may no longer find: starting it afresh",
                FAILED_RESUME_WITHIN.as_secs()
            );
            match restarter.fresh(id, agent, cwd).await {
                Ok(_) | Err(RestartError::Db(DbError::NotFound(_))) => {}
                Err(err) => tracing::warn!("pane {id} couldn't start afresh: {err}"),
            }
        });
    }

    /// Pane `id`'s exit code once its process exits, from `feed`; `None` when the feed ends.
    /// A feed that fell behind may have dropped the exit, so the pane itself is asked then.
    async fn exit(
        &self,
        id: PaneId,
        feed: &mut broadcast::Receiver<ServerMessage>,
    ) -> Option<Option<i64>> {
        loop {
            match feed.recv().await {
                Ok(ServerMessage::Exit { pane, code }) if pane == id => return Some(code),
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {
                    let status = self.terminals.get(id)?.status();
                    if status.state == AgentState::Exited {
                        return Some(status.exit_code);
                    }
                }
                Err(RecvError::Closed) => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod place {
        use super::*;

        /// A launch folder, a last known folder and a home, each a real dir.
        struct Dirs {
            launched_in: tempfile::TempDir,
            folder: tempfile::TempDir,
            home: tempfile::TempDir,
        }

        impl Dirs {
            fn new() -> Dirs {
                let dir = || tempfile::tempdir().expect("a temp dir");
                Dirs {
                    launched_in: dir(),
                    folder: dir(),
                    home: dir(),
                }
            }

            fn place(&self, respawn: Respawn) -> Option<(Respawn, PathBuf)> {
                place(
                    respawn,
                    self.launched_in.path(),
                    self.folder.path(),
                    Some(self.home.path()),
                )
            }
        }

        fn resume() -> Respawn {
            Respawn::Resume(Agent::Claude, vec!["claude".into(), "--resume".into()])
        }

        #[test]
        fn should_resume_in_the_launch_folder_not_the_last_known_one() {
            let dirs = Dirs::new();

            let placed = dirs.place(resume());

            assert_eq!(placed, Some((resume(), dirs.launched_in.path().into())));
        }

        #[test]
        fn should_start_afresh_in_the_last_known_folder_once_the_launch_folder_is_gone() {
            let dirs = Dirs::new();
            let gone = dirs.launched_in.path().join("gone");

            let placed = place(resume(), &gone, dirs.folder.path(), Some(dirs.home.path()));

            assert_eq!(
                placed,
                Some((Respawn::Fresh(Agent::Claude), dirs.folder.path().into()))
            );
        }

        #[test]
        fn should_start_a_shell_in_the_last_known_folder() {
            let dirs = Dirs::new();

            let placed = dirs.place(Respawn::Shell);

            assert_eq!(placed, Some((Respawn::Shell, dirs.folder.path().into())));
        }

        #[test]
        fn should_start_afresh_in_the_last_known_folder() {
            let dirs = Dirs::new();

            let placed = dirs.place(Respawn::Fresh(Agent::Codex));

            assert_eq!(
                placed,
                Some((Respawn::Fresh(Agent::Codex), dirs.folder.path().into()))
            );
        }

        #[test]
        fn should_fall_back_to_the_launch_folder_once_the_last_known_one_is_gone() {
            let dirs = Dirs::new();
            let gone = dirs.folder.path().join("gone");

            let placed = place(
                Respawn::Shell,
                dirs.launched_in.path(),
                &gone,
                Some(dirs.home.path()),
            );

            assert_eq!(
                placed,
                Some((Respawn::Shell, dirs.launched_in.path().into()))
            );
        }

        #[test]
        fn should_fall_back_to_home_once_both_are_gone() {
            let dirs = Dirs::new();
            let gone = dirs.home.path().join("gone");

            let placed = place(Respawn::Shell, &gone, &gone, Some(dirs.home.path()));

            assert_eq!(placed, Some((Respawn::Shell, dirs.home.path().into())));
        }

        #[test]
        fn should_be_none_once_every_folder_is_gone() {
            let gone = Path::new("/no/such/dir");

            assert_eq!(place(resume(), gone, gone, None), None);
        }

        #[test]
        fn should_skip_a_relative_folder() {
            let dirs = Dirs::new();

            let placed = place(
                Respawn::Shell,
                dirs.launched_in.path(),
                Path::new("."),
                None,
            );

            assert_eq!(
                placed,
                Some((Respawn::Shell, dirs.launched_in.path().into()))
            );
        }
    }
}
