//! What the sidebar shows (spec §11): every workspace with its rolled-up state, unread notes
//! and open PRs, the inbox's counts, and when PRs last synced.
//!
//! A [`Snapshot`] is the daemon's data as read over REST; [`Snapshot::overview`] sums it up,
//! a pure function the views render. Live `state` and `exit` messages patch the snapshot's
//! pane states in place; anything else that changes is read again.

use agentdeck_client::types::{Note, PrSyncStatus, Task, Workspace};
use agentdeck_core::{Agent, AgentState, PaneId, TaskId, TaskKind, TaskStatus, WorkspaceId};
use chrono::{DateTime, Utc};
use gpui::SharedString;

/// One pane's state, the workspace whose tab holds it, and the task it links.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneState {
    /// The pane.
    pub pane: PaneId,
    /// The workspace it sits in.
    pub workspace: WorkspaceId,
    /// Its state.
    pub state: AgentState,
    /// The agent it runs; `None` for a shell.
    pub agent: Option<Agent>,
    /// The task it links.
    pub task: Option<TaskId>,
}

/// The daemon's data behind the sidebar, as read over REST.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// Every workspace, archived ones included, oldest first.
    pub workspaces: Vec<Workspace>,
    /// Every pane placed in a tab's layout.
    pub panes: Vec<PaneState>,
    /// Every task.
    pub tasks: Vec<Task>,
    /// The unread notes.
    pub unread_notes: Vec<Note>,
    /// PR sync's last run.
    pub pr_sync: PrSyncStatus,
}

impl Default for Snapshot {
    fn default() -> Self {
        Snapshot {
            workspaces: Vec::new(),
            panes: Vec::new(),
            tasks: Vec::new(),
            unread_notes: Vec::new(),
            pr_sync: PrSyncStatus {
                last_error: None,
                last_synced_at: None,
                login: agentdeck_client::types::GhLogin::LoggedIn,
            },
        }
    }
}

/// A row's counts; a zero hides its count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Unread notes.
    pub unread_notes: usize,
    /// PR tasks that aren't done.
    pub prs: usize,
}

/// A workspace's sidebar row.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceRow {
    /// The workspace.
    pub id: WorkspaceId,
    /// Its name.
    pub name: SharedString,
    /// Its directory's absolute path.
    pub dir: SharedString,
    /// Its panes' states, rolled up.
    pub state: AgentState,
    /// Its counts.
    pub counts: Counts,
}

/// Where PR sync stands, for the sidebar's footer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrSync {
    /// No sync has finished yet.
    NotYet,
    /// The most recent sync failed.
    Failed,
    /// The last sync finished then.
    SyncedAt(DateTime<Utc>),
}

impl PrSync {
    /// The footer's text, at `now`.
    pub fn label(self, now: DateTime<Utc>) -> String {
        match self {
            PrSync::NotYet => "PRs not synced yet".to_owned(),
            PrSync::Failed => "PR sync failed".to_owned(),
            PrSync::SyncedAt(at) => format!("PRs synced {}", ago(now - at)),
        }
    }

    /// The Inbox header's text, at `now`: the footer's without "PRs".
    pub fn short_label(self, now: DateTime<Utc>) -> String {
        match self {
            PrSync::NotYet => "Not synced yet".to_owned(),
            PrSync::Failed => "Sync failed".to_owned(),
            PrSync::SyncedAt(at) => format!("Synced {}", ago(now - at)),
        }
    }
}

/// How long `since` is, as the sync labels say it.
fn ago(since: chrono::TimeDelta) -> String {
    let minutes = since.num_minutes().max(0);
    match minutes {
        0 => "just now".to_owned(),
        1..60 => format!("{minutes} min ago"),
        60..1440 => format!("{} h ago", minutes / 60),
        _ => format!("{} d ago", minutes / 1440),
    }
}

