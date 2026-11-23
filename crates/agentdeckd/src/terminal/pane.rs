//! One live pane: its PTY and process, the daemon's `Term` for it, and everything the
//! daemon derives from the output (spec §5–§7).
//!
//! Three OS threads serve each pane, since `portable-pty`'s reads and writes block:
//! - the **reader** feeds every chunk of output through [`Pane::output`] until the PTY closes;
//! - the **waiter** waits for the process, not for the PTY, since a child it leaves running
//!   keeps the PTY open. Once the process has exited it gives the reader [`EXIT_GRACE`] to
//!   reach the end of the output, records the exit and only then reports it, then hangs up on
//!   whatever is left in the pane's session or still holds its PTY ([`hang_up`]), and kills
//!   what holds the PTY [`EXIT_GRACE`] after that, so the reader always ends;
//! - the **writer** writes input in order: typed bytes, delivered `input`, and the `Term`'s
//!   answers to terminal queries. A submitted message's Enter is a write of its own, with a
//!   pause on either side ([`SUBMIT_PAUSE`]).
//!
//! When the process exits, or the agent a shell pane hosts leaves the foreground, the pane
//! turns off the terminal modes a killed program leaves on ([`reset_modes`]).
//!
//! A pane knows its current folder ([`Pane::refresh_folder`], spec §6): the folder its shell
//! last reported with OSC 7, else its process's working directory from `/proc/<pid>/cwd`, else
//! the one it last knew, which starts as the folder it launched in. It reads it again on the
//! state timer's tick after it printed, at most once every [`FOLDER_EVERY`] while it keeps
//! printing (a `cd` ends with a prompt), never while it is quiet; and whenever the daemon asks.
//!
//! Everything a chunk of output touches (the `Term`, the tail, the OSC scanner, the attached
//! clients' queues) changes under one lock, [`Pane::live`]. An attach snapshots the `Term`
//! and subscribes under that same lock, so no output falls between the snapshot and the live
//! bytes, and none arrives twice. Nothing under the lock blocks: input goes to the writer
//! thread through a channel, and clients' queues are unbounded (their size is capped by the
//! `lagged` rule instead).

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use agentdeck_core::state::osc::file_uri;
use agentdeck_core::state::{
    Foreground, LastOutput, Observation, OscScanner, RuleSet, StateTracker, hosted_agent, screen,
};
use agentdeck_core::ws::{Channel, RESET_MODES, ServerMessage};
use agentdeck_core::{Agent, AgentState, PaneId, TaskId, WorkspaceId};
use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use super::env::{daemon_dir, pane_env};
use super::fanout::{Fanout, Outbox, Outgoing, ResizeDecision, Size, SubscriberId};
use super::tail::{TAIL_CAPACITY, Tail};
use super::{palette, snapshot};
use crate::agents::TASK_VAR;
use crate::events::Events;

/// What to run in a new pane.
#[derive(Debug, Clone)]
pub struct Launch {
    /// The program and its arguments; `argv[0]` is looked up on the env's `PATH`.
    pub argv: Vec<String>,
    /// The working directory; it must exist. The pane's folder until it moves.
    pub cwd: PathBuf,
    /// The whole environment: nothing else is inherited. Build it with
    /// [`pane_env`].
    pub env: BTreeMap<OsString, OsString>,
    /// The PTY's size until a client resizes it.
    pub size: Size,
}

/// A new pane's size until a client resizes it (spec §4).
const INITIAL_SIZE: Size = Size { cols: 80, rows: 24 };

impl Launch {
    /// Pane `id`, in `workspace`, linked to `task`, running `argv` in `cwd` at 80×24. Its
    /// environment is the allowlisted part of the daemon's, its `PATH` ending with the folder
    /// holding `agentdeckd`, plus `AGENTDECK_PANE`, `AGENTDECK_WORKSPACE` and, with a task,
    /// `AGENTDECK_TASK`.
    pub fn pane(
        id: PaneId,
        workspace: WorkspaceId,
        task: Option<TaskId>,
        argv: Vec<String>,
        cwd: PathBuf,
    ) -> Launch {
        let mut extra = vec![
            ("AGENTDECK_PANE".to_owned(), id.to_string()),
            ("AGENTDECK_WORKSPACE".to_owned(), workspace.to_string()),
        ];
        extra.extend(task.map(|task| (TASK_VAR.to_owned(), task.to_string())));
        Launch {
            argv,
            cwd,
            env: pane_env(std::env::vars_os(), daemon_dir().as_deref(), &extra),
            size: INITIAL_SIZE,
        }
    }
}

/// The state rules a pane runs (spec §7).
pub enum Rules {
    /// An agent pane's, or a resumed agent pane's shell's: the same rules for good.
    Fixed(RuleSet),
    /// A shell pane's: `shell` until an agent in `agents` is its foreground process, then that
    /// agent's while it is.
    Shell {
        /// The `shell` rules.
        shell: RuleSet,
        /// Each agent a shell can host, with its rules.
        agents: Vec<(Agent, RuleSet)>,
    },
}

