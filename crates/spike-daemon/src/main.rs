//! PROTOTYPE (issue #2): a tokio daemon that owns PTYs and streams them over a WebSocket.
//!
//! - `POST /sessions` `{argv, cwd?, cols, rows, env?}` spawns a PTY, returns `{id}`.
//! - `GET /sessions` lists sessions with their hook state.
//! - `POST /sessions/{id}/state` `{state, hook?}` is what `spike-cli state` calls.
//! - `POST /sessions/{id}/input` raw bytes to the PTY (scripted experiments).
//! - `GET /sessions/{id}/ws?replay_limit=N` attaches: see `attach` for the frame format.
//!
//! Port + bearer token land in `$XDG_RUNTIME_DIR/agentdeck-spike/daemon.json` (0600).

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Context as _;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;

/// One entry in the replay log. Resizes are logged in-band so a replay re-applies them at
/// the point they happened: bytes produced at 80x24 replayed into a 120x40 grid wrap wrong.
#[derive(Clone, Debug)]
enum Frame {
    Output(Arc<[u8]>),
    Resize { cols: u16, rows: u16 },
    State(Value),
    Exit(Option<u32>),
}

/// The byte-capped replay log.
struct Log {
    frames: VecDeque<Frame>,
    bytes: usize,
    cap: usize,
    /// Grid size the first logged byte was written at.
    start_size: (u16, u16),
    /// Total output bytes ever produced.
    total: u64,
    /// Output bytes dropped from the front because of the cap.
    dropped: u64,
}

impl Log {
    fn push(&mut self, frame: Frame) {
        if let Frame::Output(b) = &frame {
            self.bytes += b.len();
            self.total += b.len() as u64;
        }
        self.frames.push_back(frame);
        while self.bytes > self.cap {
            match self.frames.pop_front() {
                Some(Frame::Output(b)) => {
                    self.bytes -= b.len();
                    self.dropped += b.len() as u64;
                }
                Some(Frame::Resize { cols, rows }) => self.start_size = (cols, rows),
                Some(_) => {}
                None => break,
            }
        }
    }
}

struct Session {
    id: String,
    argv: Vec<String>,
    started: Instant,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    log: Mutex<Log>,
    size: Mutex<(u16, u16)>,
    live: broadcast::Sender<Frame>,
    state: Mutex<Value>,
}

#[derive(Clone)]
struct App {
    token: Arc<String>,
    cap: usize,
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    hook_log: Arc<Mutex<std::fs::File>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cap: usize = std::env::var("SPIKE_REPLAY_CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4 << 20);
    let token: String = {
        use rand::Rng as _;
        let bytes: [u8; 16] = rand::rng().random();
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    };
    let runtime = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR")?;
    let dir = std::path::Path::new(&runtime).join("agentdeck-spike");
    std::fs::create_dir_all(&dir)?;
    let hook_log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("hooks.jsonl"))?;
    let app = App {
        token: Arc::new(token.clone()),
        cap,
        sessions: Default::default(),
        hook_log: Arc::new(Mutex::new(hook_log)),
    };
    let router = Router::new()
        .route("/sessions", post(spawn_session).get(list_sessions))
        .route("/sessions/{id}/state", post(set_state))
        .route("/sessions/{id}/input", post(post_input))
        .route("/sessions/{id}/raw", get(raw))
        .route("/sessions/{id}/ws", get(ws))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let file = dir.join("daemon.json");
    // Create 0600 before writing the token.
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&file)?;
        f.write_all(
            json!({"port": port, "token": token, "pid": std::process::id()})
                .to_string()
                .as_bytes(),
        )?;
    }
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
    eprintln!(
        "spike-daemon on 127.0.0.1:{port}, replay cap {cap} bytes, {}",
        file.display()
    );
    axum::serve(listener, router).await?;
    Ok(())
}

use std::os::unix::fs::OpenOptionsExt as _;

fn authorized(app: &App, headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("Bearer {}", app.token))
}

#[derive(Deserialize)]
struct SpawnReq {
    argv: Vec<String>,
    cwd: Option<String>,
    cols: u16,
    rows: u16,
    #[serde(default)]
    env: HashMap<String, String>,
}

