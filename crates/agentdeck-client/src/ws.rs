//! The daemon's WebSocket, `/v1/ws` (spec §5), for a client that renders panes: the GUI.
//!
//! [`open`] connects with the daemon's bearer token and runs the socket on tokio tasks. A
//! [`Socket`] hands back a cloneable [`Sender`] and a receiver of [`Event`]s. Both are tokio
//! `sync` types, which don't need a tokio runtime to be polled, so a GPUI task can await them.
//! [`loopback`] gives a client's tests a socket with no daemon behind it.
//!
//! The event channel is bounded: a client that stops reading stops the socket being read, so
//! the daemon sees it fall behind and sends `lagged` (spec §5, backpressure).

use agentdeck_core::DaemonInfo;
use agentdeck_core::ws::{Channel, ClientMessage, ServerMessage, frame, parse_frame};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};

/// How many events wait for the client before the socket stops being read.
const EVENT_BUFFER: usize = 256;

/// One thing the daemon sent, or the end of the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A text frame.
    Message(ServerMessage),
    /// A binary frame: a snapshot or live output on `channel`.
    Output(Channel, Vec<u8>),
    /// The connection ended; the reason, when there was one. No event follows it.
    Closed(Option<String>),
}

/// Why [`open`] couldn't connect.
#[derive(Debug, thiserror::Error)]
pub enum WsError {
    /// The token can't be sent in an HTTP header.
    #[error("the token isn't a valid header value")]
    InvalidToken,
    /// The upgrade failed: refused, or answered with an error status such as 401.
    #[error("connecting to the daemon's WebSocket")]
    Connect(#[source] Box<tungstenite::Error>),
}

/// The socket was closed, so nothing more can be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the daemon's WebSocket is closed")]
pub struct Closed;

/// One frame for the daemon, as a [`Sender`] queues it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outgoing {
    /// A text frame.
    Message(ClientMessage),
    /// PTY input on a channel.
    Input(Channel, Vec<u8>),
}

/// Sends to the daemon. Cheap to clone; the socket closes once every clone and the
/// [`Socket::events`] receiver are dropped.
#[derive(Debug, Clone)]
pub struct Sender(mpsc::UnboundedSender<Outgoing>);

impl Sender {
    /// Sends a text frame.
    pub fn send(&self, message: ClientMessage) -> Result<(), Closed> {
        self.0.send(Outgoing::Message(message)).map_err(|_| Closed)
    }

    /// Sends `bytes` as PTY input on `channel`.
    pub fn input(&self, channel: Channel, bytes: &[u8]) -> Result<(), Closed> {
        self.0
            .send(Outgoing::Input(channel, bytes.to_vec()))
            .map_err(|_| Closed)
    }
}

/// An open connection to `/v1/ws`.
#[derive(Debug)]
pub struct Socket {
    /// Sends to the daemon.
    pub sender: Sender,
    /// What the daemon sends, ending with [`Event::Closed`].
    pub events: mpsc::Receiver<Event>,
}

/// Connects to the daemon `info` describes and starts reading and writing on tokio tasks.
/// Runs inside a tokio runtime.
pub async fn open(info: &DaemonInfo) -> Result<Socket, WsError> {
    let mut request = info
        .ws_url()
        .into_client_request()
        .expect("ws://127.0.0.1:<port>/v1/ws is a valid request");
    let bearer = format!("Bearer {}", info.token)
        .parse()
        .map_err(|_| WsError::InvalidToken)?;
    request.headers_mut().insert("authorization", bearer);
    let (socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|err| WsError::Connect(Box::new(err)))?;
    let (mut sink, mut stream) = socket.split();

    let (outgoing, mut to_send) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(outgoing) = to_send.recv().await {
            if sink.send(encode(outgoing)).await.is_err() {
                return;
            }
        }
        let _ = sink.close().await;
    });

    let (events, received) = mpsc::channel(EVENT_BUFFER);
    tokio::spawn(async move {
        let reason = loop {
            match stream.next().await {
                Some(Ok(message)) => {
                    let event = match decode(message) {
                        Decoded::Event(event) => event,
                        Decoded::Skip => continue,
                        Decoded::Closed(reason) => break reason,
                    };
                    if events.send(event).await.is_err() {
                        return; // The client dropped its receiver.
                    }
                }
                Some(Err(err)) => break Some(err.to_string()),
                None => break None,
            }
        };
        let _ = events.send(Event::Closed(reason)).await;
    });

    Ok(Socket {
        sender: Sender(outgoing),
        events: received,
    })
}

/// The far end of a [`loopback`] socket: what the client sent, and a way to play the daemon.
#[derive(Debug)]
pub struct Remote {
    /// What the client sent, in order.
    pub sent: mpsc::UnboundedReceiver<Outgoing>,
    /// Delivers events to the client's [`Socket::events`].
    pub events: mpsc::Sender<Event>,
}