/// A shell pane's rules for itself and for each agent it can host, and the agent it hosts.
struct Hosting {
    shell: RuleSet,
    agents: Vec<(Agent, RuleSet)>,
    hosted: Option<Agent>,
    /// Whether the modes were reset since the hosted agent handed the terminal back to the
    /// shell, ahead of the observation that it left ([`Pane::agent_left`]).
    modes_reset: bool,
}

impl Hosting {
    /// The agent `foreground` makes the pane host, with the rules to run, when that differs
    /// from the one it hosts now.
    fn change(&self, foreground: &Foreground) -> Option<(Option<Agent>, RuleSet)> {
        let hosted = hosted_agent(foreground, &self.agents);
        if hosted == self.hosted {
            return None;
        }
        let rules = hosted
            .and_then(|agent| self.agents.iter().find(|(a, _)| *a == agent))
            .map_or(&self.shell, |(_, rules)| rules);
        Some((hosted, rules.clone()))
    }
}

/// What one observation of a pane changed, for the daemon to save.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Observed {
    /// The newly published state, with the title it was seen under.
    pub state: Option<(AgentState, String)>,
    /// The agent the shell pane hosts now, when that changed.
    pub hosted: Option<Option<Agent>>,
    /// The pane's current folder, when it moved since it was last read.
    pub folder: Option<PathBuf>,
}

/// How often, at most, a pane that keeps printing has its folder read again (spec §6).
const FOLDER_EVERY: Duration = Duration::from_secs(1);

/// When `input` reaches the PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// At once.
    Now,
    /// Once the pane's state is idle, done or blocked; at once if it already is.
    Idle,
}

impl When {
    /// Whether input waiting for `self` goes in while the pane is in `state`.
    fn accepts(self, state: AgentState) -> bool {
        match self {
            When::Now => true,
            When::Idle => matches!(
                state,
                AgentState::Idle | AgentState::Done | AgentState::Blocked
            ),
        }
    }
}

/// How long the writer waits before and after the Enter that submits a message, so the
/// program reads the message, the Enter and whatever comes next in separate reads. Claude
/// Code and Codex take text arriving with its Enter in one read as a paste, and keep the Enter
/// in it as a newline instead of submitting (#184).
const SUBMIT_PAUSE: Duration = Duration::from_millis(200);

/// What the writer thread writes.
enum Typing {
    /// Bytes, as they are.
    Bytes(Vec<u8>),
    /// Enter (CR) in a read of its own: written after [`SUBMIT_PAUSE`], and followed by
    /// another before anything else is written.
    Enter,
}

/// What a message is typed as ahead of its Enter: one bracketed paste when the program turned
/// bracketed paste on (DECSET 2004), with any ESC removed so it can't end the paste early, as
/// the GUI pastes; else `text` as is.
fn message(text: &[u8], bracketed: bool) -> Vec<u8> {
    if !bracketed {
        return text.to_vec();
    }
    let mut bytes = b"\x1b[200~".to_vec();
    bytes.extend(text.iter().filter(|byte| **byte != 0x1b));
    bytes.extend_from_slice(b"\x1b[201~");
    bytes
}

/// Whether the program in `term` turned bracketed paste on.
fn bracketed_paste(term: &Term<Listener>) -> bool {
    term.mode().contains(TermMode::BRACKETED_PASTE)
}

/// Input waiting for its [`When`].
struct Queued {
    when: When,
    bytes: Vec<u8>,
    /// Typed as a message ([`message`]) followed by Enter, rather than as is.
    submit: bool,
}

/// Why a pane couldn't start.
#[derive(Debug)]
pub enum SpawnError {
    /// The PTY or the threads serving it couldn't be set up.
    Pty(anyhow::Error),
    /// `program`, a bare name, is in none of the `PATH`'s directories; `err` says where it
    /// looked.
    NotOnPath { program: String, err: anyhow::Error },
    /// The command couldn't be started: not executable, a directory, …
    Command(anyhow::Error),
}

impl fmt::Display for SpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpawnError::Pty(err) => write!(f, "setting up the PTY: {err:#}"),
            // The lookup's error lists every PATH directory; it goes to the log instead.
            SpawnError::NotOnPath { program, .. } => write!(
                f,
                "`{program}` isn't on agentdeckd's PATH. Install it, or start agentdeckd from a \
                 shell where it's found."
            ),
            SpawnError::Command(err) => write!(f, "{err:#}"),
        }
    }
}

impl SpawnError {
    /// Everything that went wrong, for the log: the lookup's whole error for
    /// [`SpawnError::NotOnPath`].
    pub fn detail(&self) -> String {
        match self {
            SpawnError::NotOnPath { err, .. } => format!("{err:#}"),
            other => other.to_string(),
        }
    }
}

/// Whether `program` is a bare name found in none of `path`'s directories, each read against
/// `cwd` as `portable-pty`'s lookup reads them. A program named by a path isn't looked up.
fn not_on_path(program: &str, path: Option<&OsStr>, cwd: &Path) -> bool {
    if program.contains('/') {
        return false;
    }
    path.is_none_or(|path| {
        !std::env::split_paths(path).any(|dir| cwd.join(dir).join(program).exists())
    })
}