/// The sidebar's content, summed up from a [`Snapshot`].
#[derive(Debug, Clone, PartialEq)]
pub struct Overview {
    /// The inbox's counts: notes on, and open PRs among, tasks with no workspace.
    pub inbox: Counts,
    /// The workspaces that aren't archived, oldest first.
    pub workspaces: Vec<WorkspaceRow>,
    /// PR sync.
    pub pr_sync: PrSync,
}

impl Snapshot {
    /// Sets `pane`'s state, as a `state` or `exit` message reports it. A pane the snapshot
    /// doesn't hold is ignored: the `changed` that adds it brings its state.
    pub fn set_state(&mut self, pane: PaneId, state: AgentState) {
        if let Some(entry) = self.panes.iter_mut().find(|entry| entry.pane == pane) {
            entry.state = state;
        }
    }

    /// The sidebar's content.
    pub fn overview(&self) -> Overview {
        let workspace_of_task = |task_id| {
            self.tasks
                .iter()
                .find(|task| task.id == task_id)
                .map(|task| task.workspace_id)
        };
        // A note's workspace: its own, or its task's. `Some(None)` is an inbox task's note.
        let note_workspace = |note: &Note| match (note.workspace_id, note.task_id) {
            (Some(workspace), _) => Some(Some(workspace)),
            (None, Some(task)) => workspace_of_task(task),
            (None, None) => None,
        };
        let counts = |workspace: Option<WorkspaceId>| Counts {
            unread_notes: self
                .unread_notes
                .iter()
                .filter(|note| note_workspace(note) == Some(workspace))
                .count(),
            prs: self
                .tasks
                .iter()
                .filter(|task| {
                    task.kind == TaskKind::Pr
                        && task.status != TaskStatus::Done
                        && task.workspace_id == workspace
                })
                .count(),
        };
        let workspaces = self
            .workspaces
            .iter()
            .filter(|workspace| workspace.archived_at.is_none())
            .map(|workspace| WorkspaceRow {
                id: workspace.id,
                name: workspace.name.clone().into(),
                dir: workspace.dir.clone().into(),
                state: AgentState::rollup(
                    self.panes
                        .iter()
                        .filter(|pane| pane.workspace == workspace.id)
                        .map(|pane| pane.state),
                ),
                counts: counts(Some(workspace.id)),
            })
            .collect();
        let synced_at = self
            .pr_sync
            .last_synced_at
            .as_deref()
            .and_then(|at| DateTime::parse_from_rfc3339(at).ok());
        let pr_sync = match (&self.pr_sync.last_error, synced_at) {
            (Some(_), _) => PrSync::Failed,
            (None, Some(at)) => PrSync::SyncedAt(at.to_utc()),
            (None, None) => PrSync::NotYet,
        };
        Overview {
            inbox: counts(None),
            workspaces,
            pr_sync,
        }
    }
}

/// Builders for snapshots in tests.
#[cfg(test)]
pub(crate) mod fixtures {
    use agentdeck_client::types::{Note, Task, Workspace};
    use agentdeck_core::{AgentState, PaneId, TaskKind, TaskStatus, WorkspaceId};

    use super::PaneState;

    const CREATED: &str = "2026-11-12T00:00:00Z";

    /// A fixed id ending in `n`, for any id type.
    pub fn id<T: std::str::FromStr<Err: std::fmt::Debug>>(n: u32) -> T {
        format!("00000000-0000-7000-8000-{n:012}").parse().unwrap()
    }

    /// A workspace named `name`.
    pub fn workspace(n: u32, name: &str) -> Workspace {
        Workspace {
            archived_at: None,
            created_at: CREATED.into(),
            dir: format!("/w/{name}"),
            dir_owned: false,
            id: id(n),
            muted: false,
            name: name.into(),
            slug: name.into(),
        }
    }

    /// A tab of `workspace` at `position`, holding `pane` alone, or no pane.
    pub fn tab(
        n: u32,
        workspace: WorkspaceId,
        position: i64,
        pane: Option<PaneId>,
    ) -> agentdeck_client::types::Tab {
        agentdeck_client::types::Tab {
            id: id(n),
            layout: pane.map(agentdeck_core::Layout::single),
            position,
            title: format!("tab {n}"),
            workspace_id: workspace,
        }
    }

