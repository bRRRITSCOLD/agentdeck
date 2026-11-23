//! Starting an exited pane's process again in place (spec §4, §6): its agent's session
//! resumed, its agent afresh with a new session, or a shell. A Claude Code
//! or Codex resume, by `POST /v1/panes/{id}/respawn` or after a restart, that exits non-zero
//! within [`FAILED_RESUME_WITHIN`] is taken for a session the agent can no longer find, and
//! the agent starts afresh, once (#237).

use std::fmt;
use std::path::PathBuf;
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
pub enum Respawn {
    /// Its agent's resume command on its session, or an `other` agent's argv: the row's
    /// `argv` and `session_id` stay.
    Again(Agent, Vec<String>),
    /// Claude Code or Codex with no known session: a fresh launch, as a new pane's.
    Fresh(Agent),
    /// A shell, which the pane becomes.
    Shell,
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
    /// Runs `respawn` in exited pane `id`, in `cwd`, and returns its row as it is now. Its
    /// state starts over; `changed` goes out for it. A resume that fails at once is followed
    /// by a fresh start ([`Restarter::fall_back_if_resume_fails`]).
    pub async fn respawn(
        &self,
        id: PaneId,
        respawn: Respawn,
        cwd: PathBuf,
    ) -> Result<Pane, RestartError> {
        let (agent, argv) = match respawn {
            Respawn::Fresh(agent) => return self.fresh(id, agent, cwd).await,
            Respawn::Shell => (None, vec![default_shell()]),
            Respawn::Again(agent, argv) => (Some(agent), argv),
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
        // `Again` for Claude Code or Codex is always a resume of its session.
        let resumed = agent.filter(|agent| *agent != Agent::Other);
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
        let launch = Launch::pane(id, workspace, pane.task_id, argv, cwd);
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
