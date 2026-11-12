//! `tracing` to the log file and stderr (spec §2), with dropshot's `slog` records bridged in.

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::sync::Mutex;

use anyhow::Context;
use tracing_subscriber::fmt::writer::MakeWriterExt;

use crate::paths::Paths;

/// Sends every event to stderr and to `$XDG_STATE_HOME/agentdeck/agentdeckd.log`.
pub fn init(paths: &Paths) -> anyhow::Result<()> {
    std::fs::create_dir_all(&paths.state)
        .with_context(|| format!("creating {}", paths.state.display()))?;
    let log_file = paths.log_file();
    let file: File = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .with_context(|| format!("opening {}", log_file.display()))?;
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(std::io::stderr.and(Mutex::new(file)))
        .try_init()
        .map_err(|err| anyhow::anyhow!("installing the log subscriber: {err}"))
}

/// A `slog` logger for dropshot whose records become `tracing` events.
pub fn dropshot_logger() -> slog::Logger {
    slog::Logger::root(TracingDrain, slog::o!())
}

struct TracingDrain;

impl slog::Drain for TracingDrain {
    type Ok = ();
    type Err = slog::Never;

    fn log(
        &self,
        record: &slog::Record<'_>,
        values: &slog::OwnedKVList,
    ) -> Result<(), slog::Never> {
        let mut fields = Fields(String::new());
        let _ = slog::KV::serialize(&record.kv(), record, &mut fields);
        let _ = slog::KV::serialize(values, record, &mut fields);
        let message = format!("{}{}", record.msg(), fields.0);
        match record.level() {
            slog::Level::Critical | slog::Level::Error => {
                tracing::error!(target: "dropshot", "{message}")
            }
            slog::Level::Warning => tracing::warn!(target: "dropshot", "{message}"),
            slog::Level::Info => tracing::info!(target: "dropshot", "{message}"),
            slog::Level::Debug => tracing::debug!(target: "dropshot", "{message}"),
            slog::Level::Trace => tracing::trace!(target: "dropshot", "{message}"),
        }
        Ok(())
    }
}

/// Collects a record's key-values as ` key=value` pairs.
struct Fields(String);

impl slog::Serializer for Fields {
    fn emit_arguments(&mut self, key: slog::Key, val: &std::fmt::Arguments<'_>) -> slog::Result {
        let _ = write!(self.0, " {key}={val}");
        Ok(())
    }
}
