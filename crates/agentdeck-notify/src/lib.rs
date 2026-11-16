//! The GUI's desktop notifications (spec §11): the freedesktop notifications D-Bus API over one
//! session-bus connection for the whole app, however many notifications are open.
//!
//! GPUI's own Linux notifications start a thread and a bus connection per notification, each
//! held until the server closes it; one left in GNOME's tray holds both for good (#133). Here
//! one task on GPUI's foreground owns the connection: it sends each `Notify` in turn and reads
//! the server's signals, telling a click on one notification from another by the id the server
//! gave it. The click handler runs on the foreground too, so it can activate a window.
//!
//! The ids also tie each notification to its tag, so a newer one for the same tag replaces it
//! in place (`replaces_id`), and the app can withdraw it (`CloseNotification`) once the user has
//! seen what it is about. A click withdraws it, and quitting withdraws every one still shown,
//! so none outlives the app that could act on it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use futures_channel::mpsc;
use futures_util::{StreamExt as _, stream};
use gpui::{App, AsyncApp, SharedString, Subscription, Task};
use zbus::Variant;

/// The name the server shows on each notification.
const APP_NAME: &str = "agentdeck";

/// The app's desktop file id, `agentdeck.desktop` without its suffix: the `desktop-entry` hint,
/// by which the desktop groups the notifications with the app's window (its app id, the
/// file's `StartupWMClass`).
const DESKTOP_ENTRY: &str = "agentdeck";

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
/// - [`show`](Notifier::show) and [`withdraw`](Notifier::withdraw) return at once and never
///   fail the caller: a notification that can't be shown or closed is logged and dropped. It
///   has no buttons; a click on its body is its one action.
/// - One notification per tag: showing a tag whose notification is still open replaces that
///   one in place.
/// - The handler [`on_click`](Notifier::on_click) set runs on GPUI's foreground with the
///   notification's tag when the user clicks its body: at most once per notification shown,
///   and never once the notification has closed. A later `on_click` replaces it.
/// - A notification clicked is withdrawn, and so is every one still open when the app quits.
pub trait Notifier {
    /// Shows `notification`, in place of the one still open with its tag.
    fn show(&self, notification: Notification);

    /// Closes the notification open with `tag`, if one is.
    fn withdraw(&self, tag: &str);

    /// Sets what a click on a notification's body runs.
    fn on_click(&self, handler: ClickHandler);
}

/// [`Notifier`] over the desktop's session bus: one connection and one foreground task,
/// however many notifications are open. Dropping it closes the connection.
pub struct DesktopNotifier {
    /// What the task is asked to do, in order.
    requests: mpsc::UnboundedSender<Request>,
    on_click: Rc<RefCell<Option<ClickHandler>>>,
    _serving: Task<()>,
    /// Withdraws every notification still open as the app quits.
    _quitting: Subscription,
}

impl DesktopNotifier {
    /// Connects to the session bus (`$DBUS_SESSION_BUS_ADDRESS`) on a task of its own. Without
    /// one, it logs why once and shows nothing.
    pub fn session(cx: &mut App) -> DesktopNotifier {
        let (requests, queued) = mpsc::unbounded();
        let on_click = Rc::<RefCell<Option<ClickHandler>>>::default();
        let handler = on_click.clone();
        let state = Rc::<RefCell<State>>::default();
        let serving = cx.spawn({
            let state = state.clone();
            async move |cx| serve(queued, handler, state, cx).await
        });
        // Run while GPUI blocks on the quit observers, so the calls are awaited right here;
        // zbus reads their replies on its own thread.
        let quitting = cx.on_app_quit(move |_| {
            let (connection, open) = {
                let mut state = state.borrow_mut();
                (state.connection.clone(), state.routes.take())
            };
            async move {
                if let Some(connection) = connection {
                    for id in open {
                        close(&connection, id).await;
                    }
                }
            }
        });
        DesktopNotifier {
            requests,
            on_click,
            _serving: serving,
            _quitting: quitting,
        }
    }
}

impl Notifier for DesktopNotifier {
    fn show(&self, notification: Notification) {
        // Fails only once the task has given up on the bus, which it logged.
        let _ = self.requests.unbounded_send(Request::Show(notification));
    }

    fn withdraw(&self, tag: &str) {
        let tag = SharedString::from(tag.to_owned());
        let _ = self.requests.unbounded_send(Request::Withdraw(tag));
    }

    fn on_click(&self, handler: ClickHandler) {
        *self.on_click.borrow_mut() = Some(handler);
    }
}

/// What the app asks of the serving task.
enum Request {
    /// Show a notification.
    Show(Notification),
    /// Close the notification open with this tag.
    Withdraw(SharedString),
}

/// What the serving task waits on.
enum Event {
    /// A request from the app.
    Request(Request),
    /// A signal from the notification server.
    Signal(zbus::Result<zbus::Message>),
}

/// What the serving task shares with the quit observer.
#[derive(Default)]
struct State {
    /// The session bus, once connected.
    connection: Option<zbus::Connection>,
    routes: Routes,
}

