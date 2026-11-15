//! [`DesktopNotifier`] for real: the app side runs GPUI's headless platform in a child process
//! of this same test binary, whose session bus is the test's private one.

use std::collections::{HashMap, HashSet};

use agentdeck_notify::{DesktopNotifier, Notification, Notifier as _};
use gpui::{App, Global, SharedString};

use crate::support::{APP_SIDE, AppSide, FakeServer, PrivateBus};

/// The app side, when this process is one: shows the tags [`APP_SIDE`] lists, prints each
/// click, and quits once the last tag is clicked. Answers whether it was one.
fn ran_as_app_side() -> bool {
    let Some(tags) = std::env::var_os(APP_SIDE) else {
        return false;
    };
    let tags: Vec<SharedString> = (tags.to_str().expect("UTF-8 tags").split(','))
        .map(|tag| SharedString::from(tag.to_owned()))
        .collect();
    gpui_platform::headless().run(move |cx: &mut App| {
        let notifier = DesktopNotifier::session(cx);
        let last = tags.last().cloned();
        notifier.on_click(Box::new(move |tag, cx| {
            println!("clicked {tag}");
            if Some(&tag) == last.as_ref() {
                cx.quit();
            }
        }));
        for tag in tags {
            notifier.show(Notification {
                title: format!("{tag} title").into(),
                body: format!("{tag} body").into(),
                tag,
            });
        }
        cx.set_global(Keep {
            _notifier: notifier,
        });
    });
    true
}

/// Keeps the app side's notifier, and its connection, for as long as the app runs.
struct Keep {
    _notifier: DesktopNotifier,
}

impl Global for Keep {}

/// A tokio runtime for the fake server, and the server on `bus`.
fn serve(bus: &PrivateBus) -> (tokio::runtime::Runtime, FakeServer) {
    let runtime = tokio::runtime::Runtime::new().expect("a tokio runtime");
    let server = runtime.block_on(FakeServer::serve(bus.address()));
    (runtime, server)
}

const TEST: &str = "desktop_notifier::show::";

/// Threads one app side may have and another not yet, whatever the notifier does. GPUI's sleep
/// and power-mode listeners each open a system-bus connection at startup, on a zbus executor
/// thread of its own, and the `blocking` pool starts threads on demand and lets idle ones go;
/// on a loaded machine any of them can land after the notifications are sent (#141). Far
/// under the 19 more a thread per notification would add.
const LATE_THREADS: usize = 4;

mod show {
    use super::*;

    #[test]
    fn should_send_what_gpui_sent() {
        if ran_as_app_side() {
            return;
        }
        let bus = PrivateBus::start();
        let (runtime, mut server) = serve(&bus);
        let app = AppSide::spawn(
            &format!("{TEST}should_send_what_gpui_sent"),
            &bus,
            &["pane:7"],
        );

        let call = runtime.block_on(server.next_call());
        runtime.block_on(server.invoke(call.id, "default"));

        assert_eq!(
            (call.app_name.as_str(), call.replaces_id, call.icon.as_str()),
            ("agentdeck", 0, "")
        );
        assert_eq!(
            (call.summary.as_str(), call.body.as_str()),
            ("pane:7 title", "pane:7 body")
        );
        assert_eq!(call.actions, ["default", "default"]);
        assert_eq!(call.hints, HashMap::new());
        assert_eq!(call.timeout, -1);
        assert_eq!(app.clicks(), ["clicked pane:7"]);
    }

    #[test]
    fn should_share_one_connection_and_add_no_thread_per_notification() {
        if ran_as_app_side() {
            return;
        }
        let (with_one, _) = showing(1);
        let (with_twenty, senders) = showing(20);
        eprintln!("app side threads: {with_one} with 1 notification, {with_twenty} with 20");

        assert_eq!(senders.len(), 1, "one connection: {senders:?}");
        assert!(
            with_twenty <= with_one + LATE_THREADS,
            "{with_one} threads with 1 notification, {with_twenty} with 20"
        );
    }

    /// An app side that shows `count` notifications: its thread count once the server has had
    /// every one, and the connections they came from. Each app side shows its notifications as
    /// it starts, so comparing two of them counts a thread started by `show` as well as one
    /// started to send.
    fn showing(count: usize) -> (usize, HashSet<String>) {
        let bus = PrivateBus::start();
        let (runtime, mut server) = serve(&bus);
        let tags: Vec<String> = (1..=count).map(|n| format!("n:{n}")).collect();
        let tags: Vec<&str> = tags.iter().map(String::as_str).collect();
        let app = AppSide::spawn(
            &format!("{TEST}should_share_one_connection_and_add_no_thread_per_notification"),
            &bus,
            &tags,
        );

        let calls: Vec<_> = (0..count)
            .map(|_| runtime.block_on(server.next_call()))
            .collect();
        let threads = app.threads();
        let last = calls.last().expect("a notification").id;
        runtime.block_on(server.invoke(last, "default"));

        assert_eq!(app.clicks(), [format!("clicked n:{count}")]);
        (threads, calls.into_iter().map(|call| call.sender).collect())
    }
}

mod click {
    use super::*;

    const TEST: &str = "desktop_notifier::click::";

    #[test]
    fn should_report_the_tag_of_the_notification_clicked() {
        if ran_as_app_side() {
            return;
        }
        let bus = PrivateBus::start();
        let (runtime, mut server) = serve(&bus);
        let app = AppSide::spawn(
            &format!("{TEST}should_report_the_tag_of_the_notification_clicked"),
            &bus,
            &["a", "b", "c"],
        );

        let ids: Vec<u32> = (0..3)
            .map(|_| runtime.block_on(server.next_call()).id)
            .collect();
        runtime.block_on(async {
            server.invoke(ids[1], "default").await;
            server.invoke(ids[2], "default").await;
        });

        assert_eq!(app.clicks(), ["clicked b", "clicked c"]);
    }

    #[test]
    fn should_report_nothing_for_a_notification_that_closed() {
        if ran_as_app_side() {
            return;
        }
        let bus = PrivateBus::start();
        let (runtime, mut server) = serve(&bus);
        let app = AppSide::spawn(
            &format!("{TEST}should_report_nothing_for_a_notification_that_closed"),
            &bus,
            &["a", "b"],
        );

        let a = runtime.block_on(server.next_call()).id;
        let b = runtime.block_on(server.next_call()).id;
        runtime.block_on(async {
            server.close(a, 2).await;
            server.invoke(a, "default").await;
            server.invoke(b, "default").await;
        });

        assert_eq!(app.clicks(), ["clicked b"]);
    }
}
