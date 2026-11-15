//! The WebSocket at `/v1/ws` (spec §5): one per client, carrying terminal I/O for every pane
//! it attaches, plus the daemon-wide `state`, `exit` and `changed` events.
//!
//! Each connection runs two tasks. The reader (this one) handles client messages and
//! forwards daemon events; the writer drains the connection's outbox into the socket. Panes
//! push their output straight into the outbox, under their own lock, so what a pane sends a
//! connection stays in order.

use std::collections::HashMap;
use std::sync::Arc;

use agentdeck_core::ws::{Channel, ClientMessage, ServerMessage, frame, parse_frame};
use dropshot::{RequestContext, WebsocketEndpointResult, WebsocketUpgrade, endpoint};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::Role;

use super::{ApiContext, Authorized};
use crate::terminal::Terminals;
use crate::terminal::fanout::{Outbox, Outgoing, Size, SubscriberId};
use crate::terminal::pane::Pane;

/// The terminal and events WebSocket. Binary frames are `[u32 channel][bytes]`; text frames
/// are JSON tagged with `t`. See spec §5 for the messages.
///
/// An endpoint over [`WebsocketUpgrade`] rather than a `#[channel]`: a channel's body runs
/// only once the upgrade completes, after the client has seen the 101. Subscribing here,
/// before the 101 goes out, means every event published after the client sees the
/// connection open reaches it.
///
/// Unpublished: OpenAPI can't describe the messages, so a generated client would get only the
/// bare upgrade. The GUI, the one client that opens it, speaks the protocol through
/// `agentdeck_core::ws` over `agentdeck_client::ws`, whose bounded event channel lets the
/// daemon see a client that stops reading fall behind and send it `lagged` (spec §5).
#[endpoint { method = GET, path = "/v1/ws", unpublished = true }]
pub(crate) async fn ws(
    rqctx: RequestContext<ApiContext>,
    _auth: Authorized,
    upgrade: WebsocketUpgrade,
) -> WebsocketEndpointResult {
    let context = rqctx.context();
    let events = context.events.subscribe();
    let session = Session::new(context.terminals.clone());
    upgrade.handle(move |upgraded| async move {
        let socket =
            WebSocketStream::from_raw_socket(upgraded.into_inner(), Role::Server, None).await;
        session.run(socket, events).await;
        Ok(())
    })
}

/// One connection's attachments.
struct Session {
    terminals: Terminals,
    attached: HashMap<Channel, (Arc<Pane>, SubscriberId)>,
}

impl Session {
    fn new(terminals: Terminals) -> Self {
        Self {
            terminals,
            attached: HashMap::new(),
        }
    }

    async fn run<S>(
        mut self,
        socket: WebSocketStream<S>,
        mut events: broadcast::Receiver<ServerMessage>,
    ) where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (mut sink, mut stream) = socket.split();
        let (outbox, mut queue) = mpsc::unbounded_channel::<Outgoing>();
        let mut writer = tokio::spawn(async move {
            while let Some(outgoing) = queue.recv().await {
                let (message, written) = match outgoing {
                    Outgoing::Message(message) => (Message::text(json(&message)), None),
                    Outgoing::Bytes {
                        channel,
                        bytes,
                        backlog,
                    } => (
                        Message::binary(frame(channel, &bytes)),
                        backlog.map(|backlog| (backlog, bytes.len())),
                    ),
                };
                if sink.send(message).await.is_err() {
                    break;
                }
                if let Some((backlog, len)) = written {
                    backlog.sent(len);
                }
            }
            let _ = sink.close().await;
        });

        loop {
            tokio::select! {
                incoming = stream.next() => match incoming {
                    Some(Ok(Message::Text(text))) => self.on_text(&text, &outbox),
                    Some(Ok(Message::Binary(bytes))) => self.on_bytes(&bytes, &outbox),
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
                event = events.recv() => match event {
                    Ok(event) => send(&outbox, event),
                    // A missed event is refetchable over REST; keep going.
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = &mut writer => break,
            }
        }
        for (pane, subscriber) in self.attached.into_values() {
            pane.detach(subscriber);
        }
    }

    fn on_text(&mut self, text: &str, outbox: &Outbox) {
        let message = match serde_json::from_str::<ClientMessage>(text) {
            Ok(message) => message,
            Err(err) => {
                return send(
                    outbox,
                    error(
                        None,
                        "invalid_message",
                        format!("not a client message: {err}"),
                    ),
                );
            }
        };
        match message {
            ClientMessage::Attach { pane: id, channel } => {
                self.detach(channel);
                let Some(pane) = self.terminals.get(id) else {
                    return send(
                        outbox,
                        error(Some(channel), "not_found", format!("no live pane {id}")),
                    );
                };
                let subscriber = self.terminals.subscriber();
                pane.attach(subscriber, channel, outbox.clone());
                self.attached.insert(channel, (pane, subscriber));
            }
            ClientMessage::Detach { channel } => {
                if !self.detach(channel) {
                    send(outbox, not_attached(channel));
                }
            }
            ClientMessage::Resize {
                channel,
                cols,
                rows,
            } => match self.attached.get(&channel) {
                Some((pane, subscriber)) => {
                    pane.request_resize(*subscriber, Size { cols, rows });
                }
                None => send(outbox, not_attached(channel)),
            },
            ClientMessage::Primary { channel } => match self.attached.get(&channel) {
                Some((pane, subscriber)) => pane.make_primary(*subscriber),
                None => send(outbox, not_attached(channel)),
            },
        }
    }

    /// Input typed on a channel.
    fn on_bytes(&self, bytes: &[u8], outbox: &Outbox) {
        let Some((channel, bytes)) = parse_frame(bytes) else {
            return send(
                outbox,
                error(
                    None,
                    "invalid_message",
                    "a binary frame needs a channel".into(),
                ),
            );
        };
        match self.attached.get(&channel) {
            Some((pane, _)) => pane.type_bytes(bytes),
            None => send(outbox, not_attached(channel)),
        }
    }

    /// Ends the attachment on `channel`; false when there was none.
    fn detach(&mut self, channel: Channel) -> bool {
        match self.attached.remove(&channel) {
            Some((pane, subscriber)) => {
                pane.detach(subscriber);
                true
            }
            None => false,
        }
    }
}

fn send(outbox: &Outbox, message: ServerMessage) {
    // The writer has stopped only when the socket has; the reader loop ends right after.
    let _ = outbox.send(Outgoing::Message(message));
}

fn json(message: &ServerMessage) -> String {
    serde_json::to_string(message).expect("a server message always serializes")
}

fn error(channel: Option<Channel>, code: &str, message: String) -> ServerMessage {
    ServerMessage::Error {
        channel,
        code: code.to_owned(),
        message,
    }
}

fn not_attached(channel: Channel) -> ServerMessage {
    error(
        Some(channel),
        "not_attached",
        format!("nothing is attached on channel {channel}"),
    )
}
