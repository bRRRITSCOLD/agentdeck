//! One live pane: its PTY and process, the daemon's `Term` for it, and everything the
//! daemon derives from the output (spec §5–§7).
//!
//! Two OS threads serve each pane, since `portable-pty`'s reads and writes block:
//! - the **reader** feeds every chunk of output through [`Pane::output`], then waits for the
//!   process and reports its exit;
//! - the **writer** writes input in order: typed bytes, delivered `input`, and the `Term`'s
//!   answers to terminal queries.
//!
//! Everything a chunk of output touches (the `Term`, the tail, the OSC scanner, the attached
//! clients' queues) changes under one lock, [`Pane::live`]. An attach snapshots the `Term`
//! and subscribes under that same lock, so no output falls between the snapshot and the live
//! bytes, and none arrives twice. Nothing under the lock blocks: input goes to the writer
//! thread through a channel, and clients' queues are unbounded (their size is capped by the
//! `lagged` rule instead).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use agentdeck_core::state::{Observation, OscScanner, RuleSet, StateTracker, screen};
use agentdeck_core::ws::{Channel, ServerMessage};
use agentdeck_core::{AgentState, PaneId, WorkspaceId};
use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::term::Config;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::vte::ansi::Processor;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::oneshot;

use super::env::pane_env;
use super::fanout::{Fanout, Outbox, Outgoing, ResizeDecision, Size, SubscriberId};
use super::tail::{TAIL_CAPACITY, Tail};
use super::{palette, snapshot};
use crate::events::Events;

/// What to run in a new pane.
#[derive(Debug, Clone)]
pub struct Launch {
    /// The program and its arguments; `argv[0]` is looked up on the env's `PATH`.
    pub argv: Vec<String>,
    /// The working directory; it must exist.
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
    /// Pane `id`, in `workspace`, running `argv` in `cwd` at 80×24. Its environment is the
    /// allowlisted part of the daemon's, plus `AGENTDECK_PANE` and `AGENTDECK_WORKSPACE`.
    pub fn pane(id: PaneId, workspace: WorkspaceId, argv: Vec<String>, cwd: PathBuf) -> Launch {
        let extra = [
            ("AGENTDECK_PANE".to_owned(), id.to_string()),
            ("AGENTDECK_WORKSPACE".to_owned(), workspace.to_string()),
        ];
        Launch {
            argv,
            cwd,
            env: pane_env(std::env::vars_os(), &extra),
            size: INITIAL_SIZE,
        }
    }
}

/// When `input` reaches the PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// At once.
    Now,
    /// Once the pane's state is idle, done or blocked; at once if it already is.
    Idle,
    /// Once the agent waits at its prompt: idle or done, never blocked, so the text can't
    /// answer a dialog such as Claude Code's folder trust dialog (spec §6, §9).
    Prompt,
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
            When::Prompt => matches!(state, AgentState::Idle | AgentState::Done),
        }
    }
}

/// Input waiting for its [`When`].
struct Queued {
    when: When,
    bytes: Vec<u8>,
    /// Told once the bytes are typed; dropped untold if the process exits first.
    typed: Option<oneshot::Sender<()>>,
}

/// Why a pane couldn't start.
#[derive(Debug)]
pub enum SpawnError {
    /// The PTY or the threads serving it couldn't be set up.
    Pty(anyhow::Error),
    /// The command couldn't be started: no such program, not executable, …
    Command(anyhow::Error),
}

impl fmt::Display for SpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpawnError::Pty(err) => write!(f, "setting up the PTY: {err:#}"),
            SpawnError::Command(err) => write!(f, "{err:#}"),
        }
    }
}

/// The pane's process has exited, so it takes no input.
#[derive(Debug)]
pub struct Exited;

/// What a pane shows right now.
#[derive(Debug, Clone)]
pub struct Status {
    /// The title the program set.
    pub title: String,
    /// The published state.
    pub state: AgentState,
    /// The exit code, once the process has exited and it could be read.
    pub exit_code: Option<i64>,
}

/// Called once when the pane's process exits, with its exit code and last title.
pub type OnExit = Box<dyn FnOnce(PaneId, Option<i64>, String) + Send>;

