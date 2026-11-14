//! Links in a pane (spec §11): what a `secondary`-click opens, and which schemes it may open.

use std::ops::RangeInclusive;

use alacritty_terminal::index::Point;

/// A link under the pointer: an OSC 8 hyperlink, or a URL in the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// What it opens.
    pub uri: String,
    /// The grid cells it covers, first to last; a URL that soft-wraps covers several lines.
    pub cells: RangeInclusive<Point>,
}

/// Whether a click may open `uri` with the OS's handler: `http`, `https` and `mailto` only, so
/// a program can't make a click run a custom URL handler. A `file://` link goes to the file
/// opener instead (`path::file_uri`), and only for this host.
pub fn openable(uri: &str) -> bool {
    let Some((scheme, rest)) = uri.split_once(':') else {
        return false;
    };
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" => rest.starts_with("//") && rest.len() > 2,
        "mailto" => !rest.is_empty(),
        _ => false,
    }
}

/// `url` without the punctuation that ends the sentence around it, as terminals trim it:
/// trailing `.`, `,`, `:`, `;`, `!`, `?` and quotes, and a closing bracket it doesn't open.
pub fn trim_url(url: &str) -> &str {
    let mut url = url;
    loop {
        let unopened = |open: char, close: char| {
            url.chars().filter(|&c| c == close).count() > url.chars().filter(|&c| c == open).count()
        };
        let trim = match url.chars().next_back() {
            Some('.' | ',' | ':' | ';' | '!' | '?' | '\'' | '"') => true,
            Some(')') => unopened('(', ')'),
            Some(']') => unopened('[', ']'),
            _ => false,
        };
        if !trim {
            return url;
        }
        url = &url[..url.len() - 1];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod openable {
        use super::*;

        #[test]
        fn should_open_http_https_and_mailto() {
            for uri in [
                "http://example.com",
                "https://example.com/a?b=c#d",
                "mailto:me@example.com",
                "HTTPS://EXAMPLE.COM",
            ] {
                assert!(openable(uri), "{uri}");
            }
        }

        #[test]
        fn should_refuse_every_other_scheme() {
            for uri in [
                "file:///etc/passwd",
                "javascript:alert(1)",
                "ssh://host",
                "vscode://file/x",
                "example.com",
                "",
                "https:example.com",
            ] {
                assert!(!openable(uri), "{uri}");
            }
        }
    }

    mod trim_url {
        use super::*;

        #[test]
        fn should_drop_trailing_sentence_punctuation() {
            assert_eq!(trim_url("https://example.com/a."), "https://example.com/a");
            assert_eq!(trim_url("https://example.com,"), "https://example.com");
            assert_eq!(
                trim_url("https://example.com/?!:;'\""),
                "https://example.com/"
            );
        }

        #[test]
        fn should_drop_a_closing_bracket_it_does_not_open() {
            assert_eq!(trim_url("https://example.com/a)."), "https://example.com/a");
            assert_eq!(trim_url("https://example.com/a]"), "https://example.com/a");
        }

        #[test]
        fn should_keep_a_closing_bracket_it_opens() {
            assert_eq!(
                trim_url("https://en.wikipedia.org/wiki/Rust_(language))"),
                "https://en.wikipedia.org/wiki/Rust_(language)"
            );
        }

        #[test]
        fn should_keep_a_url_with_nothing_to_trim() {
            assert_eq!(trim_url("https://example.com/a"), "https://example.com/a");
        }
    }
}
