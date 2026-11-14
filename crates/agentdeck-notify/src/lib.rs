//! The GUI's desktop notifications (spec §11): the freedesktop notifications D-Bus API over one
//! session-bus connection for the whole app, however many notifications are open.
//!
//! GPUI's own Linux notifications start a thread and a bus connection per notification, each
//! held until the server closes it; one left in GNOME's tray holds both for good (#133). Here
//! one task on GPUI's foreground owns the connection: it sends each `Notify` in turn and reads
//! the server's signals, telling a click on one notification from another by the id the server
//! gave it. The click handler runs on the foreground too, so it can activate a window.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use futures_channel::mpsc;
use futures_util::{StreamExt as _, stream};
use gpui::{App, AsyncApp, SharedString, Task};
use zbus::Variant;

/// The name the server shows on each notification.
const APP_NAME: &str = "agentdeck";

/// The notification server's bus name, object path and interface.
const BUS_NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const INTERFACE: &str = "org.freedesktop.Notifications";

/// The action a server invokes for a click on a notification's body.
const DEFAULT_ACTION: &str = "default";

/// A notification to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// What a click on it reports, so the app can find what it is about.
    pub tag: SharedString,
    /// The headline.
    pub title: SharedString,
    /// The line below it.
    pub body: SharedString,
}

/// What a click on a notification's body runs, given its tag, on GPUI's foreground.
pub type ClickHandler = Box<dyn FnMut(SharedString, &mut App)>;

/// Shows desktop notifications and reports a click on one.
///
/// The contract every implementation, and every test double, keeps:
/// - [`show`](Notifier::show) returns at once and never fails the caller: a notification that
///   can't be shown is logged and dropped. It has no buttons; a click on its body is its one
///   action.
/// - The handler [`on_click`](Notifier::on_click) set runs on GPUI's foreground with the
///   notification's tag when the user clicks its body: at most once per notification shown,
///   and never once the notification has closed. A later `on_click` replaces it.
pub trait Notifier {
    /// Shows `notification`.
    fn show(&self, notification: Notification);

    /// Sets what a click on a notification's body runs.
    fn on_click(&self, handler: ClickHandler);
}

/// [`Notifier`] over the desktop's session bus: one connection and one foreground task,
/// however many notifications are open. Dropping it closes the connection.
pub struct DesktopNotifier {
    /// Notifications for the task to show, in order.
    requests: mpsc::UnboundedSender<Notification>,
    on_click: Rc<RefCell<Option<ClickHandler>>>,
    _serving: Task<()>,
}

impl DesktopNotifier {
    /// Connects to the session bus (`$DBUS_SESSION_BUS_ADDRESS`) on a task of its own. Without
    /// one, it logs why once and shows nothing.
    pub fn session(cx: &mut App) -> DesktopNotifier {
        let (requests, queued) = mpsc::unbounded();
        let on_click = Rc::<RefCell<Option<ClickHandler>>>::default();
        let handler = on_click.clone();
        let serving = cx.spawn(async move |cx| serve(queued, handler, cx).await);
        DesktopNotifier {
            requests,
            on_click,
            _serving: serving,
        }
    }
}

impl Notifier for DesktopNotifier {
    fn show(&self, notification: Notification) {
        // Fails only once the task has given up on the bus, which it logged.
        let _ = self.requests.unbounded_send(notification);
    }

    fn on_click(&self, handler: ClickHandler) {
        *self.on_click.borrow_mut() = Some(handler);
    }
}

/// What the serving task waits on.
enum Event {
    /// A notification to show.
    Show(Notification),
    /// A signal from the notification server.
    Signal(zbus::Result<zbus::Message>),
}

/// The task: connects, then shows what is queued and routes the server's signals, until the
/// [`DesktopNotifier`] is dropped.
async fn serve(
    queued: mpsc::UnboundedReceiver<Notification>,
    on_click: Rc<RefCell<Option<ClickHandler>>>,
    cx: &mut AsyncApp,
) {
    let (connection, signals) = match connect().await {
        Ok(connected) => connected,
        Err(error) => {
            tracing::warn!("no desktop notifications: the session bus can't be reached: {error}");
            return;
        }
    };
    let mut routes = Routes::default();
    let mut events = stream::select(queued.map(Event::Show), signals.map(Event::Signal));
    while let Some(event) = events.next().await {
        match event {
            // Awaited before the next event, so the id is routed before any signal about it.
            Event::Show(notification) => match notify(&connection, &notification).await {
                Ok(id) => routes.shown(id, notification.tag),
                Err(error) => tracing::warn!("a desktop notification wasn't shown: {error}"),
            },
            Event::Signal(Ok(signal)) => {
                if let Some(tag) = route(&mut routes, &signal) {
                    click(&on_click, tag, cx);
                }
            }
            Event::Signal(Err(_)) => {}
        }
    }
}

