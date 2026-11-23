//! The GUI's connection to the daemon (spec §2, §5): reach it, starting it if need be, open
//! its WebSocket, and pass on what it sends.
//!
//! [`Connection`] is an entity the views observe for its [`Status`] and subscribe to for
//! [`DaemonEvent`]s. It says when the daemon it reached predates the app, and restarts it
//! when the user asks, never on its own. How it connects and restarts is a [`Connector`], so
//! tests hand it a fake.

use std::rc::Rc;
use std::sync::Arc;

use agentdeck_client::daemon;
use agentdeck_client::ws::{self, Socket};
use agentdeck_core::DaemonInfo;
use agentdeck_core::ws::{Channel, ServerMessage};
use gpui::{App, Context, EventEmitter, SharedString, Task};

use crate::api::Api;
use crate::runtime;

/// A reached daemon: its API, its open socket and the `daemon.json` it was reached through.
pub struct Link {
    /// The daemon's API.
    pub api: Arc<dyn Api>,
    /// Its WebSocket.
    pub socket: Socket,
    /// What `daemon.json` held: which daemon this is, and its API version.
    pub daemon: DaemonInfo,
    /// Whether the daemon still runs from a binary since rebuilt or reinstalled (spec §2).
    pub binary_replaced: bool,
}

/// Reaches the daemon and opens its socket.
pub trait Connector {
    /// The daemon, or why it couldn't be reached.
    fn connect(&self, cx: &App) -> Task<anyhow::Result<Link>>;

    /// Stops `daemon`, then reaches the new one that is started in its place.
    fn restart(&self, daemon: &DaemonInfo, cx: &App) -> Task<anyhow::Result<Link>>;
}

/// The real [`Connector`]: `daemon.json`, starting `agentdeckd` when nothing answers.
pub struct DaemonConnector;

impl DaemonConnector {
    /// Opens `daemon`'s socket.
    async fn link(daemon: daemon::Daemon) -> anyhow::Result<Link> {
        let socket = ws::open(&daemon.info).await?;
        let info = daemon.info.clone();
        let binary_replaced = daemon::binary_replaced(info.pid);
        Ok(Link {
            api: Arc::new(daemon),
            socket,
            daemon: info,
            binary_replaced,
        })
    }
}

impl Connector for DaemonConnector {
    fn connect(&self, cx: &App) -> Task<anyhow::Result<Link>> {
        runtime::spawn(cx, async { Self::link(daemon::connect().await?).await })
    }

