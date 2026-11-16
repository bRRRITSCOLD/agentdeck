//! [`DesktopNotifier`] for real: the app side runs GPUI's headless platform in a child process
//! of this same test binary, whose session bus is the test's private one.

use std::collections::{HashMap, HashSet};

use agentdeck_notify::{DesktopNotifier, Notification, Notifier as _};
use gpui::{App, Global, SharedString};

use crate::support::{APP_SIDE, AppSide, FakeServer, PrivateBus};

/// The app side, when this process is one: does the steps [`APP_SIDE`] lists, prints each
/// click, and quits once the last tag shown is clicked. Answers whether it was one.
fn ran_as_app_side() -> bool {
    let Some(steps) = std::env::var_os(APP_SIDE) else {
        return false;
    };
    let steps: Vec<String> = (steps.to_str().expect("UTF-8 steps").split(','))
        .map(str::to_owned)
        .collect();
    gpui_platform::headless().run(move |cx: &mut App| {
        let notifier = DesktopNotifier::session(cx);
        let last = (steps.iter().rev())
            .find(|step| !step.starts_with('-'))
            .map(|tag| SharedString::from(tag.clone()));
        notifier.on_click(Box::new(move |tag, cx| {
            println!("clicked {tag}");
            if Some(&tag) == last.as_ref() {
                cx.quit();
            }
        }));
        for step in steps {
            match step.strip_prefix('-') {
                Some(tag) => notifier.withdraw(tag),
                None => notifier.show(Notification {
                    title: format!("{step} title").into(),
                    body: format!("{step} body").into(),
                    tag: step.into(),
                }),
            }
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
    fn should_name_the_app_and_its_desktop_entry() {
        if ran_as_app_side() {
            return;
        }
        let bus = PrivateBus::start();
        let (runtime, mut server) = serve(&bus);
        let app = AppSide::spawn(
            &format!("{TEST}should_name_the_app_and_its_desktop_entry"),
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
        let hints: HashMap<String, String> = (call.hints.into_iter())
            .map(|(key, value)| (key, String::try_from(value).expect("a string hint")))
            .collect();
        assert_eq!(
            hints,
            HashMap::from([("desktop-entry".to_owned(), "agentdeck".to_owned())])
        );
        assert_eq!(call.timeout, -1);
        assert_eq!(app.clicks(), ["clicked pane:7"]);
    }

    #[test]
    fn should_replace_the_notification_still_shown_with_the_same_tag() {
        if ran_as_app_side() {
            return;
        }
        let bus = PrivateBus::start();
        let (runtime, mut server) = serve(&bus);
        let app = AppSide::spawn(
            &format!("{TEST}should_replace_the_notification_still_shown_with_the_same_tag"),
            &bus,
            &["a", "b", "a"],
        );

        let a = runtime.block_on(server.next_call());
        let b = runtime.block_on(server.next_call());
        let again = runtime.block_on(server.next_call());
        runtime.block_on(server.invoke(again.id, "default"));

        assert_eq!((a.replaces_id, b.replaces_id), (0, 0));
        assert_eq!((again.replaces_id, again.id), (a.id, a.id));
        assert_eq!(app.clicks(), ["clicked a"]);
    }

    #[test]
    fn should_show_a_new_notification_for_a_tag_whose_last_one_closed() {
        if ran_as_app_side() {
            return;
        }
        let bus = PrivateBus::start();
        let (runtime, mut server) = serve(&bus);
        let app = AppSide::spawn(
            &format!("{TEST}should_show_a_new_notification_for_a_tag_whose_last_one_closed"),
            &bus,
            &["a", "-a", "a"],
        );

        let first = runtime.block_on(server.next_call());
        runtime.block_on(server.next_close());
        let again = runtime.block_on(server.next_call());
        runtime.block_on(server.invoke(again.id, "default"));

        assert_eq!(again.replaces_id, 0);
        assert_ne!(again.id, first.id);
        assert_eq!(app.clicks(), ["clicked a"]);
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

mod withdraw {
    use super::*;

    const TEST: &str = "desktop_notifier::withdraw::";

    #[test]
    fn should_close_the_notification_shown_with_the_tag() {
        if ran_as_app_side() {
            return;
        }
        let bus = PrivateBus::start();
        let (runtime, mut server) = serve(&bus);
        let app = AppSide::spawn(
            &format!("{TEST}should_close_the_notification_shown_with_the_tag"),
            &bus,
            &["a", "b", "-a", "-never-shown", "c"],
        );

        let a = runtime.block_on(server.next_call()).id;
        runtime.block_on(server.next_call());
        let closed = runtime.block_on(server.next_close());
        let c = runtime.block_on(server.next_call()).id;
        runtime.block_on(server.invoke(c, "default"));

        assert_eq!(closed, a);
        assert_eq!(app.clicks(), ["clicked c"]);
    }

    #[test]
    fn should_close_a_notification_once_clicked_and_every_one_left_on_quit() {
        if ran_as_app_side() {
            return;
        }
        let bus = PrivateBus::start();
        let (runtime, mut server) = serve(&bus);
        let app = AppSide::spawn(
            &format!("{TEST}should_close_a_notification_once_clicked_and_every_one_left_on_quit"),
            &bus,
            &["a", "b", "c"],
        );

        let ids: Vec<u32> = (0..3)
            .map(|_| runtime.block_on(server.next_call()).id)
            .collect();
        runtime.block_on(server.invoke(ids[0], "default"));
        let clicked = runtime.block_on(server.next_close());
        // Clicking c quits the app side, which withdraws b, still shown, as it goes.
        runtime.block_on(server.invoke(ids[2], "default"));
        let mut closed: Vec<u32> = (0..2)
            .map(|_| runtime.block_on(server.next_close()))
            .collect();
        closed.sort();

        assert_eq!(clicked, ids[0]);
        assert_eq!(closed, [ids[1], ids[2]]);
        assert_eq!(app.clicks(), ["clicked a", "clicked c"]);
    }
}
