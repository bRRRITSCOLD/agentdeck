//! GPUI's system notifications on Linux, which the app shows its alerts through (spec §11):
//! the contract `TestAppContext::shown_system_notifications` and
//! `simulate_system_notification_response` stand in for in the unit tier.
//!
//! The app side runs in a child process of this same test binary, so its session bus can be
//! the test's private one: `DBUS_SESSION_BUS_ADDRESS` is set only for the child, never here.

use std::io::Read as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use gpui::{App, SystemNotification};

use crate::support::{FakeServer, Notify, PrivateBus};

/// Set in the child: run the app side, not the test.
const CHILD: &str = "AGENTDECK_NOTIFICATIONS_CONTRACT_CHILD";

/// How long each side waits for the other.
const TIMEOUT: Duration = Duration::from_secs(20);

/// The notification the child shows, as the app shows a pane's.
fn notification() -> SystemNotification {
    SystemNotification {
        tag: "pane:7".into(),
        title: "Claude Code is blocked".into(),
        body: "first · web".into(),
        actions: Vec::new(),
    }
}

/// The app side: names the app as `notifications::init` does, shows [`notification`], then
/// prints the click GPUI reports and quits.
fn app() {
    gpui_platform::headless().run(|cx: &mut App| {
        cx.set_app_identity("agentdeck", "agentdeck");
        cx.on_system_notification_response(|response, cx| {
            println!("clicked {} {:?}", response.tag, response.action_id);
            cx.quit();
        });
        cx.show_system_notification(notification());
    });
}

mod show_system_notification {
    use super::*;

    #[test]
    fn should_reach_the_notification_server_and_report_a_click_on_its_body() {
        if std::env::var_os(CHILD).is_some() {
            app();
            return;
        }
        let bus = PrivateBus::start();
        let runtime = tokio::runtime::Runtime::new().expect("a tokio runtime");
        let mut server = runtime.block_on(FakeServer::serve(bus.address()));
        let mut child = Command::new(std::env::current_exe().expect("the test binary"))
            .args([
                "notifications::show_system_notification::should_reach_the_notification_server_and_report_a_click_on_its_body",
                "--exact",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("DBUS_SESSION_BUS_ADDRESS", bus.address())
            .env("XDG_RUNTIME_DIR", bus.dir())
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .stdout(Stdio::piped())
            .spawn()
            .expect("starting the app side");
        let killed = KillOnDrop(&mut child);

        let call = runtime
            .block_on(async { tokio::time::timeout(TIMEOUT, server.calls.recv()).await })
            .expect("Notify reaches the server in time")
            .expect("the server is still running");
        assert_eq!(
            call,
            Notify {
                app_name: "agentdeck".into(),
                summary: "Claude Code is blocked".into(),
                body: "first · web".into(),
                actions: vec!["default".into(), "default".into()],
            }
        );

        // The app subscribes to ActionInvoked only once Notify has answered, so the click is
        // repeated until it lands.
        let started = Instant::now();
        let status = loop {
            runtime.block_on(server.invoke(1, "default"));
            if let Some(status) = killed.0.try_wait().expect("polling the app side") {
                break status;
            }
            assert!(
                started.elapsed() < TIMEOUT,
                "the app side never reported the click"
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        let mut stdout = String::new();
        (killed.0.stdout.take().expect("its stdout"))
            .read_to_string(&mut stdout)
            .expect("reading the app side's output");

        assert!(status.success(), "the app side failed: {status}\n{stdout}");
        assert!(stdout.contains("clicked pane:7 None"), "{stdout}");
    }
}

/// Kills the app side if the test fails before it exits.
struct KillOnDrop<'a>(&'a mut std::process::Child);

impl Drop for KillOnDrop<'_> {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
