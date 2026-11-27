//! The daemon's terminals (spec §5–§7): one PTY, process and `alacritty_terminal::Term` per
//! live pane.
//!
//! - [`Terminals`]: the live panes by id. It spawns and kills them, runs the state timer,
//!   and persists state changes, exits, the agents shell panes host and the folders panes
//!   move to.
//! - [`pane`]: one live pane. Its `Term` is the only thing that answers the program's
//!   terminal queries.
//! - [`fanout`]: a pane's attached WebSocket clients: output, `lagged`, the primary rule.
//! - [`snapshot`]: the `Term` re-encoded for an attaching client.
//! - [`env`](mod@env): a pane's environment, from an allowlist.
//! - [`rules`]: which state rules a pane runs.
//! - [`tail`], [`palette`]: the raw output tail and the colours queries are answered with.

pub mod env;
pub mod fanout;
pub mod palette;
pub mod pane;
pub mod rules;
pub mod snapshot;
pub mod tail;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use agentdeck_core::state::HOSTABLE;
use agentdeck_core::ws::Entity;
use agentdeck_core::{Agent, AgentState, PaneId};
use nix::sys::signal::Signal;
use tokio::runtime::Handle;

use self::fanout::SubscriberId;
use self::pane::{Ended, Launch, Left, NothingToStop, Observed, Pane, Rules, SpawnError};
use crate::agents::hosted;
use crate::db::{self, Db};
use crate::events::Events;
use crate::model::LastHosted;

/// How often each pane's screen is observed for its state. Settling takes 400 ms by
/// default, so a state publishes within this of having settled.
const TICK: Duration = Duration::from_millis(100);

/// How long a killed pane gets between SIGHUP and SIGKILL (spec §4).
const KILL_GRACE: Duration = Duration::from_secs(3);

/// The live panes. Clones share them.
#[derive(Clone)]
pub struct Terminals {
    shared: Arc<Shared>,
}

struct Shared {
    panes: Mutex<HashMap<PaneId, Arc<Pane>>>,
    db: Db,
    events: Events,
    rules_dir: PathBuf,
    origin: Instant,
    runtime: Handle,
    /// Set at shutdown: processes dying with the daemon aren't recorded as exited, so they
    /// can be resumed (spec §6).
    closing: AtomicBool,
    next_subscriber: AtomicU64,
}

impl Terminals {
    /// No panes yet. Must be called inside the tokio runtime; rule files are read from
    /// `rules_dir` (`$XDG_CONFIG_HOME/agentdeck/rules`).
    pub fn new(db: Db, events: Events, rules_dir: PathBuf) -> Self {
        Self {
            shared: Arc::new(Shared {
                panes: Mutex::new(HashMap::new()),
                db,
                events,
                rules_dir,
                origin: Instant::now(),
                runtime: Handle::current(),
                closing: AtomicBool::new(false),
                next_subscriber: AtomicU64::new(1),
            }),
        }
    }

    fn panes(&self) -> MutexGuard<'_, HashMap<PaneId, Arc<Pane>>> {
        self.shared
            .panes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Starts `launch` as agent pane `id`, whose row the caller has already written, with the
    /// state rules named `rules` (its agent's name), for good. A live pane `id` that has
    /// exited is replaced, as respawning it does.
    pub fn spawn(&self, id: PaneId, launch: Launch, rules: &str) -> Result<(), SpawnError> {
        let rules = Rules::Fixed(rules::load(&self.shared.rules_dir, rules));
        self.start(id, launch, rules)
    }

    /// Starts `launch` as shell pane `id`, whose row the caller has already written: the
    /// `shell` rules, or the rules of the agent it hosts while one is its foreground process
    /// (spec §7).
    pub fn spawn_shell(&self, id: PaneId, launch: Launch) -> Result<(), SpawnError> {
        let dir = &self.shared.rules_dir;
        let rules = Rules::Shell {
            shell: rules::load(dir, "shell"),
            agents: (HOSTABLE.iter())
                .map(|agent| (*agent, rules::load(dir, agent.as_str())))
                .collect(),
        };
        self.start(id, launch, rules)
    }

