//! A WebSocket client for `/v1/ws` (spec §5), and a [`Screen`] that renders one channel's
//! bytes through its own `alacritty_terminal::Term`, as the GUI does.

use std::time::Duration;

use agentdeck_core::PaneId;
use agentdeck_core::state::screen;
use agentdeck_core::ws::{Channel, ClientMessage, ServerMessage, frame, parse_frame};
use alacritty_terminal::Term;
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::TestDaemon;
use super::terminal::DEADLINE;

/// One frame from the daemon.
#[derive(Debug)]
pub enum Frame {
    /// A text frame.
    Message(ServerMessage),
    /// A binary frame.
    Bytes(Channel, Vec<u8>),
}

/// The socket under a [`Ws`]: `connect_async`'s, or a plain TCP stream.
type Stream = MaybeTlsStream<TcpStream>;

/// A connection to the daemon's WebSocket.
pub struct Ws {
    socket: WebSocketStream<Stream>,
}

/// The upgrade request for `daemon`'s `/v1/ws`, with `token` as the bearer if any.
pub fn request(
    daemon: &TestDaemon,
    token: Option<&str>,
) -> tokio_tungstenite::tungstenite::handshake::client::Request {
    let mut request = format!("ws://127.0.0.1:{}/v1/ws", daemon.info.port)
        .into_client_request()
        .expect("a WebSocket request");
    if let Some(token) = token {
        let value = format!("Bearer {token}").parse().expect("a header value");
        request.headers_mut().insert("authorization", value);
    }
    request
}

impl Ws {
    /// Connects with the daemon's token.
    pub async fn connect(daemon: &TestDaemon) -> Ws {
        let (socket, _) =
            tokio_tungstenite::connect_async(request(daemon, Some(&daemon.info.token)))
                .await
                .expect("connect to /v1/ws");
        Ws { socket }
    }

    /// Connects over a socket with a small fixed receive buffer, which turns off the
    /// kernel's autotuning (up to 32 MiB on some hosts). A client that stops reading then
    /// falls behind after a few MiB, not tens.
    pub async fn connect_with_small_buffer(daemon: &TestDaemon) -> Ws {
        let socket = tokio::net::TcpSocket::new_v4().expect("a socket");
        socket
            .set_recv_buffer_size(16 * 1024)
            .expect("set SO_RCVBUF");
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], daemon.info.port));
        let stream = socket.connect(addr).await.expect("connect");
        let (socket, _) = tokio_tungstenite::client_async(
            request(daemon, Some(&daemon.info.token)),
            MaybeTlsStream::Plain(stream),
        )
        .await
        .expect("connect to /v1/ws");
        Ws { socket }
    }

    pub async fn send(&mut self, message: ClientMessage) {
        let text = serde_json::to_string(&message).expect("a client message serializes");
        self.socket
            .send(Message::text(text))
            .await
            .expect("send a text frame");
    }

    /// Types `bytes` on `channel`.
    pub async fn type_bytes(&mut self, channel: Channel, bytes: &[u8]) {
        self.socket
            .send(Message::binary(frame(channel, bytes)))
            .await
            .expect("send a binary frame");
    }

    /// The next frame, failing the test after [`DEADLINE`].
    pub async fn next(&mut self) -> Frame {
        loop {
            let message = tokio::time::timeout(DEADLINE, self.socket.next())
                .await
                .expect("timed out waiting for a frame")
                .expect("the socket closed")
                .expect("a WebSocket error");
            match message {
                Message::Text(text) => {
                    let message = serde_json::from_str(&text).expect("a server message");
                    return Frame::Message(message);
                }
                Message::Binary(bytes) => {
                    let (channel, bytes) = parse_frame(&bytes).expect("a channel id");
                    return Frame::Bytes(channel, bytes.to_vec());
                }
                _ => {}
            }
        }
    }

    /// Skips frames until `pick` takes one.
    pub async fn until<T>(&mut self, mut pick: impl FnMut(Frame) -> Option<T>) -> T {
        loop {
            if let Some(value) = pick(self.next().await) {
                return value;
            }
        }
    }

    /// Skips frames until this exact message.
    pub async fn expect(&mut self, wanted: ServerMessage) {
        self.until(|frame| match frame {
            Frame::Message(message) if message == wanted => Some(()),
            _ => None,
        })
        .await;
    }

    /// Whether any frame arrives within `wait`. For checking that something *didn't*
    /// happen, after something that would have come first did.
    pub async fn quiet_for(&mut self, wait: Duration) -> Option<Frame> {
        let next = tokio::time::timeout(wait, self.socket.next())
            .await
            .ok()??;
        Some(match next.expect("a WebSocket error") {
            Message::Text(text) => Frame::Message(serde_json::from_str(&text).expect("JSON")),
            Message::Binary(bytes) => {
                let (channel, bytes) = parse_frame(&bytes).expect("a channel id");
                Frame::Bytes(channel, bytes.to_vec())
            }
            other => panic!("unexpected frame {other:?}"),
        })
    }

    /// Attaches `pane` on `channel` and renders its snapshot.
    pub async fn attach(&mut self, pane: PaneId, channel: Channel) -> Screen {
        self.send(ClientMessage::Attach { pane, channel }).await;
        let (cols, rows) = self
            .until(|frame| match frame {
                Frame::Message(ServerMessage::Attached {
                    channel: c,
                    cols,
                    rows,
                    ..
                }) if c == channel => Some((cols, rows)),
                _ => None,
            })
            .await;
        let mut screen = Screen::new(channel, cols, rows);
        let snapshot = match self.next().await {
            Frame::Bytes(c, bytes) if c == channel => bytes,
            other => panic!("expected the snapshot, got {other:?}"),
        };
        screen.feed(&snapshot);
        screen
    }
}