/// A [`Socket`] with no daemon behind it, for a client's tests: its [`Remote`] sees what is
/// sent and plays the daemon's side. Needs no runtime.
pub fn loopback() -> (Socket, Remote) {
    let (outgoing, sent) = mpsc::unbounded_channel();
    let (events, received) = mpsc::channel(EVENT_BUFFER);
    let socket = Socket {
        sender: Sender(outgoing),
        events: received,
    };
    (socket, Remote { sent, events })
}

fn encode(outgoing: Outgoing) -> Message {
    match outgoing {
        Outgoing::Message(message) => {
            Message::text(serde_json::to_string(&message).expect("a client message serializes"))
        }
        Outgoing::Input(channel, bytes) => Message::binary(frame(channel, &bytes)),
    }
}

/// What one frame from the daemon means to the client.
#[derive(Debug, PartialEq, Eq)]
enum Decoded {
    Event(Event),
    /// Pings, pongs, a binary frame too short for a channel id, or a text frame this client
    /// doesn't know (a newer daemon's).
    Skip,
    /// A close frame, with its reason if it gave one.
    Closed(Option<String>),
}

fn decode(message: Message) -> Decoded {
    match message {
        Message::Text(text) => match serde_json::from_str(&text) {
            Ok(message) => Decoded::Event(Event::Message(message)),
            Err(_) => Decoded::Skip,
        },
        Message::Binary(bytes) => match parse_frame(&bytes) {
            Some((channel, bytes)) => Decoded::Event(Event::Output(channel, bytes.to_vec())),
            None => Decoded::Skip,
        },
        Message::Close(frame) => Decoded::Closed(
            frame
                .map(|frame| frame.reason.to_string())
                .filter(|reason| !reason.is_empty()),
        ),
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => Decoded::Skip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod encode {
        use super::*;

        #[test]
        fn should_send_a_message_as_tagged_json() {
            let message = encode(Outgoing::Message(ClientMessage::Detach { channel: 3 }));

            assert_eq!(message, Message::text(r#"{"t":"detach","channel":3}"#));
        }

        #[test]
        fn should_frame_input_with_its_channel() {
            assert_eq!(
                encode(Outgoing::Input(3, b"ls".to_vec())),
                Message::binary(frame(3, b"ls"))
            );
        }
    }

    mod loopback {
        use super::*;

        #[test]
        fn should_hand_the_remote_what_was_sent() {
            let (socket, mut remote) = loopback();

            socket.sender.input(1, b"x").unwrap();

            assert_eq!(
                remote.sent.try_recv().unwrap(),
                Outgoing::Input(1, b"x".to_vec())
            );
        }

        #[test]
        fn should_report_closed_once_the_remote_is_gone() {
            let (socket, remote) = loopback();
            drop(remote);

            assert_eq!(socket.sender.input(1, b"x"), Err(Closed));
        }
    }

    mod decode {
        use super::*;
        use agentdeck_core::PaneId;
        use tungstenite::protocol::CloseFrame;
        use tungstenite::protocol::frame::coding::CloseCode;

        #[test]
        fn should_parse_a_server_message() {
            let pane = PaneId::from(uuid::Uuid::from_u128(7));
            let text = format!(r#"{{"t":"exit","pane":"{pane}","code":0}}"#);

            assert_eq!(
                decode(Message::text(text)),
                Decoded::Event(Event::Message(ServerMessage::Exit {
                    pane,
                    code: Some(0)
                }))
            );
        }

        #[test]
        fn should_skip_a_message_it_doesnt_know() {
            assert_eq!(
                decode(Message::text(r#"{"t":"from_the_future"}"#)),
                Decoded::Skip
            );
        }

        #[test]
        fn should_split_a_binary_frame_into_its_channel_and_bytes() {
            assert_eq!(
                decode(Message::binary(frame(3, b"hi"))),
                Decoded::Event(Event::Output(3, b"hi".to_vec()))
            );
        }

        #[test]
        fn should_skip_a_binary_frame_without_a_channel_id() {
            assert_eq!(decode(Message::binary(vec![0, 1])), Decoded::Skip);
        }

        #[test]
        fn should_keep_a_close_frames_reason() {
            let close = Message::Close(Some(CloseFrame {
                code: CloseCode::Away,
                reason: "shutting down".into(),
            }));

            assert_eq!(decode(close), Decoded::Closed(Some("shutting down".into())));
        }

        #[test]
        fn should_close_without_a_reason_when_none_was_given() {
            assert_eq!(decode(Message::Close(None)), Decoded::Closed(None));
        }
    }
}