    fn start(&self, id: PaneId, launch: Launch, rules: Rules) -> Result<(), SpawnError> {
        let shared = Arc::clone(&self.shared);
        let on_exit = Box::new(move |id, ended: Ended| shared.persist_exit(id, ended));
        let pane = Pane::spawn(id, launch, rules, self.shared.events.clone(), on_exit)?;
        self.panes().insert(id, pane);
        Ok(())
    }

    /// The live pane `id`, exited or not, until it is killed.
    pub fn get(&self, id: PaneId) -> Option<Arc<Pane>> {
        self.panes().get(&id).cloned()
    }

    /// Kills pane `id` (SIGHUP, then SIGKILL after 3 s) and forgets it. False when no such
    /// pane is live.
    pub fn kill(&self, id: PaneId) -> bool {
        let Some(pane) = self.panes().remove(&id) else {
            return false;
        };
        pane.signal(Signal::SIGHUP);
        self.shared.runtime.spawn(async move {
            tokio::time::sleep(KILL_GRACE).await;
            pane.signal(Signal::SIGKILL);
        });
        true
    }

    /// Stops pane `id`'s agent and keeps the pane (spec §4): SIGHUP to the agent pane's process,
    /// or to the agent a shell pane hosts, then SIGKILL after 3 s to whatever of it is left. The
    /// pane's exit is recorded as any other; a shell pane's shell keeps running.
    pub fn stop(&self, id: PaneId) -> Result<(), NothingToStop> {
        let pane = self.get(id).ok_or(NothingToStop::Exited)?;
        pane.stop(Signal::SIGHUP)?;
        self.shared.runtime.spawn(async move {
            tokio::time::sleep(KILL_GRACE).await;
            let _ = pane.stop(Signal::SIGKILL);
        });
        Ok(())
    }

    /// Kills every live pane whose row is gone, as after deleting its tab or workspace.
    pub async fn reap(&self) -> Result<(), db::DbError> {
        let rows: HashSet<PaneId> = self
            .shared
            .db
            .call(|c| db::panes::ids(c))
            .await?
            .into_iter()
            .collect();
        let orphans: Vec<PaneId> = self
            .panes()
            .keys()
            .filter(|id| !rows.contains(id))
            .copied()
            .collect();
        for id in orphans {
            self.kill(id);
        }
        Ok(())
    }

    /// A fresh id for one client attachment.
    pub fn subscriber(&self) -> SubscriberId {
        SubscriberId(self.shared.next_subscriber.fetch_add(1, Ordering::Relaxed))
    }