/// One channel's terminal, as a client renders it.
pub struct Screen {
    channel: Channel,
    term: Term<VoidListener>,
    parser: Processor<StdSyncHandler>,
}

impl Screen {
    fn new(channel: Channel, cols: u16, rows: u16) -> Screen {
        Screen {
            channel,
            term: Term::new(
                Config::default(),
                &TermSize::new(cols.into(), rows.into()),
                VoidListener,
            ),
            parser: Processor::new(),
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Takes a frame meant for this channel: bytes are drawn and resizes applied.
    pub fn take(&mut self, frame: &Frame) {
        match frame {
            Frame::Bytes(channel, bytes) if *channel == self.channel => self.feed(bytes),
            Frame::Message(ServerMessage::Resize {
                channel,
                cols,
                rows,
            }) if *channel == self.channel => self
                .term
                .resize(TermSize::new((*cols).into(), (*rows).into())),
            _ => {}
        }
    }

    /// Reads frames into the screen until `done` holds.
    pub async fn until(&mut self, ws: &mut Ws, mut done: impl FnMut(&Screen) -> bool) {
        while !done(self) {
            let frame = ws.next().await;
            self.take(&frame);
        }
    }

    /// The live screen's rows.
    pub fn rows(&self) -> Vec<String> {
        screen::rows(&self.term)
    }

    /// The terminal modes the pane's output left on.
    pub fn modes(&self) -> TermMode {
        *self.term.mode()
    }

    /// Whether the live screen shows `text`.
    pub fn shows(&self, text: &str) -> bool {
        self.rows().iter().any(|row| row.contains(text))
    }

    /// Every row from the top of the scrollback to the bottom of the screen.
    pub fn all_rows(&self) -> Vec<String> {
        let grid = self.term.grid();
        let top = -(grid.history_size() as i32);
        (top..grid.screen_lines() as i32)
            .map(|line| {
                let row = &grid[Line(line)];
                let text: String = (0..grid.columns()).map(|c| row[Column(c)].c).collect();
                text.trim_end().to_owned()
            })
            .collect()
    }
}