/// The pane's process has exited, so it takes no input.
#[derive(Debug)]
pub struct Exited;

/// Why [`Pane::stop`] had nothing to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NothingToStop {
    /// The pane's process has exited.
    Exited,
    /// A shell pane hosting no agent: the shell isn't stopped.
    NoAgent,
}

/// What a pane shows right now.
#[derive(Debug, Clone)]
pub struct Status {
    /// The title the program set.
    pub title: String,
    /// The published state.
    pub state: AgentState,
    /// The exit code, once the process has exited and it could be read.
    pub exit_code: Option<i64>,
    /// The agent a shell pane hosts.
    pub hosted: Option<Agent>,
}

/// How a pane's process ended, for the daemon to save.
#[derive(Debug)]
pub struct Ended {
    /// Its exit code, when it could be read.
    pub code: Option<i64>,
    /// The title it last set.
    pub title: String,
    /// Its current folder, when its last OSC 7 moved it since it was last read.
    pub folder: Option<PathBuf>,
}

/// Called once when the pane's process exits, with how it ended, before the pane reads as
/// exited: the daemon saves the exit in it, so nothing sees `exited` before the pane's row
/// says so (spec §7).
pub type OnExit = Box<dyn FnOnce(PaneId, Ended) + Send>;

/// A live pane.
pub struct Pane {
    id: PaneId,
    live: Mutex<Live>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    input: Sender<Typing>,
    /// The process's id, which is also its process group's: it runs as a session leader.
    pid: Option<i32>,
    events: Events,
    /// The origin of the pane's own clock, for [`LastOutput`].
    started: Instant,
    /// Whether [`Pane::stop`] signalled the pane's own process, so its exit is no failed
    /// resume to start afresh from (spec §6).
    stopped: AtomicBool,
    /// Whether its process has been waited for, after which its pid may name another process.
    reaped: AtomicBool,
}

/// Everything one chunk of output changes, behind [`Pane::live`].
struct Live {
    term: Term<Listener>,
    term_events: Receiver<Event>,
    parser: Processor,
    osc: OscScanner,
    tail: Tail,
    title: String,
    size: Size,
    last_output: LastOutput,
    tracker: StateTracker,
    /// A shell pane's hosting; `None` for an agent pane.
    hosting: Option<Hosting>,
    /// `input` waiting for its state, in the order it came.
    queued: Vec<Queued>,
    exit_code: Option<i64>,
    fanout: Fanout,
    /// The last known folder (spec §6).
    folder: PathBuf,
    /// Whether it printed since its folder was last read on a tick.
    folder_due: bool,
    /// When its folder was last read on a tick, on the daemon's clock.
    folder_read_at: Option<Duration>,
}

