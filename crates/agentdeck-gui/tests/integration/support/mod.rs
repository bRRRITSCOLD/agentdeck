//! The harness: a private D-Bus session bus, and a notification server on it that records
//! what it is asked to show. Neither touches the user's own session bus.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notify {
    pub app_name: String,
    pub summary: String,
    pub body: String,
    /// Action keys and labels, alternating.
    pub actions: Vec<String>,
}

/// `org.freedesktop.Notifications` on a [`PrivateBus`]: answers each `Notify` with id 1 and
/// passes the call on.
pub struct FakeServer {
    connection: zbus::Connection,
    /// Each `Notify` received, in order.
    pub calls: mpsc::UnboundedReceiver<Notify>,
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
                let (app_name, _, _, summary, body, actions, _, _): Args =
                    message.body().deserialize().expect("Notify's arguments");
                serving
                    .reply(&header, &1u32)
                    .await
                    .expect("answering Notify");
                let _ = sender.send(Notify {
                    app_name,
                    summary,
                    body,
                    actions,
                });
            }
        });
        FakeServer { connection, calls }
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
}