/// A live pane.
pub struct Pane {
    id: PaneId,
    live: Mutex<Live>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    input: Sender<Vec<u8>>,
    /// The process's id, which is also its process group's: it runs as a session leader.
    pid: Option<i32>,
    events: Events,
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
    last_output: Option<Instant>,
    tracker: StateTracker,
    /// `input` waiting for its state, in the order it came.
    queued: Vec<Queued>,
    exit_code: Option<i64>,
    fanout: Fanout,
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
    /// `state` and `exit` events; `on_exit` runs once the process has exited.
    pub fn spawn(
        id: PaneId,
        launch: Launch,
        rules: RuleSet,
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
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(SpawnError::Command)?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(SpawnError::Pty)?;
        let mut writer = pair.master.take_writer().map_err(SpawnError::Pty)?;

        let (input, typed) = channel::<Vec<u8>>();
        let (term_events, term_events_rx) = channel();
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
                last_output: None,
                tracker: StateTracker::new(rules),
                queued: Vec::new(),
                exit_code: None,
                fanout: Fanout::default(),
            }),
            master: Mutex::new(pair.master),
            input,
            pid: child.process_id().and_then(|pid| i32::try_from(pid).ok()),
            events,
        });

        let thread = |role: &str| std::thread::Builder::new().name(format!("pane-{role}-{id}"));
        thread("write")
            .spawn(move || {
                for bytes in typed {
                    if writer
                        .write_all(&bytes)
                        .and_then(|()| writer.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|err| SpawnError::Pty(err.into()))?;
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
                let code = child
                    .wait()
                    .ok()
                    .map(|status| i64::from(status.exit_code()));
                if let Some(title) = reading.exit(code) {
                    on_exit(reading.id, code, title);
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
        live.tail.push(bytes);
        live.osc.feed(bytes);
        live.parser.advance(&mut live.term, bytes);
        live.last_output = Some(Instant::now());
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

    /// The process exited: publishes `exited` and `exit`, drops queued input. Returns the
    /// last title the first time, `None` after.
    fn exit(&self, code: Option<i64>) -> Option<String> {
        let mut live = self.live();
        live.queued.clear();
        let state = live.tracker.exit()?;
        live.exit_code = code;
        self.events.publish(ServerMessage::State {
            pane: self.id,
            state,
        });
        self.events.publish(ServerMessage::Exit {
            pane: self.id,
            code,
        });
        Some(live.title.clone())
    }

    /// Takes one observation of the screen at `now` (the daemon's clock), publishing a new
    /// state when one has settled and delivering queued input if it is idle. Returns the new
    /// state with the title it was seen under, to persist.
    pub fn observe(&self, now: Duration) -> Option<(AgentState, String)> {
        let process = self.foreground_process();
        let mut live = self.live();
        let observation = Observation {
            process,
            title: live.title.clone(),
            rows: screen::rows(&live.term),
            progress: live.osc.progress(),
            program_state: live.osc.program_state(),
            since_output: live.last_output.map(|at| at.elapsed()),
        };
        let state = live.tracker.observe(&observation, now)?;
        self.events.publish(ServerMessage::State {
            pane: self.id,
            state,
        });
        let (ready, waiting) = std::mem::take(&mut live.queued)
            .into_iter()
            .partition(|queued| queued.when.accepts(state));
        live.queued = waiting;
        for queued in ready {
            self.typed(queued);
        }
        Some((state, live.title.clone()))
    }

    /// Types `bytes` into the PTY, now or once the pane's state accepts `when`. Returns
    /// whether it was queued.
    pub fn input(&self, bytes: &[u8], when: When) -> Result<bool, Exited> {
        self.enqueue(Queued {
            when,
            bytes: bytes.to_vec(),
            typed: None,
        })
    }

    /// Types `bytes` into the PTY once the agent waits at its prompt ([`When::Prompt`]). The
    /// receiver is told when they are typed; it errs if the process exits first.
    pub fn deliver(&self, bytes: &[u8]) -> Result<oneshot::Receiver<()>, Exited> {
        let (typed, delivered) = oneshot::channel();
        self.enqueue(Queued {
            when: When::Prompt,
            bytes: bytes.to_vec(),
            typed: Some(typed),
        })?;
        Ok(delivered)
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
        self.typed(queued);
        Ok(false)
    }

    fn typed(&self, queued: Queued) {
        self.write(queued.bytes);
        if let Some(typed) = queued.typed {
            let _ = typed.send(());
        }
    }

    /// Bytes a client typed: straight to the PTY.
    pub fn type_bytes(&self, bytes: &[u8]) {
        self.write(bytes.to_vec());
    }

    fn write(&self, bytes: Vec<u8>) {
        // The writer thread stops only once the PTY is gone; then there is nobody to type to.
        let _ = self.input.send(bytes);
    }

    /// Attaches a client: queues `attached` and the snapshot, then subscribes it to live
    /// output, all under the pane's lock.
    pub fn attach(&self, subscriber: SubscriberId, channel: Channel, outbox: Outbox) {
        let mut live = self.live();
        let snapshot = snapshot::encode(&mut live.term);
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

    /// The name of the process in the foreground, from `/proc`.
    fn foreground_process(&self) -> Option<String> {
        let leader = self.master().process_group_leader()?;
        let name = std::fs::read_to_string(format!("/proc/{leader}/comm")).ok()?;
        Some(name.trim_end().to_owned())
    }
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

        #[test]
        fn should_never_type_prompt_input_into_a_blocked_pane() {
            assert_eq!(accepted(When::Prompt), [AgentState::Done, AgentState::Idle]);
        }
    }
}
