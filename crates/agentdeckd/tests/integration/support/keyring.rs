//! A real Secret Service for the keyring tests (spec §12): `gnome-keyring-daemon`, unlocked,
//! on a private `dbus-daemon` session bus of the test's own, so the user's own keyring is never
//! touched. `script/install-linux-deps.sh` installs both.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use zbus::{OwnedObjectPath, OwnedVariant};

/// How long `gnome-keyring-daemon` gets to take its name on the bus.
const STARTUP: Duration = Duration::from_secs(10);

/// The Secret Service's well-known name, object and interfaces.
const SECRETS: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE_INTERFACE: &str = "org.freedesktop.Secret.Service";
const ITEM_INTERFACE: &str = "org.freedesktop.Secret.Item";

/// A private session bus with an unlocked `gnome-keyring-daemon` on it. Dropping it kills
/// both, then removes their dir, even when the test failed.
pub struct SecretService {
    keyring: Child,
    bus: Child,
    address: String,
    // Removed after Drop has killed both.
    _dir: TempDir,
}

impl SecretService {
    /// Starts the bus and the keyring, and waits until the keyring serves. A missing
    /// `dbus-daemon` or `gnome-keyring-daemon` fails the test: CI installs both.
    pub async fn start() -> SecretService {
        let dir = tempfile::tempdir().expect("a temp dir");
        let run = dir.path().join("run");
        let home = dir.path().join("home");
        for path in [&run, &home] {
            std::fs::create_dir_all(path).expect("creating the keyring's dirs");
        }
        let config = dir.path().join("session.conf");
        std::fs::write(&config, bus_config(&run)).expect("writing the bus config");
        let mut bus = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("dbus-daemon runs (install the dbus-daemon package)");
        let mut address = String::new();
        BufReader::new(bus.stdout.take().expect("its stdout"))
            .read_line(&mut address)
            .expect("dbus-daemon prints its address");
        let address = address.trim().to_owned();
        assert!(!address.is_empty(), "dbus-daemon printed no address");

        // `--unlock` reads the login keyring's password from stdin, creating the keyring
        // (unlocked, the default collection) under $XDG_DATA_HOME on first use.
        let mut keyring = Command::new("gnome-keyring-daemon")
            .args(["--foreground", "--unlock", "--components=secrets"])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &home)
            .env("XDG_RUNTIME_DIR", &run)
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("DBUS_SESSION_BUS_ADDRESS", &address)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("gnome-keyring-daemon runs (install the gnome-keyring package)");
        keyring
            .stdin
            .take()
            .expect("its stdin")
            .write_all(b"agentdeck-test")
            .expect("unlocking the keyring");
        let service = SecretService {
            keyring,
            bus,
            address,
            _dir: dir,
        };
        service.wait_until_serving().await;
        service
    }

    /// The bus address, for the daemon's `DBUS_SESSION_BUS_ADDRESS`.
    pub fn address(&self) -> &str {
        &self.address
    }

    async fn connection(&self) -> zbus::Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .build()
            .await
            .expect("connecting to the private bus")
    }

    async fn wait_until_serving(&self) {
        let connection = self.connection().await;
        let deadline = Instant::now() + STARTUP;
        loop {
            let owned: bool = connection
                .call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "NameHasOwner",
                    &(SECRETS,),
                )
                .await
                .and_then(|reply| reply.body().deserialize())
                .unwrap_or(false);
            if owned {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "gnome-keyring-daemon didn't take {SECRETS} in {STARTUP:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The `username` of every item under the service `agentdeck`, sorted: the daemon's entry
    /// names, as the Secret Service holds them.
    pub async fn agentdeck_entries(&self) -> Vec<String> {
        let connection = self.connection().await;
        let (unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = connection
            .call_method(
                Some(SECRETS),
                SERVICE_PATH,
                Some(SERVICE_INTERFACE),
                "SearchItems",
                &(HashMap::from([("service", "agentdeck")]),),
            )
            .await
            .and_then(|reply| reply.body().deserialize())
            .expect("SearchItems");
        let mut names = Vec::new();
        for path in unlocked.into_iter().chain(locked) {
            let attributes: OwnedVariant = connection
                .call_method(
                    Some(SECRETS),
                    &path,
                    Some("org.freedesktop.DBus.Properties"),
                    "Get",
                    &(ITEM_INTERFACE, "Attributes"),
                )
                .await
                .and_then(|reply| reply.body().deserialize())
                .expect("the item's attributes");
            let attributes =
                HashMap::<String, String>::try_from(attributes).expect("a{ss} attributes");
            names.push(attributes.get("username").cloned().unwrap_or_default());
        }
        names.sort();
        names
    }
}

impl Drop for SecretService {
    fn drop(&mut self) {
        let _ = self.keyring.kill();
        let _ = self.keyring.wait();
        let _ = self.bus.kill();
        let _ = self.bus.wait();
    }
}

/// A session bus that listens in `dir`, lets anyone own any name and send anything, and
/// starts no services.
fn bus_config(dir: &Path) -> String {
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