/// Hands the `Term`'s events (titles, query answers) back to the pane after each chunk.
struct Listener(Sender<Event>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

impl Pane {
    /// Starts `launch` in a new PTY. `rules` derive the pane's state; `events` gets its
    /// `state` and `exit` events; `on_exit` runs once the process has exited, before they do.
    pub fn spawn(
        id: PaneId,
        launch: Launch,
        rules: Rules,
        events: Events,
        on_exit: OnExit,
    ) -> Result<Arc<Pane>, SpawnError> {
        let pair = native_pty_system()
            .openpty(pty_size(launch.size))
            .map_err(SpawnError::Pty)?;
        let mut command = CommandBuilder::from_argv(launch.argv.iter().map(Into::into).collect());
        command.env_clear();
        for (name, value) in &launch.env {
            command.env(name, value);
        }
        command.cwd(&launch.cwd);
        let mut child = pair.slave.spawn_command(command).map_err(|err| {
            let program = launch.argv.first().map_or("", String::as_str);
            let path = launch.env.get(OsStr::new("PATH")).map(OsString::as_os_str);
            if not_on_path(program, path, &launch.cwd) {
                SpawnError::NotOnPath {
                    program: program.to_owned(),
                    err,
                }
            } else {
                SpawnError::Command(err)
            }
        })?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(SpawnError::Pty)?;
        let mut writer = pair.master.take_writer().map_err(SpawnError::Pty)?;

        let (input, typed) = channel::<Typing>();
        let (term_events, term_events_rx) = channel();
        let (rules, hosting, last_output) = match rules {
            Rules::Fixed(rules) => (rules, None, LastOutput::agent()),
            Rules::Shell { shell, agents } => {
                let hosting = Hosting {
                    shell: shell.clone(),
                    agents,
                    hosted: None,
                    modes_reset: false,
                };
                (shell, Some(hosting), LastOutput::shell())
            }
        };
        let term = Term::new(
            Config::default(),
            &TermSize::new(launch.size.cols.into(), launch.size.rows.into()),
            Listener(term_events),
        );
        let pane = Arc::new(Pane {
            id,
            live: Mutex::new(Live {
                term,
                term_events: term_events_rx,
                parser: Processor::new(),
                osc: OscScanner::new(),
                tail: Tail::new(TAIL_CAPACITY),
                title: String::new(),
                size: launch.size,
                last_output,
                tracker: StateTracker::new(rules),
                hosting,
                queued: Vec::new(),
                exit_code: None,
                fanout: Fanout::default(),
                folder: launch.cwd.clone(),
                folder_due: false,
                folder_read_at: None,
            }),
            master: Mutex::new(pair.master),
            input,
            pid: child.process_id().and_then(|pid| i32::try_from(pid).ok()),
            events,
            started: Instant::now(),
            stopped: AtomicBool::new(false),
            reaped: AtomicBool::new(false),
        });

        let thread = |role: &str| std::thread::Builder::new().name(format!("pane-{role}-{id}"));
        thread("write")
            .spawn(move || {
                let mut type_in =
                    |bytes: &[u8]| writer.write_all(bytes).and_then(|()| writer.flush());
                for typing in typed {
                    let written = match typing {
                        Typing::Bytes(bytes) => type_in(&bytes),
                        Typing::Enter => {
                            std::thread::sleep(SUBMIT_PAUSE);
                            let written = type_in(b"\r");
                            std::thread::sleep(SUBMIT_PAUSE);
                            written
                        }
                    };
                    if written.is_err() {
                        break;
                    }
                }
            })
            .map_err(|err| SpawnError::Pty(err.into()))?;
        let (closed, pty_closed) = channel::<()>();
        let reading = Arc::clone(&pane);
        thread("read")
            .spawn(move || {
                let mut buf = vec![0; 64 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => reading.output(&buf[..n]),
                    }
                }
                let _ = closed.send(());
            })
            .map_err(|err| SpawnError::Pty(err.into()))?;
        let waiting = Arc::clone(&pane);
        thread("wait")
            .spawn(move || {
                let code = child
                    .wait()
                    .ok()
                    .map(|status| i64::from(status.exit_code()));
                waiting.reaped.store(true, Ordering::SeqCst);
                // A reader that ended without saying so is done reading too.
                let held = matches!(
                    pty_closed.recv_timeout(EXIT_GRACE),
                    Err(RecvTimeoutError::Timeout)
                );
                waiting.exit(code, on_exit);
                if held && let Some(pid) = waiting.pid {
                    tracing::info!(
                        "pane {}'s process exited but its PTY is still open: hanging up on \
                         its session and what holds the PTY",
                        waiting.id
                    );
                    let tty = waiting.master().tty_name();
                    hang_up(pid, tty.as_deref());
                    let still_held = matches!(
                        pty_closed.recv_timeout(EXIT_GRACE),
                        Err(RecvTimeoutError::Timeout)
                    );
                    if still_held && let Some(tty) = tty {
                        tracing::info!(
                            "pane {}'s PTY is still open after SIGHUP: killing what holds it",
                            waiting.id
                        );
                        kill_holders(&tty);
                    }
                }
            })
            .map_err(|err| SpawnError::Pty(err.into()))?;
        Ok(pane)
    }

    /// The pane's id.
    pub fn id(&self) -> PaneId {
        self.id
    }

    fn live(&self) -> MutexGuard<'_, Live> {
        // A panic under the lock leaves nothing half-done that matters more than the pane.
        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Reads one chunk of output: into the tail, the OSC scanner and the `Term`, then out to
    /// every attached client. The `Term`'s answers to queries go back into the PTY.
    fn output(&self, bytes: &[u8]) {
        let mut guard = self.live();
        let live = &mut *guard;
        if self.agent_left(live) {
            reset_modes(live);
        }
        live.tail.push(bytes);
        live.osc.feed(bytes);
        live.folder_due = true;
        live.parser.advance(&mut live.term, bytes);
        live.last_output.printed(self.started.elapsed());
        live.fanout.output(bytes);

        let mut retitled = false;
        while let Ok(event) = live.term_events.try_recv() {
            match event {
                Event::PtyWrite(answer) => self.write(answer.into_bytes()),
                Event::ColorRequest(index, format) => {
                    let color = palette::color(live.term.colors(), index);
                    self.write(format(color).into_bytes());
                }
                Event::Title(title) => {
                    live.title = title;
                    retitled = true;
                }
                Event::ResetTitle => {
                    live.title.clear();
                    retitled = true;
                }
                _ => {}
            }
        }
        if retitled {
            live.fanout.title(&live.title);
        }
    }

    /// The process exited: runs `on_exit` with the last title, then resets the modes it left
    /// on, publishes `exited` and `exit` and drops queued input. `on_exit` runs outside the
    /// lock, since it waits on the database.
    fn exit(&self, code: Option<i64>, on_exit: OnExit) {
        let ended = {
            let mut live = self.live();
            // The process is gone, so only its last OSC 7 can have moved it.
            let folder = self.moved(&mut live, false);
            Ended {
                code,
                title: live.title.clone(),
                folder,
            }
        };
        on_exit(self.id, ended);
        let mut live = self.live();
        reset_modes(&mut live);
        live.queued.clear();
        if let Some(hosting) = &mut live.hosting {
            hosting.hosted = None;
        }
        let Some(state) = live.tracker.exit() else {
            return;
        };
        live.exit_code = code;
        self.events.publish(ServerMessage::State {
            pane: self.id,
            state,
        });
        self.events.publish(ServerMessage::Exit {
            pane: self.id,
            code,
        });
    }

    /// Takes one observation of the screen at `now` (the daemon's clock), publishing a new
    /// state when one has settled and delivering queued input if it is idle. A shell pane
    /// first checks which agent, if any, its foreground process makes it host, and whether
    /// the shell itself is in front, whose own output isn't work (spec §7). Returns what
    /// changed, to persist.
    pub fn observe(&self, now: Duration) -> Observed {
        let leader = self.master().process_group_leader();
        let foreground = leader.and_then(foreground);
        let mut live = self.live();
        let mut observed = Observed {
            folder: self.folder_on_tick(&mut live, now),
            ..Observed::default()
        };
        if live.hosting.is_some()
            && let Some(leader) = leader
        {
            // The shell leads its own process group: it is in front when no command is.
            live.last_output.shell_in_front(Some(leader) == self.pid);
        }
        if let Some(hosted) = foreground
            .as_ref()
            .and_then(|fg| self.rehost(&mut live, fg))
        {
            observed.hosted = Some(hosted);
            if live.tracker.state() == AgentState::Unknown {
                observed.state = Some((AgentState::Unknown, live.title.clone()));
            }
        }
        let observation = Observation {
            process: foreground.map(|foreground| foreground.name),
            title: live.title.clone(),
            rows: screen::rows(&live.term),
            progress: live.osc.progress(),
            program_state: live.osc.program_state(),
            since_output: live.last_output.since(self.started.elapsed()),
        };
        let Some(state) = live.tracker.observe(&observation, now) else {
            return observed;
        };
        self.events.publish(ServerMessage::State {
            pane: self.id,
            state,
        });
        let (ready, waiting) = std::mem::take(&mut live.queued)
            .into_iter()
            .partition(|queued| queued.when.accepts(state));
        live.queued = waiting;
        for queued in ready {
            self.typed(&mut live, queued);
        }
        observed.state = Some((state, live.title.clone()));
        observed
    }

    /// Reads the folder again on a tick at `now` if the pane printed since the last read, at
    /// most once every [`FOLDER_EVERY`]; returns it when it moved.
    fn folder_on_tick(&self, live: &mut Live, now: Duration) -> Option<PathBuf> {
        let rested = (live.folder_read_at).is_none_or(|at| now.saturating_sub(at) >= FOLDER_EVERY);
        if !live.folder_due || !rested {
            return None;
        }
        live.folder_due = false;
        live.folder_read_at = Some(now);
        self.moved(live, true)
    }

    /// Reads the pane's current folder now (spec §6) and returns it when it moved since it was
    /// last read; the pane knows it from then on.
    pub fn refresh_folder(&self) -> Option<PathBuf> {
        let mut live = self.live();
        self.moved(&mut live, true)
    }

    /// The pane's last known folder.
    pub fn folder(&self) -> PathBuf {
        self.live().folder.clone()
    }

    /// The pane's current folder, when it differs from the last known one, which it becomes.
    /// The folder its shell last reported with OSC 7 wins; else, with `proc` and while its
    /// process runs, its working directory from `/proc/<pid>/cwd`; else it hasn't moved.
    fn moved(&self, live: &mut Live, proc: bool) -> Option<PathBuf> {
        let reported = (live.osc.reported_cwd()).and_then(|uri| file_uri(uri, this_host()));
        let running = proc && !self.reaped.load(Ordering::SeqCst);
        let current = reported.or_else(|| self.pid.filter(|_| running).and_then(process_cwd))?;
        if current == live.folder {
            return None;
        }
        live.folder = current.clone();
        Some(current)
    }

    /// Hosts the agent `foreground` is, if that changed: switches to its rules (or back to
    /// the shell's), which starts the state over, and drops the queued input, so nothing meant
    /// for the agent is typed into the shell, nor the other way round. Once no agent is hosted,
    /// resets the modes the one that left had on, unless the shell's output already did.
    /// Returns the agent hosted now when it changed.
    fn rehost(&self, live: &mut Live, foreground: &Foreground) -> Option<Option<Agent>> {
        let (hosted, rules) = live.hosting.as_ref()?.change(foreground)?;
        let hosting = live.hosting.as_mut()?;
        let left = hosting.hosted.is_some() && hosted.is_none() && !hosting.modes_reset;
        hosting.hosted = hosted;
        hosting.modes_reset = false;
        if left {
            reset_modes(live);
        }
        live.queued.clear();
        if let Some(state) = live.tracker.switch_rules(rules) {
            self.events.publish(ServerMessage::State {
                pane: self.id,
                state,
            });
        }
        Some(hosted)
    }

    /// Whether the agent the pane hosts has handed the terminal back to the shell, whose own
    /// process group is in the foreground again, with the modes not yet reset for it; it
    /// records that they are about to be. The state timer sees the agent leave up to a tick
    /// later, after the shell's prompt, which may turn bracketed paste on again; checking each
    /// chunk of output resets the modes ahead of the prompt instead.
    fn agent_left(&self, live: &mut Live) -> bool {
        let Some(hosting) = live.hosting.as_mut() else {
            return false;
        };
        if hosting.hosted.is_none() || hosting.modes_reset {
            return false;
        }
        let back = self.pid.is_some() && self.master().process_group_leader() == self.pid;
        hosting.modes_reset = back;
        back
    }

    /// Types `bytes` into the PTY, now or once the pane's state accepts `when`: as they are,
    /// or with `submit` as a message followed by Enter ([`message`], [`Typing::Enter`]).
    /// Returns whether it was queued.
    pub fn input(&self, bytes: &[u8], submit: bool, when: When) -> Result<bool, Exited> {
        self.enqueue(Queued {
            when,
            bytes: bytes.to_vec(),
            submit,
        })
    }

    fn enqueue(&self, queued: Queued) -> Result<bool, Exited> {
        let mut live = self.live();
        let state = live.tracker.state();
        if state == AgentState::Exited {
            return Err(Exited);
        }
        if !queued.when.accepts(state) {
            live.queued.push(queued);
            return Ok(true);
        }
        self.typed(&mut live, queued);
        Ok(false)
    }

    /// Types `queued`.
    fn typed(&self, live: &mut Live, queued: Queued) {
        live.last_output.typed();
        if queued.submit {
            if !queued.bytes.is_empty() {
                self.write(message(&queued.bytes, bracketed_paste(&live.term)));
            }
            self.send(Typing::Enter);
        } else {
            self.write(queued.bytes);
        }
    }

    /// Bytes a client typed: straight to the PTY.
    pub fn type_bytes(&self, bytes: &[u8]) {
        self.live().last_output.typed();
        self.write(bytes.to_vec());
    }

    fn write(&self, bytes: Vec<u8>) {
        self.send(Typing::Bytes(bytes));
    }

    fn send(&self, typing: Typing) {
        // The writer thread stops only once the PTY is gone; then there is nobody to type to.
        let _ = self.input.send(typing);
    }

    /// Attaches a client: queues `attached` and the snapshot, then subscribes it to live
    /// output, all under the pane's lock.
    pub fn attach(&self, subscriber: SubscriberId, channel: Channel, outbox: Outbox) {
        let mut live = self.live();
        let mut snapshot = snapshot::encode(&mut live.term);
        snapshot::report_cwd(&mut snapshot, live.osc.reported_cwd());
        let attached = ServerMessage::Attached {
            channel,
            pane: self.id,
            cols: live.size.cols,
            rows: live.size.rows,
            title: live.title.clone(),
            state: live.tracker.state(),
        };
        let _ = outbox.send(Outgoing::Message(attached));
        let _ = outbox.send(Outgoing::Bytes {
            channel,
            bytes: snapshot.into(),
            backlog: None,
        });
        live.fanout.subscribe(subscriber, channel, outbox);
    }

    /// Detaches a client.
    pub fn detach(&self, subscriber: SubscriberId) {
        self.live().fanout.unsubscribe(subscriber);
    }

    /// A client asked for `size`: applied if it is the primary client (or there is none),
    /// else the client is told the size the pane keeps.
    pub fn request_resize(&self, subscriber: SubscriberId, size: Size) {
        let mut live = self.live();
        match live.fanout.request_resize(subscriber, size) {
            ResizeDecision::Apply(size) => self.resize(&mut live, size, subscriber),
            ResizeDecision::Refuse => live.fanout.tell_size(subscriber, live.size),
        }
    }

    /// Makes a client the primary one, applying the size it last asked for.
    pub fn make_primary(&self, subscriber: SubscriberId) {
        let mut live = self.live();
        if let Some(size) = live.fanout.make_primary(subscriber) {
            self.resize(&mut live, size, subscriber);
        }
    }

    /// Resizes the `Term` and the PTY together, under the pane's lock, so the program's
    /// redraw reaches every client after the `resize` that tells them.
    fn resize(&self, live: &mut Live, size: Size, by: SubscriberId) {
        if size == live.size || size.cols == 0 || size.rows == 0 {
            return;
        }
        live.term
            .resize(TermSize::new(size.cols.into(), size.rows.into()));
        if let Err(err) = self.master().resize(pty_size(size)) {
            tracing::warn!("resizing pane {}: {err:#}", self.id);
        }
        live.size = size;
        live.last_output.resized(self.started.elapsed());
        live.fanout.resized(size, Some(by));
    }

    fn master(&self) -> MutexGuard<'_, Box<dyn MasterPty + Send>> {
        self.master
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The title and state.
    pub fn status(&self) -> Status {
        let live = self.live();
        Status {
            title: live.title.clone(),
            state: live.tracker.state(),
            exit_code: live.exit_code,
            hosted: live.hosting.as_ref().and_then(|hosting| hosting.hosted),
        }
    }

    /// The last `bytes` bytes of raw output.
    pub fn tail(&self, bytes: usize) -> Vec<u8> {
        self.live().tail.last(bytes)
    }

    /// Sends `signal` to the process's group and to the group in the foreground, which a
    /// shell's job control may have put elsewhere. Does nothing once the process has exited.
    pub fn signal(&self, signal: Signal) {
        if self.live().tracker.state() == AgentState::Exited {
            return;
        }
        let foreground = self.master().process_group_leader();
        for group in [self.pid, foreground].into_iter().flatten() {
            let _ = killpg(Pid::from_raw(group), signal);
        }
    }

    /// Sends `signal` to what stopping the pane ends (spec §4): in a shell pane, the process
    /// group of the agent it hosts, in the foreground, leaving the shell running; in an agent
    /// pane, the pane's process, as [`Pane::signal`] does. Called again after a grace period,
    /// it reaches only what is still there: a hosted agent still in the foreground, or a
    /// process that hasn't exited.
    pub fn stop(&self, signal: Signal) -> Result<(), NothingToStop> {
        let hosting = {
            let live = self.live();
            if live.tracker.state() == AgentState::Exited {
                return Err(NothingToStop::Exited);
            }
            live.hosting.as_ref().map(|hosting| hosting.hosted)
        };
        match hosting {
            None => {
                self.stopped.store(true, Ordering::SeqCst);
                self.signal(signal);
            }
            Some(None) => return Err(NothingToStop::NoAgent),
            // The hosted agent leads the foreground group; the shell's own group is the
            // shell, which a stale `hosted` (the agent just left) must never reach.
            Some(Some(_)) => match self.master().process_group_leader() {
                Some(group) if Some(group) != self.pid => {
                    let _ = killpg(Pid::from_raw(group), signal);
                }
                _ => return Err(NothingToStop::NoAgent),
            },
        }
        Ok(())
    }

    /// Whether [`Pane::stop`] signalled the pane's own process.
    pub fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
}

