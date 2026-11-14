//! The integration tier: [`DesktopNotifier`](agentdeck_notify::DesktopNotifier) against a fake
//! notification server on a private `dbus-daemon` session bus, the contract the GUI's unit tests'
//! fake [`Notifier`](agentdeck_notify::Notifier) stands in for.

mod desktop_notifier;
mod support;
