//! The harness: a private D-Bus session bus, a notification server on it that records what it
//! is asked to show, and the app side, a child process whose session bus is the private one.
//! None of them touches the user's own session bus.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Read as _};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures_util::StreamExt as _;
use tempfile::TempDir;
use tokio::sync::mpsc;
use zbus::OwnedVariant;

/// The notification server's object path and interface.
const PATH: &str = "/org/freedesktop/Notifications";
const INTERFACE: &str = "org.freedesktop.Notifications";

/// A `dbus-daemon` of the test's own, listening in a temp dir. Dropping it kills the daemon,
/// then removes the dir, even when the test failed.
pub struct PrivateBus {
    daemon: Child,
    address: String,
    // Removed after Drop has killed the daemon.
    dir: TempDir,
}

impl PrivateBus {
    /// Starts the bus. A missing `dbus-daemon` fails the test: CI installs it.
    pub fn start() -> PrivateBus {
        let dir = tempfile::tempdir().expect("a temp dir");
        let config = dir.path().join("session.conf");
        std::fs::write(&config, config_for(dir.path())).expect("writing the bus config");
        let mut daemon = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("dbus-daemon runs (install the dbus-daemon package)");
        let mut address = String::new();
        let stdout = daemon.stdout.take().expect("its stdout");
        BufReader::new(stdout)
            .read_line(&mut address)
            .expect("dbus-daemon prints its address");
        let address = address.trim().to_owned();
        assert!(!address.is_empty(), "dbus-daemon printed no address");
        PrivateBus {
            daemon,
            address,
            dir,
        }
    }

    /// Where clients connect: `unix:path=…`.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The temp dir it listens in, which also stands in for `$XDG_RUNTIME_DIR`.
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// A session bus that listens in `dir`, lets anyone own any name and send anything, and
/// starts no services.
fn config_for(dir: &Path) -> String {
    format!(
        r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-BUS Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:dir={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
        dir.display()
    )
}

/// A `Notify` call, as the server received it.
#[derive(Debug, Clone, PartialEq)]
pub struct Notify {
    /// The unique bus name of the connection that called: one per connection.
    pub sender: String,
    /// The id the server answered with.
    pub id: u32,
    pub app_name: String,
    pub replaces_id: u32,
    pub icon: String,
    pub summary: String,
    pub body: String,
    /// Action keys and labels, alternating.
    pub actions: Vec<String>,
    pub hints: HashMap<String, OwnedVariant>,
    pub timeout: i32,
}

/// `org.freedesktop.Notifications` on a [`PrivateBus`]: answers each `Notify` with the next id,
/// from 1, and passes the call on.
pub struct FakeServer {
    connection: zbus::Connection,
    /// Each `Notify` received, in order.
    calls: mpsc::UnboundedReceiver<Notify>,
}

impl FakeServer {
    /// Owns the notifications name on the bus at `address` and starts answering.
    pub async fn serve(address: &str) -> FakeServer {
        let connection = zbus::connection::Builder::address(address)
            .build()
            .await
            .expect("connecting to the private bus");
        connection
            .request_name(INTERFACE)
            .await
            .expect("owning org.freedesktop.Notifications");
        let (sender, calls) = mpsc::unbounded_channel();
        let serving = connection.clone();
        tokio::spawn(async move {
            let mut messages = zbus::MessageStream::from(&serving);
            let mut next_id = 0u32;
            while let Some(Ok(message)) = messages.next().await {
                let header = message.header();
                let notify = header.message_type() == zbus::message::Type::MethodCall
                    && header
                        .member()
                        .is_some_and(|member| member.as_str() == "Notify");
                if !notify {
                    continue;
                }
                type Args = (
                    String,
                    u32,
                    String,
                    String,
                    String,
                    Vec<String>,
                    HashMap<String, OwnedVariant>,
                    i32,
                );
                let (app_name, replaces_id, icon, summary, body, actions, hints, timeout): Args =
                    message.body().deserialize().expect("Notify's arguments");
                next_id += 1;
                serving
                    .reply(&header, &next_id)
                    .await
                    .expect("answering Notify");
                let _ = sender.send(Notify {
                    sender: header.sender().expect("a sender").to_string(),
                    id: next_id,
                    app_name,
                    replaces_id,
                    icon,
                    summary,
                    body,
                    actions,
                    hints,
                    timeout,
                });
            }
        });
        FakeServer { connection, calls }
    }

    /// The next `Notify` call; it must arrive within [`TIMEOUT`].
    pub async fn next_call(&mut self) -> Notify {
        tokio::time::timeout(TIMEOUT, self.calls.recv())
            .await
            .expect("Notify reaches the server in time")
            .expect("the server is still running")
    }

    /// Reports that the user invoked `action` on notification `id`, as a server does on a
    /// click (`default` for the body).
    pub async fn invoke(&self, id: u32, action: &str) {
        self.connection
            .emit_signal(
                None::<zbus::names::BusName<'_>>,
                PATH,
                INTERFACE,
                "ActionInvoked",
                &(id, action),
            )
            .await
            .expect("emitting ActionInvoked");
    }

    /// Reports that notification `id` closed for `reason` (2: dismissed by the user), as a
    /// server does when one expires or is dismissed.
    pub async fn close(&self, id: u32, reason: u32) {
        self.connection
            .emit_signal(
                None::<zbus::names::BusName<'_>>,
                PATH,
                INTERFACE,
                "NotificationClosed",
                &(id, reason),
            )
            .await
            .expect("emitting NotificationClosed");
    }
}

/// How long each side waits for the other.
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// Set in the app side's environment: the tags it shows, comma-separated.
pub const APP_SIDE: &str = "AGENTDECK_NOTIFY_APP_SIDE";

/// This test binary re-run as a child to run `test`'s app side, with `DBUS_SESSION_BUS_ADDRESS`
/// set to `bus` for the child only, never here. Dropping it kills the child, even when the test
/// failed.
pub struct AppSide(Child);

impl AppSide {
    /// Starts `test` (its full path) in a child that shows `tags`.
    pub fn spawn(test: &str, bus: &PrivateBus, tags: &[&str]) -> AppSide {
        let child = Command::new(std::env::current_exe().expect("the test binary"))
            .args([test, "--exact", "--nocapture"])
            .env(APP_SIDE, tags.join(","))
            .env("DBUS_SESSION_BUS_ADDRESS", bus.address())
            .env("XDG_RUNTIME_DIR", bus.dir())
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .stdout(Stdio::piped())
            .spawn()
            .expect("starting the app side");
        AppSide(child)
    }

    /// How many threads it runs now.
    pub fn threads(&self) -> usize {
        std::fs::read_dir(format!("/proc/{}/task", self.0.id()))
            .expect("the app side's threads")
            .count()
    }

    /// Waits for it to exit, and answers the clicks it printed, `clicked <tag>`, in order. It
    /// must exit, and succeed.
    pub fn clicks(mut self) -> Vec<String> {
        let started = Instant::now();
        let status = loop {
            if let Some(status) = self.0.try_wait().expect("polling the app side") {
                break status;
            }
            assert!(started.elapsed() < TIMEOUT, "the app side never exited");
            std::thread::sleep(Duration::from_millis(50));
        };
        let mut stdout = String::new();
        (self.0.stdout.take().expect("its stdout"))
            .read_to_string(&mut stdout)
            .expect("reading the app side's output");
        assert!(status.success(), "the app side failed: {status}\n{stdout}");
        (stdout.lines())
            .filter(|line| line.starts_with("clicked "))
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for AppSide {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