    /// Observes every pane's screen each tick, even when it prints nothing, so a state
    /// publishes once it has settled. Runs until the runtime stops.
    pub fn run_state_timer(&self) {
        let terminals = self.clone();
        self.shared.runtime.spawn(async move {
            let mut ticks = tokio::time::interval(TICK);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticks.tick().await;
                terminals.observe_all().await;
            }
        });
    }

    async fn observe_all(&self) {
        let now = self.shared.origin.elapsed();
        let panes: Vec<Arc<Pane>> = self.panes().values().cloned().collect();
        for pane in panes {
            let Observed {
                state,
                hosted,
                folder,
                left,
            } = pane.observe(now);
            let id = pane.id();
            if let Some(folder) = folder {
                self.shared.save_folder(id, &folder).await;
            }
            if let Some((state, title)) = state {
                let saved = self
                    .shared
                    .db
                    .call(move |c| db::panes::set_state(c, id, state, &title))
                    .await;
                if let Err(err) = saved {
                    tracing::warn!("saving pane {id}'s state: {err}");
                }
            }
            // Ahead of `hosted`, so the `changed` it sends carries the agent that left.
            if let Some(left) = left {
                self.left(&pane, left).await;
            }
            if let Some(hosted) = hosted {
                self.hosted(id, hosted).await;
            }
        }
    }

    /// Reads live pane `id`'s current folder now and, when it moved, saves it and tells
    /// clients (spec §6). Nothing for a pane this daemon doesn't hold.
    pub async fn refresh_folder(&self, id: PaneId) {
        let moved = self.get(id).and_then(|pane| pane.refresh_folder());
        if let Some(folder) = moved {
            self.shared.save_folder(id, &folder).await;
        }
    }

    /// Looks for the session of the agent that just `left` live shell `pane` and records it
    /// as the agent the pane hosted last (spec §7). The files are read off the runtime.
    async fn left(&self, pane: &Pane, left: Left) {
        let Left {
            hosting,
            run,
            homes,
        } = left;
        let claimed = match self.shared.db.call(|c| db::panes::session_ids(c)).await {
            Ok(claimed) => claimed,
            Err(err) => {
                tracing::warn!("reading the sessions panes own: {err}");
                HashSet::new()
            }
        };
        let agent = run.agent;
        let looking = tokio::task::spawn_blocking(move || hosted::session(&run, &homes, &claimed));
        let session_id = looking.await.ok().flatten();
        let last = LastHosted {
            agent,
            session_id,
            left_at: db::now(),
        };
        if pane.set_last_hosted(hosting, last) {
            tracing::info!("pane {} hosted {agent} last", pane.id());
        }
    }

    /// Saves the agent shell pane `id` hosts now, then tells clients (spec §7).
    async fn hosted(&self, id: PaneId, agent: Option<Agent>) {
        let saved = self
            .shared
            .db
            .call(move |c| db::panes::set_hosted_agent(c, id, agent))
            .await;
        if let Err(err) = saved {
            tracing::warn!("saving the agent pane {id} hosts: {err}");
            return;
        }
        match agent {
            Some(agent) => tracing::info!("pane {id} hosts {agent}"),
            None => tracing::info!("pane {id} hosts no agent"),
        }
        self.shared.events.changed(Entity::Pane, id);
    }

    /// The daemon is stopping: from now on, processes that die aren't recorded as exited, so
    /// they are resumed (spec §6). An agent pane stopped on purpose whose process hasn't
    /// exited yet, inside its kill grace, is recorded as exited now instead, so it isn't.
    /// Each live pane's current folder is read and saved first, while its process still runs,
    /// so a resumed pane starts where it was.
    pub async fn close(&self) {
        let live: Vec<PaneId> = self.panes().keys().copied().collect();
        for id in live {
            self.refresh_folder(id).await;
        }
        self.shared.closing.store(true, Ordering::SeqCst);
        let stopped: Vec<(PaneId, String)> = (self.panes().values())
            .filter(|pane| pane.stopped())
            .map(|pane| (pane.id(), pane.status()))
            .filter(|(_, status)| status.state != AgentState::Exited)
            .map(|(id, status)| (id, status.title))
            .collect();
        for (id, title) in stopped {
            let saved = (self.shared.db)
                .call(move |c| db::panes::set_stopped(c, id, &title))
                .await;
            match saved {
                Ok(()) => tracing::info!("pane {id} was stopped: saved as exited"),
                Err(err) => tracing::warn!("saving stopped pane {id} as exited: {err}"),
            }
        }
    }

    /// Whether the daemon is stopping ([`Terminals::close`]).
    pub fn is_closing(&self) -> bool {
        self.shared.closing.load(Ordering::SeqCst)
    }
}

impl Shared {
    /// Records an exit, and returns once it is saved: the pane reads as exited only after
    /// this, so the API never reports an exit that archiving can't yet see (spec §7). Called
    /// on the pane's reader thread, which is outside the runtime and may block.
    fn persist_exit(&self, id: PaneId, ended: Ended) {
        if self.closing.load(Ordering::SeqCst) {
            return;
        }
        let Ended {
            code,
            title,
            folder,
        } = ended;
        let saved = self.runtime.block_on(
            self.db
                .call(move |c| db::panes::set_exited(c, id, code, &title)),
        );
        if let Err(err) = saved {
            tracing::warn!("saving pane {id}'s exit: {err}");
        }
        if let Some(folder) = folder {
            self.runtime.block_on(self.save_folder(id, &folder));
        }
    }

    /// Saves `folder` as pane `id`'s last known folder, then tells clients (spec §6).
    async fn save_folder(&self, id: PaneId, folder: &Path) {
        let folder = folder.to_string_lossy().into_owned();
        let saved = (self.db)
            .call(move |c| db::panes::set_folder(c, id, Some(&folder)))
            .await;
        match saved {
            Ok(()) => self.events.changed(Entity::Pane, id),
            Err(err) => tracing::warn!("saving pane {id}'s folder: {err}"),
        }
    }
}
