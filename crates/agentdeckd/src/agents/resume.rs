//! Resume after a restart (spec §6): every pane that was alive when the daemon last stopped
//! is spawned again in its `cwd`. Claude Code and Codex panes resume their session, with
//! agentdeck's MCP server; every other pane comes back as a fresh shell. A pane that can't be spawned is marked exited.
//! A shell pane comes back as its shell alone, hosting no agent, whatever it hosted before.

use std::path::PathBuf;

use agentdeck_core::PaneKind;

use crate::agents::{cli, resume_argv};
use crate::db::{self, Db, DbError};
use crate::terminal::Terminals;
use crate::terminal::env::default_shell;
use crate::terminal::pane::Launch;

/// Re-spawns every pane the database holds as not exited.
pub async fn resume_panes(db: &Db, terminals: &Terminals) -> Result<(), DbError> {
    db.call(|c| db::panes::forget_hosted_agents(c)).await?;
    let panes = db.call(|c| db::panes::resumable(c)).await?;
    let cli = cli();
    for (pane, workspace) in panes {
        let (argv, rules) = match (
            resume_argv(pane.agent, pane.session_id.as_deref(), &cli),
            pane.agent,
        ) {
            (Some(argv), Some(agent)) => (argv, Some(agent.as_str())),
            _ if pane.kind == PaneKind::Shell => (vec![default_shell()], None),
            _ => (vec![default_shell()], Some("shell")),
        };
        let launch = Launch::pane(
            pane.id,
            workspace,
            pane.task_id,
            argv,
            PathBuf::from(&pane.cwd),
        );
        let spawned = match rules {
            Some(rules) => terminals.spawn(pane.id, launch, rules),
            None => terminals.spawn_shell(pane.id, launch),
        };
        match spawned {
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
