//! What the GUI reads from the daemon over REST (spec §4), behind [`Api`] so a view takes a
//! fake in tests. The real one is the connected [`Daemon`].

use agentdeck_client::Client;
use agentdeck_client::daemon::Daemon;
use gpui::{App, Task};

use crate::overview::{PaneState, Snapshot};
use crate::runtime;

/// The daemon's API, as the GUI uses it.
pub trait Api: Send + Sync {
    /// Everything the sidebar shows, read fresh.
    fn snapshot(&self, cx: &App) -> Task<anyhow::Result<Snapshot>>;
}

impl Api for Daemon {
    fn snapshot(&self, cx: &App) -> Task<anyhow::Result<Snapshot>> {
        runtime::spawn(cx, snapshot(self.client.clone()))
    }
}

/// Reads a [`Snapshot`]: the workspaces, then each live one's tabs and the panes in their
/// layouts, then the tasks, unread notes and PR sync status.
async fn snapshot(client: Client) -> anyhow::Result<Snapshot> {
    let workspaces = client.workspace_list().send().await?.into_inner();
    let mut panes = Vec::new();
    for workspace in workspaces.iter().filter(|w| w.archived_at.is_none()) {
        let tabs = client
            .tab_list()
            .id(workspace.id)
            .send()
            .await?
            .into_inner();
        for pane in tabs
            .iter()
            .filter_map(|tab| tab.layout.as_ref())
            .flat_map(|l| l.panes())
        {
            let pane = client.pane_get().id(pane).send().await?.into_inner();
            panes.push(PaneState {
                pane: pane.id,
                workspace: workspace.id,
                state: pane.state,
            });
        }
    }
    Ok(Snapshot {
        workspaces,
        panes,
        tasks: client.task_list().send().await?.into_inner(),
        unread_notes: client.note_list().unread(true).send().await?.into_inner(),
        pr_sync: client.prs_status().send().await?.into_inner(),
    })
}

/// A scripted [`Api`] for tests.
#[cfg(test)]
pub(crate) mod fake {
    use std::sync::Mutex;

    use super::*;

    /// Answers every read with the snapshot it holds now, and counts the reads.
    #[derive(Default)]
    pub struct FakeApi {
        snapshot: Mutex<Snapshot>,
        reads: Mutex<usize>,
    }

    impl FakeApi {
        /// A fake that answers with `snapshot`.
        pub fn with(snapshot: Snapshot) -> Self {
            FakeApi {
                snapshot: Mutex::new(snapshot),
                reads: Mutex::default(),
            }
        }

        /// Answers later reads with `snapshot`.
        pub fn set(&self, snapshot: Snapshot) {
            *self.snapshot.lock().unwrap() = snapshot;
        }

        /// How many snapshots were read.
        pub fn reads(&self) -> usize {
            *self.reads.lock().unwrap()
        }
    }

    impl Api for FakeApi {
        fn snapshot(&self, _cx: &App) -> Task<anyhow::Result<Snapshot>> {
            *self.reads.lock().unwrap() += 1;
            Task::ready(Ok(self.snapshot.lock().unwrap().clone()))
        }
    }
}
