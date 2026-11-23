//! Opening a PR's GitHub page in the default browser (spec §11, Opening PRs): a PR row's
//! reference and its **Open on GitHub** both call [`open()`] with the PR's stored `url`.
//!
//! It adds no opener of its own: the URL goes to the OS's handler through
//! [`open::open_url`], as the default `terminal.file_opener` (`xdg-open`) with the URL as its
//! one argument, so a handler that can't start is toasted. Only a GitHub `https` URL opens
//! ([`openable`]); anything else is refused with a debug line, as a pane refuses other schemes.

use gpui::{App, Window};

use crate::terminal::open;

/// The host a PR's `url` must name.
const GITHUB: &str = "github.com";

/// Whether a click may open `url`: its scheme is `https` and its host is `github.com`, with no
/// user and no port, followed by a path. Scheme and host are matched ignoring case.
pub fn openable(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (host, path) = rest.split_at(host_end);
    scheme.eq_ignore_ascii_case("https")
        && host.eq_ignore_ascii_case(GITHUB)
        && path.starts_with('/')
}

/// Opens `url`, a PR's, with the OS's handler: refused (a debug line) unless [`openable`];
/// a toast "Couldn't open the PR" when the handler can't start.
pub fn open(url: &str, window: &mut Window, cx: &mut App) {
    if !openable(url) {
        tracing::debug!(
            url,
            "refused to open a PR link that isn't a GitHub https URL"
        );
        return;
    }
    open::open_url(url, "Couldn't open the PR", window, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    mod openable {
        use super::*;

        #[test]
        fn should_open_a_github_https_url() {
            for url in [
                "https://github.com/acme/widgets/pull/4",
                "HTTPS://GitHub.com/acme/widgets/pull/4",
                "https://github.com/acme/widgets/pull/4?w=1#discussion",
            ] {
                assert!(openable(url), "{url}");
            }
        }

        #[test]
        fn should_refuse_another_scheme() {
            for url in [
                "http://github.com/acme/widgets/pull/4",
                "file:///etc/passwd",
                "javascript:alert(1)",
                "github.com/acme/widgets/pull/4",
                "https:github.com/acme/widgets/pull/4",
            ] {
                assert!(!openable(url), "{url}");
            }
        }

        #[test]
        fn should_refuse_another_host() {
            for url in [
                "https://gitlab.com/acme/widgets/pull/4",
                "https://github.com.evil.io/acme/widgets/pull/4",
                "https://api.github.com/repos/acme/widgets/pulls/4",
                "https://evil.io/github.com/acme/widgets/pull/4",
            ] {
                assert!(!openable(url), "{url}");
            }
        }

        #[test]
        fn should_refuse_a_user_or_a_port() {
            for url in [
                "https://github.com@evil.io/acme/widgets/pull/4",
                "https://me@github.com/acme/widgets/pull/4",
                "https://github.com:8443/acme/widgets/pull/4",
            ] {
                assert!(!openable(url), "{url}");
            }
        }

        #[test]
        fn should_refuse_no_path() {
            for url in ["https://github.com", "https://github.com?x=1", ""] {
                assert!(!openable(url), "{url}");
            }
        }
    }

    mod open {
        use gpui::{AppContext as _, TestAppContext, VisualTestContext};

        use super::*;
        use crate::terminal::open::fake;

        const URL: &str = "https://github.com/acme/widgets/pull/4";

        /// A window to toast in.
        fn window(cx: &mut TestAppContext) -> &mut VisualTestContext {
            cx.update(brrritscold_core_gpui::init);
            let window = cx.add_window(|window, cx| {
                let view = cx.new(|_| gpui::Empty);
                brrritscold_core_gpui::Root::new(view, window, cx)
            });
            VisualTestContext::from_window(*window, cx).into_mut()
        }

        #[gpui::test]
        fn should_start_the_os_handler_on_the_url(cx: &mut TestAppContext) {
            let cx = window(cx);
            let opener = cx.update(|_, cx| fake::install(cx));

            cx.update(|window, cx| open(URL, window, cx));

            assert_eq!(
                *opener.started.lock().unwrap(),
                [["xdg-open".to_owned(), URL.to_owned()]]
            );
        }

        #[gpui::test]
        fn should_start_nothing_for_a_url_it_refuses(cx: &mut TestAppContext) {
            let cx = window(cx);
            let opener = cx.update(|_, cx| fake::install(cx));

            cx.update(|window, cx| open("https://evil.io/acme/widgets/pull/4", window, cx));

            assert!(opener.started.lock().unwrap().is_empty());
            assert!(cx.update(|_, cx| crate::notice::shown(cx)).is_empty());
        }

        #[gpui::test]
        fn should_toast_when_the_handler_cant_start(cx: &mut TestAppContext) {
            let cx = window(cx);
            let opener = cx.update(|_, cx| fake::install(cx));
            *opener.failure.lock().unwrap() = Some("No such file or directory".into());

            cx.update(|window, cx| open(URL, window, cx));

            let shown = cx.update(|_, cx| crate::notice::shown(cx));
            let toast = shown.last().expect("a toast");
            assert_eq!(toast.title, "Couldn't open the PR");
            assert_eq!(
                toast.description.as_deref(),
                Some("xdg-open: No such file or directory")
            );
        }
    }
}
