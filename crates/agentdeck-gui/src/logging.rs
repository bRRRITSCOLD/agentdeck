//! The GUI's log (spec §11): `tracing` events, and GPUI's `log` records through the
//! `tracing-log` bridge, to stderr and to `agentdeck-gui.log` in the state dir.

use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::Mutex;

use anyhow::Context as _;
use tracing::Subscriber;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::{BoxMakeWriter, MakeWriterExt as _};
use tracing_subscriber::util::SubscriberInitExt as _;

/// The log's file name, in the state dir.
pub const FILE: &str = "agentdeck-gui.log";

/// Installs the log subscriber for the whole app, with the `log` bridge. See [`subscriber`]
/// for where it writes. Logging never stops the GUI from starting: a subscriber that can't be
/// installed is said on stderr, and a file that can't be opened is logged to stderr.
pub fn init(file: Option<&Path>, rust_log: Option<&str>) {
    let (subscriber, unopened) = subscriber(file, rust_log);
    if let Err(err) = subscriber.try_init() {
        eprintln!("agentdeck-gui: installing the log subscriber: {err}");
        return;
    }
    if let Some(err) = unopened {
        tracing::warn!("logging to stderr alone: {err:#}");
    }
}

/// Events at the levels `rust_log` (`RUST_LOG`'s value) picks, `info` and above without it
/// or when it doesn't parse, to stderr and appended to `file`, creating its dir. Without a
/// file, or when it can't be opened, to stderr alone; the error says why.
pub(crate) fn subscriber(
    file: Option<&Path>,
    rust_log: Option<&str>,
) -> (
    impl Subscriber + Send + Sync + 'static,
    Option<anyhow::Error>,
) {
    let filter = rust_log
        .and_then(|directives| EnvFilter::try_new(directives).ok())
        .unwrap_or_else(|| EnvFilter::new("info"));
    let (writer, unopened) = match file.map(open) {
        Some(Ok(file)) => (
            BoxMakeWriter::new(std::io::stderr.and(Mutex::new(file))),
            None,
        ),
        Some(Err(err)) => (BoxMakeWriter::new(std::io::stderr), Some(err)),
        None => (BoxMakeWriter::new(std::io::stderr), None),
    };
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(filter)
        .with_writer(writer)
        .finish();
    (subscriber, unopened)
}

/// `path` for appending, creating its dir first.
fn open(path: &Path) -> anyhow::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `<temp>/state/agentdeck/agentdeck-gui.log`, whose dir doesn't exist yet.
    fn log_file(root: &tempfile::TempDir) -> std::path::PathBuf {
        root.path().join("state/agentdeck").join(FILE)
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    mod subscriber {
        use super::*;

        #[test]
        fn should_append_events_to_the_file_creating_its_dir() {
            let root = tempfile::tempdir().unwrap();
            let path = log_file(&root);

            let (subscriber, unopened) = subscriber(Some(&path), None);
            tracing::subscriber::with_default(subscriber, || {
                tracing::warn!("saving the window failed");
            });

            assert!(unopened.is_none(), "{unopened:?}");
            assert!(
                read(&path).contains("saving the window failed"),
                "{}",
                read(&path)
            );
        }

        #[test]
        fn should_keep_what_an_earlier_run_wrote() {
            let root = tempfile::tempdir().unwrap();
            let path = log_file(&root);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "an earlier run\n").unwrap();

            let (subscriber, _) = subscriber(Some(&path), None);
            tracing::subscriber::with_default(subscriber, || tracing::info!("this run"));

            let text = read(&path);
            assert!(text.starts_with("an earlier run\n"), "{text}");
            assert!(text.contains("this run"), "{text}");
        }

        #[test]
        fn should_leave_out_debug_lines_without_rust_log() {
            let root = tempfile::tempdir().unwrap();
            let path = log_file(&root);

            let (subscriber, _) = subscriber(Some(&path), None);
            tracing::subscriber::with_default(subscriber, || {
                tracing::debug!("refused to open a link");
                tracing::info!("connected");
            });

            let text = read(&path);
            assert!(!text.contains("refused to open a link"), "{text}");
            assert!(text.contains("connected"), "{text}");
        }

        #[test]
        fn should_keep_debug_lines_when_rust_log_asks_for_them() {
            let root = tempfile::tempdir().unwrap();
            let path = log_file(&root);

            let (subscriber, _) = subscriber(Some(&path), Some("debug"));
            tracing::subscriber::with_default(subscriber, || {
                tracing::debug!("refused to open a link");
            });

            assert!(
                read(&path).contains("refused to open a link"),
                "{}",
                read(&path)
            );
        }

        #[test]
        fn should_say_why_when_the_file_cant_be_opened() {
            let root = tempfile::tempdir().unwrap();
            // A file where the state dir should be.
            let blocker = root.path().join("state");
            std::fs::write(&blocker, "").unwrap();
            let path = blocker.join("agentdeck").join(FILE);

            let (_, unopened) = subscriber(Some(&path), None);

            let message = format!("{:#}", unopened.expect("an error for the unopened file"));
            assert!(
                message.contains(&blocker.display().to_string()),
                "{message}"
            );
        }
    }

    mod init {
        use super::*;

        // The only test that installs the global subscriber and the `log` bridge: one per
        // process.
        #[test]
        fn should_bring_gpuis_log_records_into_the_file() {
            let root = tempfile::tempdir().unwrap();
            let path = log_file(&root);

            init(Some(&path), None);
            log::warn!("a record from GPUI");
            tracing::warn!("an event from agentdeck");

            let text = read(&path);
            assert!(text.contains("a record from GPUI"), "{text}");
            assert!(text.contains("an event from agentdeck"), "{text}");
        }
    }
}