    /// A pane in `workspace`.
    pub fn pane(n: u32, workspace: WorkspaceId, state: AgentState) -> PaneState {
        PaneState {
            pane: id::<PaneId>(n),
            workspace,
            state,
            agent: None,
            task: None,
        }
    }

    /// A task of `kind` in `workspace`, or the inbox.
    pub fn task(n: u32, kind: TaskKind, workspace: Option<WorkspaceId>) -> Task {
        Task {
            body: String::new(),
            created_at: CREATED.into(),
            done_at: None,
            id: id(n),
            kind,
            pr: None,
            status: TaskStatus::Todo,
            title: format!("task {n}"),
            updated_at: CREATED.into(),
            workspace_id: workspace,
        }
    }

    /// An unread note on a workspace.
    pub fn workspace_note(n: u32, workspace: WorkspaceId) -> Note {
        Note {
            body: "note".into(),
            created_at: CREATED.into(),
            id: id(n),
            read_at: None,
            read_by_pane: None,
            task_id: None,
            workspace_id: Some(workspace),
        }
    }

    /// An unread note on a task.
    pub fn task_note(n: u32, task: &Task) -> Note {
        Note {
            task_id: Some(task.id),
            workspace_id: None,
            ..workspace_note(n, id(0))
        }
    }
}

#[cfg(test)]
mod tests {
    use agentdeck_core::AgentState::*;
    use agentdeck_core::{TaskKind, TaskStatus};