/// The session bus, and its stream of the notification server's signals. The match rule is
/// registered before anything is shown, so no signal about a notification can be missed.
async fn connect() -> zbus::Result<(zbus::Connection, zbus::MessageStream)> {
    let connection = zbus::connection::Builder::session().build().await?;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(INTERFACE)
        .build()?;
    let signals = zbus::MessageStream::for_match_rule(rule, &connection, None).await?;
    Ok((connection, signals))
}

/// Shows `notification`, as GPUI did: named [`APP_NAME`], no icon, no hints, the server's own
/// timeout, and the default action alone. Answers the id the server gave it.
async fn notify(connection: &zbus::Connection, notification: &Notification) -> zbus::Result<u32> {
    let hints = HashMap::<&str, Variant<'_>>::new();
    let actions = [DEFAULT_ACTION, DEFAULT_ACTION];
    let reply = connection
        .call_method(
            Some(BUS_NAME),
            PATH,
            Some(INTERFACE),
            "Notify",
            &(
                APP_NAME,
                0u32,
                "",
                notification.title.as_str(),
                notification.body.as_str(),
                actions.as_slice(),
                hints,
                -1i32,
            ),
        )
        .await?;
    reply.body().deserialize()
}

/// Updates `routes` with `signal`: the tag clicked, when it reports a click on one of ours.
fn route(routes: &mut Routes, signal: &zbus::Message) -> Option<SharedString> {
    let header = signal.header();
    match header.member()?.as_str() {
        "ActionInvoked" => {
            let (id, action): (u32, String) = signal.body().deserialize().ok()?;
            routes.invoked(id, &action)
        }
        "NotificationClosed" => {
            let (id, _reason): (u32, u32) = signal.body().deserialize().ok()?;
            routes.closed(id);
            None
        }
        _ => None,
    }
}

/// Runs the click handler for `tag`, taken out for the call: it may set another.
fn click(on_click: &Rc<RefCell<Option<ClickHandler>>>, tag: SharedString, cx: &mut AsyncApp) {
    let Some(mut handler) = on_click.borrow_mut().take() else {
        return;
    };
    cx.update(|cx| handler(tag, cx));
    on_click.borrow_mut().get_or_insert(handler);
}

/// What the server tells the app about its notifications, by the id it gave each one.
#[derive(Debug, Default, PartialEq, Eq)]
struct Routes {
    /// The tag of each notification still open.
    open: HashMap<u32, SharedString>,
}

impl Routes {
    /// Notification `id` was shown for `tag`.
    fn shown(&mut self, id: u32, tag: SharedString) {
        self.open.insert(id, tag);
    }

    /// The user invoked `action` on notification `id`: its tag when that is a click on its body.
    /// Any action ends it, as the server closes a notification once one is invoked.
    fn invoked(&mut self, id: u32, action: &str) -> Option<SharedString> {
        let tag = self.open.remove(&id)?;
        (action == DEFAULT_ACTION).then_some(tag)
    }

    /// Notification `id` closed.
    fn closed(&mut self, id: u32) {
        self.open.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod routes {
        use super::*;

        fn showing(tags: &[&str]) -> Routes {
            let mut routes = Routes::default();
            for (id, tag) in (1..).zip(tags) {
                routes.shown(id, SharedString::new(*tag));
            }
            routes
        }

        #[test]
        fn should_route_a_click_on_the_body_to_the_tag_shown_with_its_id() {
            let mut routes = showing(&["a", "b", "c"]);

            assert_eq!(routes.invoked(2, "default"), Some("b".into()));
        }

        #[test]
        fn should_report_a_click_once() {
            let mut routes = showing(&["a"]);
            routes.invoked(1, "default");

            assert_eq!(routes.invoked(1, "default"), None);
            assert_eq!(routes, Routes::default());
        }

        #[test]
        fn should_forget_a_notification_once_it_closes() {
            let mut routes = showing(&["a", "b"]);

            routes.closed(1);

            assert_eq!(routes.invoked(1, "default"), None);
            assert_eq!(routes.invoked(2, "default"), Some("b".into()));
        }

        #[test]
        fn should_hold_nothing_once_every_notification_closed() {
            let mut routes = showing(&["a", "b", "c"]);

            for id in 1..=3 {
                routes.closed(id);
            }

            assert_eq!(routes, Routes::default());
        }

        #[test]
        fn should_report_no_click_for_another_action() {
            let mut routes = showing(&["a"]);

            assert_eq!(routes.invoked(1, "inline-reply"), None);
        }

        #[test]
        fn should_ignore_an_id_it_never_showed() {
            let mut routes = showing(&["a"]);

            assert_eq!(routes.invoked(9, "default"), None);
            routes.closed(9);
            assert_eq!(routes, showing(&["a"]));
        }
    }
}
