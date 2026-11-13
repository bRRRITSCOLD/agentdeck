//! The GUI's connection to the daemon (spec §2, §5): reach it, starting it if need be, open
//! its WebSocket, and pass on what it sends.
//!
//! [`Connection`] is an entity the views observe for its [`Status`] and subscribe to for
//! [`DaemonEvent`]s. How it connects is a [`Connector`], so tests hand it a fake.

use std::rc::Rc;
use std::sync::Arc;

use agentdeck_client::daemon;
use agentdeck_client::ws::{self, Socket};
use agentdeck_core::ws::{Channel, ServerMessage};
use gpui::{App, Context, EventEmitter, SharedString, Task};

use crate::api::Api;
use crate::runtime;

/// Reaches the daemon and opens its socket.
pub trait Connector {
    /// The daemon's API and its open socket, or why neither could be had.
    fn connect(&self, cx: &App) -> Task<anyhow::Result<(Arc<dyn Api>, Socket)>>;
}

/// The real [`Connector`]: `daemon.json`, starting `agentdeckd` when nothing answers.
pub struct DaemonConnector;

impl Connector for DaemonConnector {
    fn connect(&self, cx: &App) -> Task<anyhow::Result<(Arc<dyn Api>, Socket)>> {
        runtime::spawn(cx, async {
            let daemon = daemon::connect().await?;
            let socket = ws::open(&daemon.info).await?;
            Ok((Arc::new(daemon) as Arc<dyn Api>, socket))
        })
    }
}

/// A connected daemon: its API and the way to send on its socket.
#[derive(Clone)]
pub struct Session {
    /// The daemon's API.
    pub api: Arc<dyn Api>,
    /// Sends on its WebSocket.
    pub sender: ws::Sender,
}

/// Where the connection stands.
#[derive(Clone)]
pub enum Status {
    /// Reaching or starting the daemon.
    Connecting,
    /// Connected.
    Connected(Session),
    /// Couldn't connect, or the connection was lost; why, for the user.
    Failed(SharedString),
}

/// What the daemon sent on the socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonEvent {
    /// A text frame.
    Message(ServerMessage),
    /// Terminal bytes on a channel.
    Output(Channel, Vec<u8>),
}

/// The connection to the daemon.
pub struct Connection {
    connector: Rc<dyn Connector>,
    status: Status,
    /// Connects, then reads the socket; replaced on each [`Connection::connect`].
    task: Option<Task<()>>,
    /// The last channel handed out on this socket.
    last_channel: Channel,
}

impl EventEmitter<DaemonEvent> for Connection {}

impl Connection {
    /// A connection that starts connecting through `connector` at once.
    pub fn new(connector: Rc<dyn Connector>, cx: &mut Context<Self>) -> Self {
        let mut connection = Connection {
            connector,
            status: Status::Connecting,
            task: None,
            last_channel: 0,
        };
        connection.connect(cx);
        connection
    }

    /// Where the connection stands.
    pub fn status(&self) -> &Status {
        &self.status
    }

    /// A channel number no attachment on this socket has used yet.
    pub fn open_channel(&mut self) -> Channel {
        self.last_channel += 1;
        self.last_channel
    }

    /// Connects again from scratch, dropping any socket held now.
    pub fn connect(&mut self, cx: &mut Context<Self>) {
        self.last_channel = 0;
        self.set_status(Status::Connecting, cx);
        let connecting = self.connector.connect(cx);
        self.task = Some(cx.spawn(async move |this, cx| {
            let (api, socket) = match connecting.await {
                Ok(connected) => connected,
                Err(err) => {
                    let reason = format!("Couldn't reach agentdeckd: {err:#}");
                    let _ = this.update(cx, |this, cx| {
                        this.set_status(Status::Failed(reason.into()), cx);
                    });
                    return;
                }
            };
            let Socket { sender, mut events } = socket;
            let connected = Status::Connected(Session { api, sender });
            if this
                .update(cx, |this, cx| this.set_status(connected, cx))
                .is_err()
            {
                return;
            }
            let reason = loop {
                let event = match events.recv().await {
                    Some(ws::Event::Message(message)) => DaemonEvent::Message(message),
                    Some(ws::Event::Output(channel, bytes)) => DaemonEvent::Output(channel, bytes),
                    Some(ws::Event::Closed(reason)) => break reason,
                    None => break None,
                };
                if this.update(cx, |_, cx| cx.emit(event)).is_err() {
                    return;
                }
            };
            let reason = match reason {
                Some(reason) => format!("Lost the connection to agentdeckd: {reason}"),
                None => "Lost the connection to agentdeckd".to_owned(),
            };
            let _ = this.update(cx, |this, cx| {
                this.set_status(Status::Failed(reason.into()), cx);
            });
        }));
    }

    fn set_status(&mut self, status: Status, cx: &mut Context<Self>) {
        self.status = status;
        cx.notify();
    }
}

/// A scripted [`Connector`] for tests.
#[cfg(test)]
pub(crate) mod fake {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use agentdeck_client::ws::{Remote, loopback};

    use super::*;
    use crate::api::fake::FakeApi;

    /// What a connect resolves to.
    type Outcome = anyhow::Result<(Arc<dyn Api>, Socket)>;