    use super::fixtures::*;
    use super::*;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339).unwrap().to_utc()
    }

    mod overview {
        use super::*;

        mod workspaces {
            use super::*;

            #[test]
            fn should_roll_up_each_workspaces_panes() {
                let (a, b) = (workspace(1, "a"), workspace(2, "b"));
                let snapshot = Snapshot {
                    panes: vec![
                        pane(10, a.id, Done),
                        pane(11, a.id, Blocked),
                        pane(12, b.id, Working),
                    ],
                    workspaces: vec![a, b],
                    ..Snapshot::default()
                };

                let states: Vec<_> = snapshot
                    .overview()
                    .workspaces
                    .iter()
                    .map(|row| (row.name.to_string(), row.state))
                    .collect();

                assert_eq!(states, [("a".into(), Blocked), ("b".into(), Working)]);
            }

            #[test]
            fn should_be_idle_with_no_panes() {
                let snapshot = Snapshot {
                    workspaces: vec![workspace(1, "a")],
                    ..Snapshot::default()
                };

                assert_eq!(snapshot.overview().workspaces[0].state, Idle);
            }

            #[test]
            fn should_leave_out_archived_workspaces() {
                let archived = Workspace {
                    archived_at: Some("2026-11-12T01:00:00Z".into()),
                    ..workspace(2, "old")
                };
                let snapshot = Snapshot {
                    workspaces: vec![workspace(1, "a"), archived],
                    ..Snapshot::default()
                };

                let names: Vec<_> = snapshot
                    .overview()
                    .workspaces
                    .iter()
                    .map(|row| row.name.to_string())
                    .collect();

                assert_eq!(names, ["a"]);
            }
        }

        mod counts {
            use super::*;

            #[test]
            fn should_count_notes_on_the_workspace_and_on_its_tasks() {
                let a = workspace(1, "a");
                let task = task(20, TaskKind::Plain, Some(a.id));
                let snapshot = Snapshot {
                    unread_notes: vec![workspace_note(30, a.id), task_note(31, &task)],
                    tasks: vec![task],
                    workspaces: vec![a],
                    ..Snapshot::default()
                };

                assert_eq!(snapshot.overview().workspaces[0].counts.unread_notes, 2);
            }

            #[test]
            fn should_count_pr_tasks_that_are_not_done() {
                let a = workspace(1, "a");
                let done = Task {
                    status: TaskStatus::Done,
                    ..task(22, TaskKind::Pr, Some(a.id))
                };
                let snapshot = Snapshot {
                    tasks: vec![
                        task(20, TaskKind::Pr, Some(a.id)),
                        task(21, TaskKind::Plain, Some(a.id)),
                        done,
                    ],
                    workspaces: vec![a],
                    ..Snapshot::default()
                };

                assert_eq!(snapshot.overview().workspaces[0].counts.prs, 1);
            }

            #[test]
            fn should_count_the_inbox_from_tasks_with_no_workspace() {
                let a = workspace(1, "a");
                let inbox_pr = task(20, TaskKind::Pr, None);
                let snapshot = Snapshot {
                    unread_notes: vec![task_note(30, &inbox_pr)],
                    tasks: vec![inbox_pr, task(21, TaskKind::Pr, Some(a.id))],
                    workspaces: vec![a],
                    ..Snapshot::default()
                };

                assert_eq!(
                    snapshot.overview().inbox,
                    Counts {
                        unread_notes: 1,
                        prs: 1
                    }
                );
            }
        }

        mod pr_sync {
            use super::*;

            #[test]
            fn should_report_the_last_sync() {
                let mut snapshot = Snapshot::default();
                snapshot.pr_sync.last_synced_at = Some("2026-11-12T10:00:00Z".into());

                assert_eq!(
                    snapshot.overview().pr_sync,
                    PrSync::SyncedAt(at("2026-11-12T10:00:00Z"))
                );
            }

            #[test]
            fn should_report_a_failure_over_an_earlier_sync() {
                let mut snapshot = Snapshot::default();
                snapshot.pr_sync.last_synced_at = Some("2026-11-12T10:00:00Z".into());
                snapshot.pr_sync.last_error = Some("gh: rate limited".into());

                assert_eq!(snapshot.overview().pr_sync, PrSync::Failed);
            }

            #[test]
            fn should_report_none_before_the_first_sync() {
                assert_eq!(Snapshot::default().overview().pr_sync, PrSync::NotYet);
            }
        }
    }

    mod set_state {
        use super::*;

        #[test]
        fn should_change_the_rolled_up_state() {
            let a = workspace(1, "a");
            let mut snapshot = Snapshot {
                panes: vec![pane(10, a.id, Working)],
                workspaces: vec![a],
                ..Snapshot::default()
            };

            snapshot.set_state(id(10), Blocked);

            assert_eq!(snapshot.overview().workspaces[0].state, Blocked);
        }
    }

    mod pr_sync_label {
        use super::*;

        const NOW: &str = "2026-11-12T12:00:00Z";

        fn synced(at_: &str) -> String {
            PrSync::SyncedAt(at(at_)).label(at(NOW))
        }

        #[test]
        fn should_say_just_now_within_a_minute() {
            assert_eq!(synced("2026-11-12T11:59:30Z"), "PRs synced just now");
        }

        #[test]
        fn should_count_minutes_within_an_hour() {
            assert_eq!(synced("2026-11-12T11:59:00Z"), "PRs synced 1 min ago");
            assert_eq!(synced("2026-11-12T11:01:00Z"), "PRs synced 59 min ago");
        }

        #[test]
        fn should_count_hours_within_a_day() {
            assert_eq!(synced("2026-11-12T09:30:00Z"), "PRs synced 2 h ago");
        }

        #[test]
        fn should_count_days_after_that() {
            assert_eq!(synced("2026-11-10T12:00:00Z"), "PRs synced 2 d ago");
        }

        #[test]
        fn should_say_just_now_for_a_sync_stamped_after_now() {
            assert_eq!(synced("2026-11-12T12:00:05Z"), "PRs synced just now");
        }
        #[test]
        fn should_drop_prs_from_the_inbox_headers_label() {
            let short = |sync: PrSync| sync.short_label(at(NOW));

            assert_eq!(
                short(PrSync::SyncedAt(at("2026-11-12T11:59:00Z"))),
                "Synced 1 min ago"
            );
            assert_eq!(short(PrSync::NotYet), "Not synced yet");
            assert_eq!(short(PrSync::Failed), "Sync failed");
        }
    }
}