async fn spawn_session(
    State(app): State<App>,
    headers: HeaderMap,
    Json(req): Json<SpawnReq>,
) -> Response {
    if !authorized(&app, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match spawn(&app, req) {
        Ok(id) => Json(json!({ "id": id })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    }
}

fn spawn(app: &App, req: SpawnReq) -> anyhow::Result<String> {
    let id = format!("s{}", app.sessions.lock().unwrap().len() + 1);
    let pair = native_pty_system().openpty(PtySize {
        rows: req.rows,
        cols: req.cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(&req.argv[0]);
    cmd.args(&req.argv[1..]);
    if let Some(cwd) = &req.cwd {
        cmd.cwd(cwd);
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("AGENTDECK_SPIKE_SESSION", &id);
    for (k, v) in &req.env {
        cmd.env(k, v);
    }
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    let (live, _) = broadcast::channel(4096);
    let session = Arc::new(Session {
        id: id.clone(),
        argv: req.argv,
        started: Instant::now(),
        master: Mutex::new(pair.master),
        writer: Mutex::new(writer),
        log: Mutex::new(Log {
            frames: VecDeque::new(),
            bytes: 0,
            cap: app.cap,
            start_size: (req.cols, req.rows),
            total: 0,
            dropped: 0,
        }),
        size: Mutex::new((req.cols, req.rows)),
        live,
        state: Mutex::new(json!({ "state": "unknown" })),
    });
    app.sessions
        .lock()
        .unwrap()
        .insert(id.clone(), session.clone());

    // portable-pty's reader is blocking: one OS thread per PTY.
    let s = session.clone();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => s.publish(Frame::Output(Arc::from(&buf[..n]))),
            }
        }
        let code = child.wait().ok().map(|s| s.exit_code());
        s.publish(Frame::Exit(code));
    });
    Ok(id)
}

impl Session {
    /// Appends to the log and fans out under one lock, so an attach that snapshots the log and
    /// subscribes under the same lock never misses or duplicates a frame.
    fn publish(&self, frame: Frame) {
        let mut log = self.log.lock().unwrap();
        log.push(frame.clone());
        let _ = self.live.send(frame);
    }

    fn resize(&self, cols: u16, rows: u16) {
        let mut size = self.size.lock().unwrap();
        if *size == (cols, rows) {
            return;
        }
        *size = (cols, rows);
        // Log the resize before the PTY sees it, so it precedes the app's redraw in the log.
        self.publish(Frame::Resize { cols, rows });
        let _ = self.master.lock().unwrap().resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    fn summary(&self) -> Value {
        let log = self.log.lock().unwrap();
        let (cols, rows) = *self.size.lock().unwrap();
        json!({
            "id": self.id,
            "argv": self.argv,
            "cols": cols, "rows": rows,
            "state": *self.state.lock().unwrap(),
            "buffered_bytes": log.bytes,
            "total_bytes": log.total,
            "dropped_bytes": log.dropped,
            "uptime_ms": self.started.elapsed().as_millis() as u64,
        })
    }
}

async fn list_sessions(State(app): State<App>, headers: HeaderMap) -> Response {
    if !authorized(&app, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let sessions: Vec<Value> = app
        .sessions
        .lock()
        .unwrap()
        .values()
        .map(|s| s.summary())
        .collect();
    Json(json!(sessions)).into_response()
}

fn session(app: &App, id: &str) -> Option<Arc<Session>> {
    app.sessions.lock().unwrap().get(id).cloned()
}

#[derive(Deserialize)]
struct StateReq {
    state: String,
    #[serde(default)]
    hook: Value,
}

async fn set_state(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<StateReq>,
) -> Response {
    if !authorized(&app, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(s) = session(&app, &id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Record the raw hook payload so the spike can report what each event carries.
    let line = json!({"session": id, "state": req.state, "hook": req.hook});
    let _ = writeln!(app.hook_log.lock().unwrap(), "{line}");
    let mut state = s.state.lock().unwrap();
    let claude_session = req
        .hook
        .get("session_id")
        .cloned()
        .or_else(|| state.get("claude_session_id").cloned())
        .unwrap_or(Value::Null);
    *state = json!({
        "state": req.state,
        "claude_session_id": claude_session,
        "event": req.hook.get("hook_event_name"),
    });
    let frame = Frame::State(state.clone());
    drop(state);
    s.publish(frame);
    StatusCode::NO_CONTENT.into_response()
}

async fn post_input(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    if !authorized(&app, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(s) = session(&app, &id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let _ = s.writer.lock().unwrap().write_all(&body);
    StatusCode::NO_CONTENT.into_response()
}

/// The logged output bytes, concatenated (experiments: grep for OSC sequences).
async fn raw(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    if !authorized(&app, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(s) = session(&app, &id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let log = s.log.lock().unwrap();
    let mut out = Vec::with_capacity(log.bytes);
    for f in &log.frames {
        if let Frame::Output(b) = f {
            out.extend_from_slice(b);
        }
    }
    out.into_response()
}

#[derive(Deserialize)]
struct WsQuery {
    /// Replay at most this many trailing output bytes (experiments: what does a cap break?).
    replay_limit: Option<usize>,
}

async fn ws(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<WsQuery>,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !authorized(&app, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(s) = session(&app, &id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    upgrade.on_upgrade(move |socket| attach(socket, s, q.replay_limit))
}

/// One attached client.
///
/// Server → client: text `{"t":"hello",cols,rows,...}` (the size the first replayed byte was
/// written at), then the replay as binary frames interleaved with text `{"t":"resize"}`, then
/// text `{"t":"live"}`, then live binary output and text `resize` / `state` / `exit` events.
/// Client → server: binary = input bytes for the PTY; text `{"t":"resize",cols,rows}`.
/// Closing the socket is the detach; the PTY keeps running.
async fn attach(mut socket: WebSocket, s: Arc<Session>, replay_limit: Option<usize>) {
    let started = Instant::now();
    // Snapshot + subscribe under the log lock: no gap, no duplicate.
    let (frames, start_size, mut live, total, dropped) = {
        let log = s.log.lock().unwrap();
        (
            log.frames.clone(),
            log.start_size,
            s.live.subscribe(),
            log.total,
            log.dropped,
        )
    };
    let (frames, (cols, rows)) = trim(frames, start_size, replay_limit);
    let replay_bytes: usize = frames
        .iter()
        .map(|f| if let Frame::Output(b) = f { b.len() } else { 0 })
        .sum();
    let hello = json!({
        "t": "hello", "session": s.id, "cols": cols, "rows": rows,
        "replay_bytes": replay_bytes, "total_bytes": total, "dropped_bytes": dropped,
        "state": *s.state.lock().unwrap(),
    });
    if socket
        .send(Message::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    for frame in frames {
        if send_frame(&mut socket, frame).await.is_err() {
            return;
        }
    }
    let live_msg = json!({"t": "live", "replay_send_ms": started.elapsed().as_secs_f64() * 1000.0});
    if socket
        .send(Message::Text(live_msg.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            frame = live.recv() => match frame {
                Ok(frame) => if send_frame(&mut socket, frame).await.is_err() { return },
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // A slow client: drop it; it reattaches and replays.
                    let _ = socket.send(Message::Text(json!({"t":"lagged","frames":n}).to_string().into())).await;
                    return;
                }
                Err(_) => return,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(bytes))) => {
                    let _ = s.writer.lock().unwrap().write_all(&bytes);
                }
                Some(Ok(Message::Text(text))) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&text)
                        && v["t"] == "resize"
                    {
                        let cols = v["cols"].as_u64().unwrap_or(80) as u16;
                        let rows = v["rows"].as_u64().unwrap_or(24) as u16;
                        s.resize(cols, rows);
                    }
                }
                Some(Ok(_)) => {}
                _ => return, // detach
            }
        }
    }
}

/// Keeps the trailing `limit` output bytes and returns the size they start at.
fn trim(
    frames: VecDeque<Frame>,
    start_size: (u16, u16),
    limit: Option<usize>,
) -> (Vec<Frame>, (u16, u16)) {
    let frames: Vec<Frame> = frames.into();
    let Some(limit) = limit else {
        return (frames, start_size);
    };
    let total: usize = frames
        .iter()
        .map(|f| if let Frame::Output(b) = f { b.len() } else { 0 })
        .sum();
    let mut skip = total.saturating_sub(limit);
    let mut size = start_size;
    let mut kept = Vec::new();
    for frame in frames {
        match frame {
            Frame::Output(b) if skip > 0 => {
                if b.len() <= skip {
                    skip -= b.len();
                } else {
                    kept.push(Frame::Output(Arc::from(&b[skip..])));
                    skip = 0;
                }
            }
            Frame::Resize { cols, rows } if skip > 0 => size = (cols, rows),
            _ if skip > 0 => {}
            other => kept.push(other),
        }
    }
    (kept, size)
}

async fn send_frame(socket: &mut WebSocket, frame: Frame) -> Result<(), axum::Error> {
    let msg = match frame {
        Frame::Output(b) => Message::Binary(b.to_vec().into()),
        Frame::Resize { cols, rows } => Message::Text(
            json!({"t":"resize","cols":cols,"rows":rows})
                .to_string()
                .into(),
        ),
        Frame::State(v) => Message::Text(json!({"t":"state","state":v}).to_string().into()),
        Frame::Exit(code) => Message::Text(json!({"t":"exit","code":code}).to_string().into()),
    };
    socket.send(msg).await
}