/// The task: connects, then carries out what is queued and routes the server's signals, until
/// the [`DesktopNotifier`] is dropped.
async fn serve(
    queued: mpsc::UnboundedReceiver<Request>,
    on_click: Rc<RefCell<Option<ClickHandler>>>,
    state: Rc<RefCell<State>>,
    cx: &mut AsyncApp,
) {
    let (connection, signals) = match connect().await {
        Ok(connected) => connected,
        Err(error) => {
            tracing::warn!("no desktop notifications: the session bus can't be reached: {error}");
            return;
        }
    };
    state.borrow_mut().connection = Some(connection.clone());
    let mut events = stream::select(queued.map(Event::Request), signals.map(Event::Signal));
    while let Some(event) = events.next().await {
        match event {
            // Awaited before the next event, so the id is routed before any signal about it.
            Event::Request(Request::Show(notification)) => {
                let replaces = state.borrow().routes.id_of(&notification.tag);
                match notify(&connection, &notification, replaces).await {
                    Ok(id) => state.borrow_mut().routes.shown(id, notification.tag),
                    Err(error) => tracing::warn!("a desktop notification wasn't shown: {error}"),
                }
            }
            Event::Request(Request::Withdraw(tag)) => {
                let id = state.borrow_mut().routes.withdrawn(&tag);
                if let Some(id) = id {
                    close(&connection, id).await;
                }
            }
            Event::Signal(Ok(signal)) => {
                let clicked = route(&mut state.borrow_mut().routes, &signal);
                if let Some((id, tag)) = clicked {
                    // Closed first: the click may quit the app.
                    close(&connection, id).await;
                    if let Some(tag) = tag {
                        click(&on_click, tag, cx);
                    }
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

/// Shows `notification` in place of notification `replaces`, when that is still open: named
/// [`APP_NAME`] with the [`DESKTOP_ENTRY`] hint, no icon, the server's own timeout, and the
/// default action alone. Answers the id the server gave it.
async fn notify(
    connection: &zbus::Connection,
    notification: &Notification,
    replaces: Option<u32>,
) -> zbus::Result<u32> {
    let hints = HashMap::from([("desktop-entry", Variant::from(DESKTOP_ENTRY))]);
    let actions = [DEFAULT_ACTION, DEFAULT_ACTION];
    let reply = connection
        .call_method(
            Some(BUS_NAME),
            PATH,
            Some(INTERFACE),
            "Notify",
            &(
                APP_NAME,
                replaces.unwrap_or(0),
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

/// Closes notification `id`. One already gone (the server closed it, or the user did) is no
/// failure worth more than a debug line.
async fn close(connection: &zbus::Connection, id: u32) {
    let closing = connection
        .call_method(
            Some(BUS_NAME),
            PATH,
            Some(INTERFACE),
            "CloseNotification",
            &id,
        )
        .await;
    if let Err(error) = closing {
        tracing::debug!(id, "a desktop notification wasn't closed: {error}");
    }
}

/// Updates `routes` with `signal`. When it reports an action invoked on one of ours: its id,
/// and its tag when that action is a click on its body.
fn route(routes: &mut Routes, signal: &zbus::Message) -> Option<(u32, Option<SharedString>)> {
    let header = signal.header();
    match header.member()?.as_str() {
        "ActionInvoked" => {
            let (id, action): (u32, String) = signal.body().deserialize().ok()?;
            let ours = routes.is_open(id);
            let tag = routes.invoked(id, &action);
            ours.then_some((id, tag))
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
    /// Notification `id` was shown for `tag`, replacing any other open with it.
    fn shown(&mut self, id: u32, tag: SharedString) {
        self.open.retain(|_, open| *open != tag);
        self.open.insert(id, tag);
    }

    /// The id of the notification open with `tag`.
    fn id_of(&self, tag: &str) -> Option<u32> {
        (self.open.iter()).find_map(|(id, open)| (open == tag).then_some(*id))
    }

    /// The notification open with `tag` is being closed: its id, if one is open.
    fn withdrawn(&mut self, tag: &str) -> Option<u32> {
        let id = self.id_of(tag)?;
        self.open.remove(&id);
        Some(id)
    }

    /// Every notification is being closed: their ids.
    fn take(&mut self) -> Vec<u32> {
        self.open.drain().map(|(id, _)| id).collect()
    }

    /// Whether notification `id` is one of ours, still open.
    fn is_open(&self, id: u32) -> bool {
        self.open.contains_key(&id)
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
        fn should_keep_one_notification_per_tag() {
            let mut routes = showing(&["a", "b"]);

            routes.shown(3, "a".into());

            assert_eq!(routes.id_of("a"), Some(3));
            assert_eq!(routes.invoked(1, "default"), None);
        }

        #[test]
        fn should_find_the_id_open_with_a_tag() {
            let routes = showing(&["a", "b"]);

            assert_eq!(routes.id_of("b"), Some(2));
            assert_eq!(routes.id_of("c"), None);
        }

        #[test]
        fn should_withdraw_a_tag_once() {
            let mut routes = showing(&["a", "b"]);

            assert_eq!(routes.withdrawn("a"), Some(1));
            assert_eq!(routes.withdrawn("a"), None);
            assert_eq!(routes.invoked(1, "default"), None);
            assert_eq!(routes.invoked(2, "default"), Some("b".into()));
        }

        #[test]
        fn should_take_every_id_still_open() {
            let mut routes = showing(&["a", "b", "c"]);
            routes.closed(2);

            let mut taken = routes.take();
            taken.sort();

            assert_eq!(taken, [1, 3]);
            assert_eq!(routes, Routes::default());
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
