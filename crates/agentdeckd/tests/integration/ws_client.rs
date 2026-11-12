//! `agentdeck_client::ws`, the GUI's socket, against the real daemon (spec §5).

use agentdeck_client::ws::{Event, Socket, WsError, open};
use agentdeck_core::ws::{ClientMessage, Entity, ServerMessage};
use tokio_tungstenite::tungstenite;

use crate::support::TestDaemon;
use crate::support::terminal::{DEADLINE, Place, sh};

/// `daemon.json` as `agentdeck_core` types it, with the daemon's port and token.
fn info(daemon: &TestDaemon, token: &str) -> agentdeck_core::DaemonInfo {
    agentdeck_core::DaemonInfo {
        port: daemon.info.port,
        token: token.into(),
        pid: daemon.info.pid,
        version: daemon.info.version.clone(),
    }
}

/// The first event `pick` accepts, failing the test after [`DEADLINE`] or on `Closed`.
async fn until<T>(socket: &mut Socket, mut pick: impl FnMut(Event) -> Option<T>) -> T {
    let found = tokio::time::timeout(DEADLINE, async {
        loop {
            match socket.events.recv().await.expect("the socket's events") {
                Event::Closed(reason) => panic!("the socket closed: {reason:?}"),
                event => {
                    if let Some(found) = pick(event) {
                        return found;
                    }
                }
            }
        }
    });
    found.await.expect("timed out waiting for an event")
}

mod open {
    use super::*;

    #[tokio::test]
    async fn should_refuse_a_wrong_token_401() {
        let daemon = TestDaemon::start().await;

        let result = open(&info(&daemon, "nope")).await;

        let Err(WsError::Connect(err)) = result else {
            panic!("expected a refused upgrade, got {result:?}");
        };
        assert!(matches!(*err, tungstenite::Error::Http(ref r) if r.status() == 401));
    }

    #[tokio::test]
    async fn should_deliver_changed_as_a_message() {
        let daemon = TestDaemon::start().await;
        let mut socket = open(&info(&daemon, &daemon.info.token))
            .await
            .expect("open the socket");

        let place = Place::new(&daemon).await;

        let id = place.workspace.id.to_string();
        until(&mut socket, |event| match event {
            Event::Message(ServerMessage::Changed {
                entity: Entity::Workspace,
                id: changed,
            }) if changed == id => Some(()),
            _ => None,
        })
        .await;
    }

    #[tokio::test]
    async fn should_attach_and_carry_input_and_output_on_the_channel() {
        let daemon = TestDaemon::start().await;
        let place = Place::new(&daemon).await;
        let pane = place.spawn(&daemon.client, &sh("cat")).await;
        let mut socket = open(&info(&daemon, &daemon.info.token))
            .await
            .expect("open the socket");

        socket
            .sender
            .send(ClientMessage::Attach {
                pane: pane.id,
                channel: 5,
            })
            .expect("send attach");
        until(&mut socket, |event| match event {
            Event::Message(ServerMessage::Attached { channel: 5, .. }) => Some(()),
            _ => None,
        })
        .await;
        socket.sender.input(5, b"hello\n").expect("send input");

        let mut seen = Vec::new();
        until(&mut socket, |event| match event {
            Event::Output(5, bytes) => {
                seen.extend_from_slice(&bytes);
                String::from_utf8_lossy(&seen)
                    .contains("hello")
                    .then_some(())
            }
            _ => None,
        })
        .await;
    }

    #[tokio::test]
    async fn should_end_with_closed_when_the_daemon_stops() {
        let daemon = TestDaemon::start().await;
        let mut socket = open(&info(&daemon, &daemon.info.token))
            .await
            .expect("open the socket");

        drop(daemon);

        let closed = tokio::time::timeout(DEADLINE, async {
            loop {
                if let Event::Closed(_) = socket.events.recv().await.expect("an event") {
                    return;
                }
            }
        });
        closed.await.expect("timed out waiting for Closed");
        assert!(socket.events.recv().await.is_none(), "nothing after Closed");
    }
}
