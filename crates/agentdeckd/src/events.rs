//! The daemon-wide events every WebSocket client gets, attached or not (spec §5): `state`,
//! `exit` and `changed`.

use std::fmt::Display;

use agentdeck_core::ws::{Entity, ServerMessage};
use tokio::sync::broadcast;

/// How many events a slow connection may fall behind before it misses some. One that does
/// gets `resync` and reads everything again over REST (spec §5).
const CAPACITY: usize = 1024;

/// A handle to the event bus; clones share it.
#[derive(Clone)]
pub struct Events(broadcast::Sender<ServerMessage>);

impl Events {
    /// A bus nobody listens to yet.
    pub fn new() -> Self {
        Self(broadcast::channel(CAPACITY).0)
    }

    /// Sends `event` to every connection.
    pub fn publish(&self, event: ServerMessage) {
        // No receivers is not an error: no client is connected.
        let _ = self.0.send(event);
    }

    /// `{"t":"changed"}`: an entity was created, changed or deleted.
    pub fn changed(&self, entity: Entity, id: impl Display) {
        self.publish(ServerMessage::Changed {
            entity,
            id: id.to_string(),
        });
    }

    /// A new connection's feed of events from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<ServerMessage> {
        self.0.subscribe()
    }
}
