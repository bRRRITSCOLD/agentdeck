//! Resume after a restart (spec §6): every pane that was alive when the daemon last stopped
//! is spawned again in its `cwd`. Claude Code and Codex panes resume their session, with
//! agentdeck's MCP server; every other pane comes back as a fresh shell, with hosting on. An
//! agent pane that comes back as a shell becomes a shell pane (spec §3) before it starts. A
//! pane that can't be spawned is marked exited. A shell pane comes back as its shell alone,
//! hosting no agent, whatever it hosted before.

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
    for (mut pane, workspace) in panes {
        let resumed = resume_argv(pane.agent, pane.session_id.as_deref(), &cli);
        let (argv, agent) = match (resumed, pane.agent) {
            (Some(argv), Some(agent)) => (argv, Some(agent)),
            _ => (vec![default_shell()], None),
        };
        if agent.is_none() && pane.kind == PaneKind::Agent {
            let (id, shell) = (pane.id, argv.clone());
            pane = db
                .call(move |c| db::panes::become_shell(c, id, &shell))
                .await?;
            tracing::info!("pane {id} comes back as a shell, so it is a shell pane now");
        }
        let launch = Launch::pane(
            pane.id,
            workspace,
            pane.task_id,
            argv,
            PathBuf::from(&pane.cwd),
        );
        let spawned = match agent {
            Some(agent) => terminals.spawn(pane.id, launch, agent.as_str()),
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