/// Turns off the modes a killed program may have left on ([`RESET_MODES`]) in the pane's
/// `Term` and in every attached client's copy, as if the program had written it (spec §7).
/// It isn't the program's output, so the tail doesn't get it.
fn reset_modes(live: &mut Live) {
    live.parser.advance(&mut live.term, RESET_MODES);
    live.fanout.output(RESET_MODES);
}

/// Process `leader`, the PTY's foreground process group's leader, from `/proc`: `None` when its
/// name can't be read.
fn foreground(leader: i32) -> Option<Foreground> {
    let name = std::fs::read_to_string(format!("/proc/{leader}/comm")).ok()?;
    let cmdline = std::fs::read(format!("/proc/{leader}/cmdline")).unwrap_or_default();
    let args = (cmdline.split(|byte| *byte == 0))
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect();
    Some(Foreground {
        name: name.trim_end().to_owned(),
        args,
    })
}

/// Process `pid`'s working directory, from `/proc`: `None` once it can't be read. A directory
/// removed since reads as its old path.
fn process_cwd(pid: i32) -> Option<PathBuf> {
    let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    let removed = cwd
        .to_str()
        .and_then(|text| text.strip_suffix(" (deleted)"));
    Some(removed.map_or_else(|| cwd.clone(), PathBuf::from))
}

