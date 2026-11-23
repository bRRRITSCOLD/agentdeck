//! File paths in a pane (spec §11): `src/main.rs:42:7` in the text, and `file://` URIs from
//! OSC 8 links and OSC 7. A path is a link only once it resolves to an existing regular file.

use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use alacritty_terminal::index::Point;

use super::link::trim_url;

/// A file path in the text and the grid cells it covers, `:line:col` included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathLink {
    /// The path as written.
    pub path: PathText,
    /// Its cells, first to last; a path that soft-wraps covers several lines.
    pub cells: RangeInclusive<Point>,
}

/// A file path as the text gives it, with the line and column it names, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathText {
    /// The path as written: relative, absolute or `~/…`.
    pub path: String,
    /// The line after it (`:42`).
    pub line: Option<u32>,
    /// The column after the line (`:42:7`).
    pub col: Option<u32>,
    /// How many characters of the text it covers, `:line:col` included.
    pub len: usize,
}

/// The path in `text`, a run of characters with no spaces, quotes or brackets, without the
/// punctuation that ends a sentence around it and with its `:line[:col]`. Anything after the
/// column (`src/main.rs:42:fn main`) is left out. `None` for text that doesn't look like a
/// path: one with neither a `/` nor a `.` in it.
pub fn parse(text: &str) -> Option<PathText> {
    let mut parts = trim_url(text).split(':');
    let path = parts.next()?;
    if !(path.contains('/') || path.contains('.')) || path.chars().all(|c| c == '.') {
        return None;
    }
    let mut len = path.chars().count();
    let mut number = || {
        let part = parts.next()?;
        let n = part
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())??;
        len += 1 + part.len();
        Some(n)
    };
    let line = number();
    let col = line.and_then(|_| number());
    Some(PathText {
        path: path.to_owned(),
        line,
        col,
        len,
    })
}

/// Where `path` points: against `home` for `~/…`, as is when absolute, else against `cwd`.
/// `None` for `~user/…`, or for `~` with no home.
pub fn resolve(path: &str, cwd: &Path, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(rest) = path.strip_prefix('~') {
        let home = home?;
        if rest.is_empty() {
            return Some(home.to_path_buf());
        }
        return Some(home.join(rest.strip_prefix('/')?));
    }
    Some(cwd.join(path))
}

/// Whether `path` is an existing regular file, or a link to one.
pub fn is_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
}

pub use agentdeck_core::state::osc::file_uri;

/// This machine's hostname, read once.
pub fn this_host() -> Option<&'static str> {
    static HOST: OnceLock<Option<String>> = OnceLock::new();
    HOST.get_or_init(|| {
        std::fs::read_to_string("/proc/sys/kernel/hostname")
            .ok()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
    })
    .as_deref()
}

/// The user's home dir, from `$HOME`.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    mod parse {
        use super::*;

        fn parsed(text: &str) -> Option<(String, Option<u32>, Option<u32>, usize)> {
            parse(text).map(|p| (p.path, p.line, p.col, p.len))
        }

        #[test]
        fn should_read_a_path_alone() {
            for path in ["src/main.rs", "./a/b.txt", "/abs/path", "~/x", "Cargo.toml"] {
                assert_eq!(
                    parsed(path),
                    Some((path.to_owned(), None, None, path.len())),
                    "{path}"
                );
            }
        }

        #[test]
        fn should_read_the_line_and_column() {
            assert_eq!(
                parsed("src/main.rs:42"),
                Some(("src/main.rs".into(), Some(42), None, 14))
            );
            assert_eq!(
                parsed("src/main.rs:42:7"),
                Some(("src/main.rs".into(), Some(42), Some(7), 16))
            );
        }

        #[test]
        fn should_leave_out_what_follows_the_column() {
            assert_eq!(
                parsed("src/main.rs:42:fn"),
                Some(("src/main.rs".into(), Some(42), None, 14))
            );
            assert_eq!(
                parsed("src/main.rs:42:7:warning"),
                Some(("src/main.rs".into(), Some(42), Some(7), 16))
            );
        }

        #[test]
        fn should_leave_out_trailing_sentence_punctuation() {
            assert_eq!(
                parsed("src/main.rs."),
                Some(("src/main.rs".into(), None, None, 11))
            );
            assert_eq!(
                parsed("src/main.rs:42:"),
                Some(("src/main.rs".into(), Some(42), None, 14))
            );
            assert_eq!(
                parsed("a/b.txt:3,"),
                Some(("a/b.txt".into(), Some(3), None, 9))
            );
        }

        #[test]
        fn should_refuse_text_that_is_not_a_path() {
            for text in ["hello", "12:30", "", ":42", "...", "https"] {
                assert_eq!(parsed(text), None, "{text}");
            }
        }
    }

    mod resolve {
        use super::*;

        fn resolved(path: &str, home: Option<&str>) -> Option<PathBuf> {
            resolve(path, Path::new("/w/repo"), home.map(Path::new))
        }

        #[test]
        fn should_resolve_a_relative_path_against_the_cwd() {
            assert_eq!(
                resolved("src/main.rs", None),
                Some("/w/repo/src/main.rs".into())
            );
            assert_eq!(resolved("./a.txt", None), Some("/w/repo/./a.txt".into()));
        }

        #[test]
        fn should_keep_an_absolute_path() {
            assert_eq!(resolved("/etc/hosts", None), Some("/etc/hosts".into()));
        }

        #[test]
        fn should_resolve_a_tilde_against_home() {
            assert_eq!(
                resolved("~/x.txt", Some("/home/me")),
                Some("/home/me/x.txt".into())
            );
            assert_eq!(resolved("~", Some("/home/me")), Some("/home/me".into()));
        }

        #[test]
        fn should_refuse_a_tilde_it_cannot_resolve() {
            assert_eq!(resolved("~/x.txt", None), None);
            assert_eq!(resolved("~other/x.txt", Some("/home/me")), None);
        }
    }

    mod is_file {
        use super::*;

        #[test]
        fn should_be_true_only_for_an_existing_regular_file() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("a.txt");
            std::fs::write(&file, "a").unwrap();

            assert!(is_file(&file));
            assert!(!is_file(dir.path()));
            assert!(!is_file(&dir.path().join("missing.txt")));
        }
    }
}