    /// Answers each connect with the next scripted outcome, and counts the calls.
    #[derive(Default)]
    pub struct FakeConnector {
        outcomes: RefCell<VecDeque<Outcome>>,
        pub calls: RefCell<usize>,
    }

    impl FakeConnector {
        /// Scripts a failure with `reason`.
        pub fn then_fail(self, reason: &str) -> Self {
            self.outcomes
                .borrow_mut()
                .push_back(Err(anyhow::anyhow!(reason.to_owned())));
            self
        }

        /// Scripts a success to a daemon with nothing in it; the returned [`Remote`] plays
        /// the daemon's socket.
        pub fn then_connect(&self) -> Remote {
            self.then_connect_to(Arc::new(FakeApi::default()))
        }

        /// Scripts a success to `api`; the returned [`Remote`] plays the daemon's socket.
        pub fn then_connect_to(&self, api: Arc<FakeApi>) -> Remote {
            let (socket, remote) = loopback();
            self.outcomes.borrow_mut().push_back(Ok((api, socket)));
            remote
        }
    }

    impl Connector for FakeConnector {
        fn connect(&self, _cx: &App) -> Task<Outcome> {
            *self.calls.borrow_mut() += 1;
            let outcome = self.outcomes.borrow_mut().pop_front();
            Task::ready(outcome.expect("a scripted outcome for every connect"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use agentdeck_core::ws::Entity as Changed;
    use gpui::{AppContext as _, Entity, TestAppContext};

    use super::fake::FakeConnector;
    use super::*;

    fn connection(connector: &Rc<FakeConnector>, cx: &mut TestAppContext) -> Entity<Connection> {
        let connector: Rc<dyn Connector> = connector.clone();
        let connection = cx.update(|cx| cx.new(|cx| Connection::new(connector, cx)));
        cx.run_until_parked();
        connection
    }

    fn failure(connection: &Entity<Connection>, cx: &TestAppContext) -> Option<String> {
        connection.read_with(cx, |connection, _| match connection.status() {
            Status::Failed(reason) => Some(reason.to_string()),
            _ => None,
        })
    }

    fn is_connected(connection: &Entity<Connection>, cx: &TestAppContext) -> bool {
        connection.read_with(cx, |connection, _| {
            matches!(connection.status(), Status::Connected(_))
        })
    }

    mod connect {
        use super::*;

        #[gpui::test]
        fn should_be_connected_once_the_connector_succeeds(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let _remote = connector.then_connect();

            let connection = connection(&connector, cx);

            assert!(is_connected(&connection, cx));
        }

        #[gpui::test]
        fn should_fail_with_the_connectors_reason(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default().then_fail("no XDG_RUNTIME_DIR"));

            let connection = connection(&connector, cx);

            assert_eq!(
                failure(&connection, cx).as_deref(),
                Some("Couldn't reach agentdeckd: no XDG_RUNTIME_DIR")
            );
        }

        #[gpui::test]
        fn should_connect_again_when_asked_after_a_failure(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default().then_fail("refused"));
            let connection = connection(&connector, cx);
            let _remote = connector.then_connect();

            connection.update(cx, |connection, cx| connection.connect(cx));
            cx.run_until_parked();

            assert!(is_connected(&connection, cx));
            assert_eq!(*connector.calls.borrow(), 2);
        }
    }

    mod events {
        use super::*;

        #[gpui::test]
        fn should_pass_on_what_the_daemon_sends(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let remote = connector.then_connect();
            let connection = connection(&connector, cx);
            let seen = Rc::new(RefCell::new(Vec::new()));
            let _subscription = cx.update(|cx| {
                let seen = seen.clone();
                cx.subscribe(&connection, move |_, event: &DaemonEvent, _| {
                    seen.borrow_mut().push(event.clone());
                })
            });
            let changed = ServerMessage::Changed {
                entity: Changed::Task,
                id: "t".into(),
            };

            remote
                .events
                .try_send(ws::Event::Message(changed.clone()))
                .unwrap();
            remote
                .events
                .try_send(ws::Event::Output(3, b"hi".to_vec()))
                .unwrap();
            cx.run_until_parked();

            assert_eq!(
                *seen.borrow(),
                [
                    DaemonEvent::Message(changed),
                    DaemonEvent::Output(3, b"hi".to_vec())
                ]
            );
        }

        #[gpui::test]
        fn should_fail_with_the_reason_when_the_socket_closes(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let remote = connector.then_connect();
            let connection = connection(&connector, cx);

            remote
                .events
                .try_send(ws::Event::Closed(Some("shutting down".into())))
                .unwrap();
            cx.run_until_parked();

            assert_eq!(
                failure(&connection, cx).as_deref(),
                Some("Lost the connection to agentdeckd: shutting down")
            );
        }

        #[gpui::test]
        fn should_fail_when_the_socket_goes_away_without_a_reason(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let remote = connector.then_connect();
            let connection = connection(&connector, cx);

            drop(remote);
            cx.run_until_parked();

            assert_eq!(
                failure(&connection, cx).as_deref(),
                Some("Lost the connection to agentdeckd")
            );
        }
    }
}