/// This machine's hostname, read once, which an OSC 7 URI from a shell here may name.
fn this_host() -> Option<&'static str> {
    static HOST: OnceLock<Option<String>> = OnceLock::new();
    HOST.get_or_init(|| {
        std::fs::read_to_string("/proc/sys/kernel/hostname")
            .ok()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
    })
    .as_deref()
}

/// How long the waiter gives the reader to reach the end of the output once the process has
/// exited, before it records the exit and hangs up on what still holds the PTY; and how long
/// it then gives that SIGHUP before it kills what still holds the PTY (spec §7).
const EXIT_GRACE: Duration = Duration::from_secs(1);

/// Sends SIGHUP to every process left in the session `leader` started, and to its process
/// group: what an exited pane's process left running (a background job, an MCP server), so
/// the PTY they hold closes. A process that started a session of its own (`setsid`) is in no
/// session the pane knows, so every process holding `tty`, the PTY's slave, gets it too.
fn hang_up(leader: i32, tty: Option<&Path>) {
    let _ = killpg(Pid::from_raw(leader), Signal::SIGHUP);
    for pid in processes() {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        if session_of(&stat) == Some(leader) || tty.is_some_and(|tty| holds(pid, tty)) {
            let _ = kill(Pid::from_raw(pid), Signal::SIGHUP);
        }
    }
}

