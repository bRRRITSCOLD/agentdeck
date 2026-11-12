//! Resume after a restart (spec §6): every pane that was alive when the daemon last stopped
//! is spawned again in its `cwd`. Claude Code and Codex panes resume their session; every
//! other pane comes back as a fresh shell. A pane that can't be spawned is marked exited.

use std::path::PathBuf;

use crate::agents::resume_argv;
use crate::db::{self, Db, DbError};
use crate::terminal::Terminals;
use crate::terminal::env::default_shell;
use crate::terminal::pane::Launch;

/// Re-spawns every pane the database holds as not exited.
pub async fn resume_panes(db: &Db, terminals: &Terminals) -> Result<(), DbError> {
    let panes = db.call(|c| db::panes::resumable(c)).await?;
    for (pane, workspace) in panes {
        let (argv, rules) = match (
            resume_argv(pane.agent, pane.session_id.as_deref()),
            pane.agent,
        ) {
            (Some(argv), Some(agent)) => (argv, agent.as_str()),
            _ => (vec![default_shell()], "shell"),
        };
        let launch = Launch::pane(pane.id, workspace, argv, PathBuf::from(&pane.cwd));
        match terminals.spawn(pane.id, launch, rules) {
            Ok(()) => tracing::info!("resumed pane {}", pane.id),
            Err(err) => {
                tracing::warn!("resuming pane {} in {}: {err}", pane.id, pane.cwd);
                let (id, title) = (pane.id, pane.title);
                db.call(move |c| db::panes::set_exited(c, id, None, &title))
                    .await?;
            }
        }
    }
    Ok(())
}
