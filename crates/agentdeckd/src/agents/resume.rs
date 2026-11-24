//! Resume after a restart (spec §6): every pane that was alive when the daemon last stopped
//! is spawned again in its last known folder, or in `~` once that folder is gone. Claude Code and Codex panes resume their session;
//! every other pane comes back as a fresh shell, with hosting on. An
//! agent pane that comes back as a shell becomes a shell pane (spec §3) before it starts. A
//! pane that can't be spawned is marked exited. A shell pane comes back as its shell alone,
//! hosting no agent, whatever it hosted before. A resumed agent that exits non-zero at once,
//! as one whose session can't be found does, starts afresh once ([`super::restart`]).

use std::path::{Path, PathBuf};

use agentdeck_core::PaneKind;

use crate::agents::restart::{Restarter, existing_dir, home};
use crate::agents::resume_argv;
use crate::db::{self, DbError};
use crate::terminal::env::default_shell;
use crate::terminal::pane::Launch;

/// Re-spawns every pane the database holds as not exited.
pub async fn resume_panes(restarter: &Restarter) -> Result<(), DbError> {
    let (db, terminals) = (&restarter.db, &restarter.terminals);
    db.call(|c| db::panes::forget_hosted_agents(c)).await?;
    let panes = db.call(|c| db::panes::resumable(c)).await?;
    for (mut pane, workspace) in panes {
        let resumed = resume_argv(pane.agent, pane.session_id.as_deref());
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
        let Some(cwd) = restore_folder(Path::new(&pane.folder), home().as_deref()) else {
            tracing::warn!(
                "resuming pane {}: {} is gone, and so is ~",
                pane.id,
                pane.folder
            );
            let (id, title) = (pane.id, pane.title);
            db.call(move |c| db::panes::set_exited(c, id, None, &title))
                .await?;
            continue;
        };
        if cwd != Path::new(&pane.folder) {
            tracing::info!(
                "pane {}'s folder {} is gone: resuming it in ~",
                pane.id,
                pane.folder
            );
            let (id, folder) = (pane.id, cwd.to_string_lossy().into_owned());
            db.call(move |c| db::panes::set_folder(c, id, Some(&folder)))
                .await?;
        }
        let launch = Launch::pane(pane.id, workspace, argv, cwd.clone());
        let feed = restarter.events.subscribe();
        let spawned = match agent {
            Some(agent) => terminals.spawn(pane.id, launch, agent.as_str()),
            None => terminals.spawn_shell(pane.id, launch),
        };
        match spawned {
            Ok(()) => {
                tracing::info!("resumed pane {}", pane.id);
                if let Some(agent) = agent {
                    restarter.fall_back_if_resume_fails(pane.id, agent, cwd, feed);
                }
            }
            Err(err) => {
                tracing::warn!("resuming pane {} in {}: {err}", pane.id, cwd.display());
                let (id, title) = (pane.id, pane.title);
                db.call(move |c| db::panes::set_exited(c, id, None, &title))
                    .await?;
            }
        }
    }
    Ok(())
}

/// Where a pane resumes (spec §6): its last known `folder` while it is a directory, else
/// `home` while that is; `None` when neither is.
fn restore_folder(folder: &Path, home: Option<&Path>) -> Option<PathBuf> {
    existing_dir([Some(folder), home])
}

#[cfg(test)]
mod tests {
    use super::*;

    mod restore_folder {
        use super::*;

        #[test]
        fn should_be_the_last_known_folder_while_it_is_there() {
            let folder = tempfile::tempdir().unwrap();
            let home = tempfile::tempdir().unwrap();

            let restored = restore_folder(folder.path(), Some(home.path()));

            assert_eq!(restored.as_deref(), Some(folder.path()));
        }

        #[test]
        fn should_be_home_once_the_folder_is_gone() {
            let home = tempfile::tempdir().unwrap();

            let restored = restore_folder(&home.path().join("gone"), Some(home.path()));

            assert_eq!(restored.as_deref(), Some(home.path()));
        }

        #[test]
        fn should_be_none_without_either() {
            assert_eq!(restore_folder(Path::new("/no/such/dir"), None), None);
        }
    }
}