/// Sends SIGKILL to every process holding `tty`, the PTY's slave: what ignored [`hang_up`]'s
/// SIGHUP, so the PTY closes and the pane's reader thread, which keeps the pane alive, ends.
fn kill_holders(tty: &Path) {
    for pid in processes().filter(|pid| holds(*pid, tty)) {
        let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
    }
}

/// Every process `/proc` lists but the daemon itself.
fn processes() -> impl Iterator<Item = i32> {
    let daemon = i32::try_from(std::process::id()).ok();
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
        .filter(move |pid| Some(*pid) != daemon)
}

/// Whether process `pid` has `tty` open on any of its file descriptors. Another user's
/// process can't be read, nor signalled, so it never does.
fn holds(pid: i32, tty: &Path) -> bool {
    std::fs::read_dir(format!("/proc/{pid}/fd")).is_ok_and(|fds| {
        fds.flatten()
            .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|target| target == tty))
    })
}

/// The session id in a `/proc/<pid>/stat` line: the fourth field after the command, which is
/// in parentheses and may hold spaces and parentheses itself.
fn session_of(stat: &str) -> Option<i32> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(3)?.parse().ok()
}

fn pty_size(size: Size) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod spawn_error {
        use super::*;

        #[test]
        fn should_say_only_what_to_do_about_a_program_not_on_the_path() {
            let err = SpawnError::NotOnPath {
                program: "codex".into(),
                err: anyhow::anyhow!("No viable candidates found in PATH \"/usr/bin:/bin\""),
            };

            assert_eq!(
                err.to_string(),
                "`codex` isn't on agentdeckd's PATH. Install it, or start agentdeckd from a \
                 shell where it's found."
            );
        }
    }

    mod message {
        use super::*;

        #[test]
        fn should_bracket_the_text_as_one_paste_when_bracketed_paste_is_on() {
            assert_eq!(
                message(b"fix it\nthen test", true),
                b"\x1b[200~fix it\nthen test\x1b[201~"
            );
        }

        #[test]
        fn should_drop_any_esc_so_the_text_cannot_end_the_paste_early() {
            assert_eq!(message(b"a\x1b[201~b", true), b"\x1b[200~a[201~b\x1b[201~");
        }

        #[test]
        fn should_type_the_text_as_is_when_bracketed_paste_is_off() {
            assert_eq!(message(b"a\x1bb\n", false), b"a\x1bb\n");
        }
    }

    mod session_of {
        use super::*;

        #[test]
        fn should_read_the_session_after_a_command_holding_parentheses_and_spaces() {
            let stat = "4242 (a (b) c) S 1 4240 4239 34817 4240 4194560 0 0";
            assert_eq!(session_of(stat), Some(4239));
        }

        #[test]
        fn should_be_none_for_a_line_it_cannot_read() {
            assert_eq!(session_of(""), None);
            assert_eq!(session_of("4242 (sh) S 1"), None);
        }
    }

    mod when {
        use super::*;

        fn accepted(when: When) -> Vec<AgentState> {
            AgentState::ALL
                .iter()
                .copied()
                .filter(|state| when.accepts(*state))
                .collect()
        }

        #[test]
        fn should_type_idle_input_while_idle_done_or_blocked() {
            assert_eq!(
                accepted(When::Idle),
                [AgentState::Blocked, AgentState::Done, AgentState::Idle]
            );
        }
    }
}

