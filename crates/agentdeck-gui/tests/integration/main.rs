//! The GUI's integration tier: what its unit tests stand in for, run for real. GPUI's test
//! platform records the notifications the app shows; [`notifications`] checks that GPUI's
//! Linux platform delivers them to a notification server over D-Bus, and reports a click.

mod notifications;
mod support;