    fn restart(&self, daemon: &DaemonInfo, cx: &App) -> Task<anyhow::Result<Link>> {
        let daemon = daemon.clone();
        runtime::spawn(cx, async move {
            Self::link(daemon::restart(&daemon).await?).await
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
    /// Stopping a daemon older than the app and starting a new one, as the user asked.
    Restarting,
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
    /// The daemon reached, while connected.
    daemon: Option<DaemonInfo>,
    /// Whether that daemon runs from a binary since rebuilt or reinstalled.
    binary_replaced: bool,
    /// Connects, then reads the socket; replaced on each [`Connection::connect`] and
    /// [`Connection::restart`].
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
            daemon: None,
            binary_replaced: false,
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

    /// Whether the daemon reached is older than the app, so it may lack what the app asks of
    /// it (spec §2): its API version is lower than the app's, or it still runs from a binary
    /// that has since been rebuilt or reinstalled.
    pub fn daemon_predates_app(&self) -> bool {
        self.daemon
            .as_ref()
            .is_some_and(|daemon| self.binary_replaced || daemon.predates_client())
    }

    /// A channel number no attachment on this socket has used yet.
    pub fn open_channel(&mut self) -> Channel {
        self.last_channel += 1;
        self.last_channel
    }

    /// Connects again from scratch, dropping any socket held now.
    pub fn connect(&mut self, cx: &mut Context<Self>) {
        let connecting = self.connector.connect(cx);
        self.follow(
            Status::Connecting,
            connecting,
            "Couldn't reach agentdeckd",
            cx,
        );
    }

    /// Stops the daemon reached and connects to a new one, which resumes its panes (spec §6).
    /// Only while connected; the old socket's closing isn't a lost connection.
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        let Some(daemon) = self.daemon.clone() else {
            return;
        };
        let restarting = self.connector.restart(&daemon, cx);
        self.follow(
            Status::Restarting,
            restarting,
            "Couldn't restart agentdeckd",
            cx,
        );
    }

    /// Shows `status` until `connecting` reaches a daemon, then reads its socket, dropping
    /// the one held now. A failure reads `failed: <why>`.
    fn follow(
        &mut self,
        status: Status,
        connecting: Task<anyhow::Result<Link>>,
        failed: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.last_channel = 0;
        self.daemon = None;
        self.set_status(status, cx);
        self.task = Some(cx.spawn(async move |this, cx| {
            let Link {
                api,
                socket,
                daemon,
                binary_replaced,
            } = match connecting.await {
                Ok(link) => link,
                Err(err) => {
                    let reason = format!("{failed}: {err:#}");
                    let _ = this.update(cx, |this, cx| {
                        this.set_status(Status::Failed(reason.into()), cx);
                    });
                    return;
                }
            };
            let Socket { sender, mut events } = socket;
            let connected = Status::Connected(Session { api, sender });
            if this
                .update(cx, |this, cx| {
                    this.daemon = Some(daemon);
                    this.binary_replaced = binary_replaced;
                    this.set_status(connected, cx);
                })
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
                this.daemon = None;
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
    use agentdeck_core::API_VERSION;

    use super::*;
    use crate::api::fake::FakeApi;

    /// What a connect or a restart resolves to.
    type Outcome = anyhow::Result<Link>;

    /// Answers each connect and restart with the next scripted outcome, and counts the calls.
    #[derive(Default)]
    pub struct FakeConnector {
        outcomes: RefCell<VecDeque<Outcome>>,
        pub calls: RefCell<usize>,
        /// The daemon each restart was asked to stop.
        pub restarts: RefCell<Vec<DaemonInfo>>,
    }

    /// A `daemon.json` at `api_version`.
    fn daemon(api_version: u32) -> DaemonInfo {
        DaemonInfo {
            port: 1,
            token: "fake".into(),
            pid: 1,
            version: "0.1.0".into(),
            api_version,
        }
    }

    impl FakeConnector {
        /// Scripts a failure with `reason`.
        pub fn then_fail(self, reason: &str) -> Self {
            self.push_failure(reason);
            self
        }

        /// Scripts a failure with `reason`, on a shared connector.
        pub fn push_failure(&self, reason: &str) {
            self.outcomes
                .borrow_mut()
                .push_back(Err(anyhow::anyhow!(reason.to_owned())));
        }

        /// Scripts a success to a daemon with nothing in it; the returned [`Remote`] plays
        /// the daemon's socket.
        pub fn then_connect(&self) -> Remote {
            self.then_connect_to(Arc::new(FakeApi::default()))
        }

        /// Scripts a success to `api`; the returned [`Remote`] plays the daemon's socket.
        pub fn then_connect_to(&self, api: Arc<FakeApi>) -> Remote {
            self.push(api, daemon(API_VERSION))
        }

        /// Scripts a success to a daemon with nothing in it, from before the app's API
        /// version.
        pub fn then_connect_older(&self) -> Remote {
            self.then_connect_older_to(Arc::new(FakeApi::default()))
        }

        /// Scripts a success to `api`, from before the app's API version.
        pub fn then_connect_older_to(&self, api: Arc<FakeApi>) -> Remote {
            self.push(api, daemon(0))
        }

        /// Scripts a success to a daemon at the app's API version whose binary has since been
        /// rebuilt.
        pub fn then_connect_rebuilt(&self) -> Remote {
            self.push_link(Arc::new(FakeApi::default()), daemon(API_VERSION), true)
        }

        fn push(&self, api: Arc<FakeApi>, daemon: DaemonInfo) -> Remote {
            self.push_link(api, daemon, false)
        }

        fn push_link(
            &self,
            api: Arc<FakeApi>,
            daemon: DaemonInfo,
            binary_replaced: bool,
        ) -> Remote {
            let (socket, remote) = loopback();
            self.outcomes.borrow_mut().push_back(Ok(Link {
                api,
                socket,
                daemon,
                binary_replaced,
            }));
            remote
        }

        fn next(&self) -> Task<Outcome> {
            let outcome = self.outcomes.borrow_mut().pop_front();
            Task::ready(outcome.expect("a scripted outcome for every connect and restart"))
        }
    }

    impl Connector for FakeConnector {
        fn connect(&self, _cx: &App) -> Task<Outcome> {
            *self.calls.borrow_mut() += 1;
            self.next()
        }

        fn restart(&self, daemon: &DaemonInfo, _cx: &App) -> Task<Outcome> {
            self.restarts.borrow_mut().push(daemon.clone());
            self.next()
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

    mod older_daemon {
        use super::*;

        fn predates_app(connection: &Entity<Connection>, cx: &TestAppContext) -> bool {
            connection.read_with(cx, |connection, _| connection.daemon_predates_app())
        }

        #[gpui::test]
        fn should_say_a_daemon_older_than_the_app_is(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let _remote = connector.then_connect_older();

            let connection = connection(&connector, cx);

            assert!(is_connected(&connection, cx));
            assert!(predates_app(&connection, cx));
        }

        #[gpui::test]
        fn should_say_a_daemon_running_from_a_since_rebuilt_binary_is(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let _remote = connector.then_connect_rebuilt();

            let connection = connection(&connector, cx);

            assert!(is_connected(&connection, cx));
            assert!(predates_app(&connection, cx));
        }

        #[gpui::test]
        fn should_not_say_so_of_a_daemon_as_new_as_the_app(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let _remote = connector.then_connect();

            let connection = connection(&connector, cx);

            assert!(!predates_app(&connection, cx));
        }

        #[gpui::test]
        fn should_never_restart_it_unasked(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let _remote = connector.then_connect_older();

            let _connection = connection(&connector, cx);
            cx.run_until_parked();

            assert!(connector.restarts.borrow().is_empty());
        }

        mod restart {
            use super::*;

            #[gpui::test]
            fn should_restart_the_daemon_it_reached_and_connect_to_the_new_one(
                cx: &mut TestAppContext,
            ) {
                let connector = Rc::new(FakeConnector::default());
                let _old = connector.then_connect_older();
                let connection = connection(&connector, cx);
                let _new = connector.then_connect();

                connection.update(cx, |connection, cx| connection.restart(cx));
                cx.run_until_parked();

                let restarted: Vec<u32> = connector
                    .restarts
                    .borrow()
                    .iter()
                    .map(|daemon| daemon.api_version)
                    .collect();
                assert_eq!(restarted, [0]);
                assert!(is_connected(&connection, cx));
                assert!(!predates_app(&connection, cx));
            }

            #[gpui::test]
            fn should_say_it_is_restarting_until_the_new_daemon_answers(cx: &mut TestAppContext) {
                let connector = Rc::new(FakeConnector::default());
                let _old = connector.then_connect_older();
                let connection = connection(&connector, cx);
                let _new = connector.then_connect();

                connection.update(cx, |connection, cx| connection.restart(cx));

                let restarting = connection.read_with(cx, |connection, _| {
                    matches!(connection.status(), Status::Restarting)
                });
                assert!(restarting);
            }

            #[gpui::test]
            fn should_not_take_the_old_socket_closing_for_a_lost_connection(
                cx: &mut TestAppContext,
            ) {
                let connector = Rc::new(FakeConnector::default());
                let old = connector.then_connect_older();
                let connection = connection(&connector, cx);
                let _new = connector.then_connect();

                connection.update(cx, |connection, cx| connection.restart(cx));
                let _ = old
                    .events
                    .try_send(ws::Event::Closed(Some("shutting down".into())));
                drop(old);
                cx.run_until_parked();

                assert!(is_connected(&connection, cx));
            }

            #[gpui::test]
            fn should_fail_with_the_reason_when_the_restart_fails(cx: &mut TestAppContext) {
                let connector = Rc::new(FakeConnector::default());
                let _old = connector.then_connect_older();
                let connection = connection(&connector, cx);
                connector.push_failure("agentdeckd (pid 7) was still running");

                connection.update(cx, |connection, cx| connection.restart(cx));
                cx.run_until_parked();

                assert_eq!(
                    failure(&connection, cx).as_deref(),
                    Some("Couldn't restart agentdeckd: agentdeckd (pid 7) was still running")
                );
            }
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
                entity: Changed::PullRequest,
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