/// A pane over a real PTY running `sh`. `Pane` is crate-private and the daemon is a binary,
/// so `tests/integration` can't reach it; the integration nextest profile runs this module,
/// and the unit one skips it.
#[cfg(test)]
mod integration_tests {
    use super::*;

    mod exit {
        use super::*;

        fn is_exit(event: &ServerMessage) -> bool {
            matches!(
                event,
                ServerMessage::Exit { .. }
                    | ServerMessage::State {
                        state: AgentState::Exited,
                        ..
                    }
            )
        }

        #[test]
        fn should_report_an_exit_only_once_on_exit_has_returned() {
            let events = Events::new();
            let mut feed = events.subscribe();
            let (exited, on_exit_called) = channel();
            let (release, released) = channel::<()>();
            let on_exit: OnExit = Box::new(move |_, ended: Ended| {
                exited.send(ended.code).expect("the test waits for on_exit");
                released.recv().expect("the test releases on_exit");
            });
            let id = uuid::Uuid::now_v7().into();
            let argv = ["sh", "-c", "exit 3"].map(String::from).to_vec();
            let launch = Launch::pane(
                id,
                uuid::Uuid::now_v7().into(),
                None,
                argv,
                std::env::temp_dir(),
            );
            let rules = Rules::Fixed(crate::terminal::rules::resolve("shell", None));
            let pane = Pane::spawn(id, launch, rules, events, on_exit).expect("spawn sh");

            // on_exit is where the daemon saves the exit: while it runs, nothing may read the
            // pane as exited, or archiving its workspace is refused (#172).
            let code = on_exit_called.recv().expect("on_exit runs once sh exits");
            assert_eq!(code, Some(3));
            assert_ne!(pane.status().state, AgentState::Exited);
            let early: Vec<_> = std::iter::from_fn(|| feed.try_recv().ok()).collect();
            assert!(
                !early.iter().any(is_exit),
                "published before on_exit returned: {early:?}"
            );

            release.send(()).expect("on_exit is waiting");
            let event = feed.blocking_recv().expect("the exit is published");
            assert!(is_exit(&event), "{event:?}");
            assert_eq!(pane.status().state, AgentState::Exited);
            assert_eq!(pane.status().exit_code, Some(3));
        }
    }
}
