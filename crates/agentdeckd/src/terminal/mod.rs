//! The daemon's terminals (spec §5–§7): one PTY, process and `alacritty_terminal::Term` per
//! live pane.
//!
//! - [`Terminals`]: the live panes by id. It spawns and kills them, runs the state timer,
//!   and persists state changes, exits and the agents shell panes host.
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
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use agentdeck_core::state::HOSTABLE;
use agentdeck_core::ws::Entity;
use agentdeck_core::{Agent, PaneId};
use nix::sys::signal::Signal;
use tokio::runtime::Handle;
use tokio::sync::broadcast;

use self::fanout::SubscriberId;
use self::pane::{Launch, Observed, Pane, Rules, SpawnError};
use crate::db::{self, Db};
use crate::events::Events;

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
    /// Each shell pane that just started hosting an agent, once that is saved.
    hosting: broadcast::Sender<PaneId>,
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
                hosting: broadcast::channel(64).0,
            }),
        }
    }

    fn panes(&self) -> MutexGuard<'_, HashMap<PaneId, Arc<Pane>>> {
        self.shared
            .panes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Starts `launch` as pane `id`, whose row the caller has already written, with the
    /// state rules named `rules` (an agent's name, or `shell` for a resumed agent pane that
    /// comes back as a shell), for good.
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
        let on_exit = Box::new(move |id, code, title: String| shared.persist_exit(id, code, title));
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

    /// Tells of each shell pane that starts hosting an agent, once its row says so.
    pub fn hosting(&self) -> broadcast::Receiver<PaneId> {
        self.shared.hosting.subscribe()
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
            let Observed { state, hosted } = pane.observe(now);
            let id = pane.id();
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
            if let Some(hosted) = hosted {
                self.hosted(id, hosted).await;
            }
        }
    }

    /// Saves the agent shell pane `id` hosts now, then tells clients, and the comment delivery
    /// when it started hosting one (spec §7, §9).
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
        if agent.is_some() {
            // Nobody listening (no delivery yet, at start) is fine: nothing waits on it.
            let _ = self.shared.hosting.send(id);
        }
    }

    /// The daemon is stopping: from now on, processes that die aren't recorded as exited.
    pub fn close(&self) {
        self.shared.closing.store(true, Ordering::SeqCst);
    }
}

impl Shared {
    /// Records an exit, and returns once it is saved: the pane reads as exited only after
    /// this, so the API never reports an exit that archiving can't yet see (spec §7). Called
    /// on the pane's reader thread, which is outside the runtime and may block.
    fn persist_exit(&self, id: PaneId, code: Option<i64>, title: String) {
        if self.closing.load(Ordering::SeqCst) {
            return;
        }
        let saved = self.runtime.block_on(
            self.db
                .call(move |c| db::panes::set_exited(c, id, code, &title)),
        );
        if let Err(err) = saved {
            tracing::warn!("saving pane {id}'s exit: {err}");
        }
    }
}
