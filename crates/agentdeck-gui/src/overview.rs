//! What the sidebar shows (spec §11): the Pull requests view's count, every workspace with its
//! rolled-up state and open PRs, the archived ones apart, the inbox's counts, and where PR sync
//! stands for the Pull requests view's header.
//!
//! A [`Snapshot`] is the daemon's data as read over REST; [`Snapshot::overview`] sums it up,
//! a pure function the views render. Live `state` and `exit` messages patch the snapshot's
//! pane states in place; anything else that changes is read again.

use agentdeck_client::types::{
    Instruction, PrSyncStatus, PullRequest, RepositoryLinks, Task, Workspace,
};
use agentdeck_core::{
    Agent, AgentState, PaneId, PrState, TaskId, TaskKind, TaskStatus, WorkspaceId,
};
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
    /// The agent it runs, or the one a shell pane hosts (spec §3); `None` for a shell
    /// hosting none.
    pub agent: Option<Agent>,
    /// The task it links.
    pub task: Option<TaskId>,
    /// The title of the tab it sits in.
    pub tab: String,
}

/// The daemon's data behind the sidebar, as read over REST; empty until read.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Every workspace, archived ones included, oldest first.
    pub workspaces: Vec<Workspace>,
    /// Every pane placed in a tab's layout.
    pub panes: Vec<PaneState>,
    /// Every task.
    pub tasks: Vec<Task>,
    /// Every workspace's instructions.
    pub instructions: Vec<Instruction>,
    /// PR sync's last run and its settings; `None` until read.
    pub pr_sync: Option<PrSyncStatus>,
    /// Every pull request PR sync keeps, by Repository name, then number (spec §4).
    pub prs: Vec<PullRequest>,
    /// Every Repository the daemon keeps, with the Workspaces linking it, archived ones
    /// included: what the PR alert mute rule reads (spec §11).
    pub repositories: Vec<RepositoryLinks>,
}

/// A row's counts; a zero hides its count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// PR tasks that aren't done, but for those whose PR the GitHub sign-in can't see.
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
    /// Its counts; none while archived.
    pub counts: Counts,
    /// Whether it is archived.
    pub archived: bool,
    /// Whether its notifications are muted.
    pub muted: bool,
}

/// Where PR sync stands, for the Pull requests view's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrSync {
    /// No sync has finished yet.
    NotYet,
    /// The most recent sync failed.
    Failed,
    /// No GitHub token GitHub accepts, so no sync can read GitHub.
    LoggedOut,
    /// The last sync finished then.
    SyncedAt(DateTime<Utc>),
    /// A sync is running, `prs_done` of its `prs` PRs read; `prs` is 0 until the first page
    /// answers.
    Syncing {
        /// PRs read so far.
        prs_done: u32,
        /// PRs expected.
        prs: u32,
        /// Fetching reviews and checks, after the last page: no count to show.
        details: bool,
    },
}

impl PrSync {
    /// The Pull requests view header's text, at `now`.
    pub fn label(self, now: DateTime<Utc>) -> String {
        match self {
            PrSync::NotYet => "Not synced yet".to_owned(),
            PrSync::Failed => "Sync failed".to_owned(),
            PrSync::LoggedOut => "Not signed in".to_owned(),
            PrSync::SyncedAt(at) => format!("Synced {}", ago(now - at)),
            PrSync::Syncing { details: true, .. } => "Syncing details…".to_owned(),
            PrSync::Syncing { prs_done, prs, .. } => {
                format!("Syncing…{}", of(prs_done, prs, " PRs"))
            }
        }
    }
}

/// " 100 of 271" and then `unit` once a sync knows how many PRs it has; nothing before.
fn of(prs_done: u32, prs: u32, unit: &str) -> String {
    if prs == 0 {
        String::new()
    } else {
        format!(" {prs_done} of {prs}{unit}")
    }
}

/// How long `since` is, as the sync labels say it.
pub fn ago(since: chrono::TimeDelta) -> String {
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
    /// The Pull requests view's counts: open pull requests, drafts included, but for those the
    /// GitHub sign-in can't see and Finished ones (spec §11).
    pub pull_requests: Counts,
    /// The inbox's counts: open PRs among tasks with no workspace.
    pub inbox: Counts,
    /// The workspaces that aren't archived, oldest first.
    pub workspaces: Vec<WorkspaceRow>,
    /// The archived workspaces, oldest first.
    pub archived: Vec<WorkspaceRow>,
    /// PR sync.
    pub pr_sync: PrSync,
}

impl Overview {
    /// The row of workspace `id`, archived or not.
    pub fn workspace(&self, id: WorkspaceId) -> Option<&WorkspaceRow> {
        (self.workspaces.iter())
            .chain(&self.archived)
            .find(|row| row.id == id)
    }
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
        let counts = |workspace: Option<WorkspaceId>| Counts {
            prs: self
                .tasks
                .iter()
                .filter(|task| {
                    task.kind == TaskKind::Pr
                        && task.status != TaskStatus::Done
                        && task.workspace_id == workspace
                        && !task.pr.as_ref().is_some_and(|pr| pr.unreachable)
                })
                .count(),
        };
        let row = |workspace: &Workspace| {
            let archived = workspace.archived_at.is_some();
            WorkspaceRow {
                id: workspace.id,
                name: workspace.name.clone().into(),
                dir: workspace.dir.clone().into(),
                state: AgentState::rollup(
                    self.panes
                        .iter()
                        .filter(|pane| pane.workspace == workspace.id)
                        .map(|pane| pane.state),
                ),
                counts: if archived {
                    Counts::default()
                } else {
                    counts(Some(workspace.id))
                },
                archived,
                muted: workspace.muted,
            }
        };
        let (archived, workspaces): (Vec<_>, Vec<_>) = self
            .workspaces
            .iter()
            .map(row)
            .partition(|row| row.archived);
        let pull_requests = Counts {
            prs: (self.prs.iter())
                .filter(|pr| pr.state == PrState::Open && !pr.unreachable && !pr.finished)
                .count(),
        };
        let Some(status) = &self.pr_sync else {
            return Overview {
                pull_requests,
                inbox: counts(None),
                workspaces,
                archived,
                pr_sync: PrSync::NotYet,
            };
        };
        let synced_at = status
            .last_synced_at
            .as_deref()
            .and_then(|at| DateTime::parse_from_rfc3339(at).ok());
        // A running sync shows first, so "Sync now" after logging in answers at once; then
        // a logged-out gh, the reason any sync fails until then.
        let logged_out = status.login == agentdeck_client::types::GhLogin::LoggedOut;
        let pr_sync = match (&status.progress, &status.last_error, synced_at) {
            (Some(progress), _, _) => PrSync::Syncing {
                prs_done: progress.prs_done,
                prs: progress.prs,
                details: progress.stage == agentdeck_client::types::PrSyncStage::Details,
            },
            (None, _, _) if logged_out => PrSync::LoggedOut,
            (None, Some(_), _) => PrSync::Failed,
            (None, None, Some(at)) => PrSync::SyncedAt(at.to_utc()),
            (None, None, None) => PrSync::NotYet,
        };
        Overview {
            pull_requests,
            inbox: counts(None),
            workspaces,
            archived,
            pr_sync,
        }
    }
}

/// Builders for snapshots in tests.
#[cfg(test)]
pub(crate) mod fixtures {
    use agentdeck_client::types::{
        Comment, Instruction, PrSyncStatus, PullRequest, Repository, RepositoryLinks, Task, TaskPr,
        Workspace,
    };
    use agentdeck_core::{
        AgentState, CommentAuthor, PaneId, PrRef, PrState, RepositoryId, RepositoryName, TaskId,
        TaskKind, TaskStatus, WorkspaceId,
    };

    use super::{PaneState, Snapshot};

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

    /// PR sync's status as read: gh logged in, no sync yet, the default 90-day cutoff.
    pub fn pr_status() -> PrSyncStatus {
        PrSyncStatus {
            last_error: None,
            last_synced_at: None,
            login: agentdeck_client::types::GhLogin::LoggedIn,
            max_age_days: 90,
            progress: None,
            synced_as: None,
        }
    }

    /// `snapshot`'s PR sync status, read as [`pr_status`] if it wasn't.
    pub fn pr_sync(snapshot: &mut Snapshot) -> &mut PrSyncStatus {
        snapshot.pr_sync.get_or_insert_with(pr_status)
    }

    /// A pane in `workspace`.
    pub fn pane(n: u32, workspace: WorkspaceId, state: AgentState) -> PaneState {
        PaneState {
            pane: id::<PaneId>(n),
            workspace,
            state,
            agent: None,
            task: None,
            tab: format!("tab {n}"),
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
            unseen_agent_comments: 0,
            status: TaskStatus::Todo,
            title: format!("task {n}"),
            updated_at: CREATED.into(),
            workspace_id: workspace,
        }
    }

    /// A PR task in review for PR `n` of `repo` (`owner/name`), stored as `state`.
    pub fn pr_task(n: u32, repo: &str, state: PrState, workspace: Option<WorkspaceId>) -> Task {
        Task {
            status: TaskStatus::Review,
            pr: Some(TaskPr {
                checks: None,
                draft: false,
                head_ref: "feat/x".into(),
                number: i64::from(n),
                repo: repo.into(),
                review: None,
                state,
                synced_at: CREATED.into(),
                unreachable: false,
                updated_at: CREATED.into(),
                url: format!("https://github.com/{repo}/pull/{n}"),
            }),
            ..task(n, TaskKind::Pr, workspace)
        }
    }

    /// [`pr_task`] whose open PR the GitHub sign-in can't see.
    pub fn unreachable_pr_task(n: u32, repo: &str, workspace: Option<WorkspaceId>) -> Task {
        let mut task = pr_task(n, repo, PrState::Open, workspace);
        if let Some(pr) = &mut task.pr {
            pr.unreachable = true;
        }
        task
    }

    /// The id GitHub gives Repository `repo` in these tests: the same for the same name.
    pub fn repository_id(repo: &str) -> RepositoryId {
        let hash = repo.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
        RepositoryId::from(hash & 0x7fff_ffff)
    }

    /// Open pull request `n` of `repo` (`owner/name`), titled "PR <n>", from `feat/x` into
    /// `main`, with no checks and no review.
    pub fn pull_request(n: u32, repo: &str) -> PullRequest {
        let name: RepositoryName = repo.parse().expect("owner/name");
        PullRequest {
            author: Some("octocat".into()),
            base_ref: "main".into(),
            checks: None,
            draft: false,
            head_ref: "feat/x".into(),
            number: n,
            reference: PrRef::new(name.clone(), n),
            repository: Repository {
                archived: false,
                id: repository_id(repo),
                name,
                private: false,
                url: format!("https://github.com/{repo}"),
            },
            review: None,
            state: PrState::Open,
            title: format!("PR {n}"),
            unreachable: false,
            finished: false,
            updated_at: CREATED.into(),
            url: format!("https://github.com/{repo}/pull/{n}"),
        }
    }

    /// Repository `repo` (`owner/name`), with the id [`repository_id`] gives it, linked by
    /// `workspaces`.
    pub fn links(repo: &str, workspaces: &[WorkspaceId]) -> RepositoryLinks {
        RepositoryLinks {
            archived: false,
            id: repository_id(repo),
            name: repo.parse().expect("owner/name"),
            private: false,
            unreachable: false,
            url: format!("https://github.com/{repo}"),
            workspaces: workspaces.to_vec(),
        }
    }

    /// The user's comment on `task`, seen and not yet delivered.
    pub fn comment(n: u32, task: TaskId) -> Comment {
        Comment {
            author: CommentAuthor::User,
            author_agent: None,
            author_pane: None,
            body: format!("comment {n}"),
            created_at: CREATED.into(),
            delivered_at: None,
            delivered_to_pane: None,
            id: id(n),
            seen_at: Some(CREATED.into()),
            task_id: task,
        }
    }

    /// An instruction for `workspace`.
    pub fn instruction(n: u32, workspace: WorkspaceId) -> Instruction {
        Instruction {
            body: "instruction".into(),
            created_at: CREATED.into(),
            id: id(n),
            updated_at: CREATED.into(),
            workspace_id: workspace,
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

        mod pull_requests {
            use super::*;

            #[test]
            fn should_count_open_reachable_prs_drafts_included() {
                let draft = PullRequest {
                    draft: true,
                    ..pull_request(2, "acme/widgets")
                };
                let merged = PullRequest {
                    state: PrState::Merged,
                    ..pull_request(3, "acme/widgets")
                };
                let unreachable = PullRequest {
                    unreachable: true,
                    ..pull_request(4, "acme/gadgets")
                };
                let snapshot = Snapshot {
                    prs: vec![pull_request(1, "acme/widgets"), draft, merged, unreachable],
                    ..Snapshot::default()
                };

                assert_eq!(snapshot.overview().pull_requests, Counts { prs: 2 });
            }

            #[test]
            fn should_leave_out_finished_prs_open_ones_included() {
                let stale = PullRequest {
                    finished: true,
                    ..pull_request(2, "acme/widgets")
                };
                let closed = PullRequest {
                    state: PrState::Closed,
                    finished: true,
                    ..pull_request(3, "acme/widgets")
                };
                let snapshot = Snapshot {
                    prs: vec![pull_request(1, "acme/widgets"), stale, closed],
                    ..Snapshot::default()
                };

                assert_eq!(snapshot.overview().pull_requests, Counts { prs: 1 });
            }
        }

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

            #[test]
            fn should_list_archived_workspaces_apart_oldest_first_with_no_counts() {
                let archived = |n, name| Workspace {
                    archived_at: Some("2026-11-12T01:00:00Z".into()),
                    ..workspace(n, name)
                };
                let old = archived(2, "old");
                let snapshot = Snapshot {
                    tasks: vec![task(30, TaskKind::Pr, Some(old.id))],
                    workspaces: vec![workspace(1, "a"), old, archived(3, "older")],
                    ..Snapshot::default()
                };

                let overview = snapshot.overview();
                let rows: Vec<_> = (overview.archived.iter())
                    .map(|row| (row.name.to_string(), row.archived, row.counts))
                    .collect();

                assert_eq!(
                    rows,
                    [
                        ("old".into(), true, Counts::default()),
                        ("older".into(), true, Counts::default())
                    ]
                );
                assert!(!overview.workspaces[0].archived);
            }

            #[test]
            fn should_find_a_workspace_archived_or_not() {
                let snapshot = Snapshot {
                    workspaces: vec![
                        workspace(1, "a"),
                        Workspace {
                            archived_at: Some("2026-11-12T01:00:00Z".into()),
                            ..workspace(2, "old")
                        },
                    ],
                    ..Snapshot::default()
                };
                let overview = snapshot.overview();

                let name = |n| overview.workspace(id(n)).map(|row| row.name.to_string());

                assert_eq!(name(1).as_deref(), Some("a"));
                assert_eq!(name(2).as_deref(), Some("old"));
                assert_eq!(name(3), None);
            }
        }

        mod counts {
            use super::*;

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
                    tasks: vec![inbox_pr, task(21, TaskKind::Pr, Some(a.id))],
                    workspaces: vec![a],
                    ..Snapshot::default()
                };

                assert_eq!(snapshot.overview().inbox, Counts { prs: 1 });
            }

            #[test]
            fn should_leave_out_prs_the_sign_in_cannot_see() {
                let a = workspace(1, "a");
                let snapshot = Snapshot {
                    tasks: vec![
                        task(20, TaskKind::Pr, None),
                        unreachable_pr_task(21, "acme/secret", None),
                        task(22, TaskKind::Pr, Some(a.id)),
                        unreachable_pr_task(23, "acme/secret", Some(a.id)),
                    ],
                    workspaces: vec![a],
                    ..Snapshot::default()
                };
                let overview = snapshot.overview();

                assert_eq!(overview.inbox, Counts { prs: 1 });
                assert_eq!(overview.workspaces[0].counts, Counts { prs: 1 });
            }
        }

        mod pr_sync {
            use super::*;

            #[test]
            fn should_report_the_last_sync() {
                let mut snapshot = Snapshot::default();
                pr_sync(&mut snapshot).last_synced_at = Some("2026-11-12T10:00:00Z".into());

                assert_eq!(
                    snapshot.overview().pr_sync,
                    PrSync::SyncedAt(at("2026-11-12T10:00:00Z"))
                );
            }

            #[test]
            fn should_report_a_failure_over_an_earlier_sync() {
                let mut snapshot = Snapshot::default();
                pr_sync(&mut snapshot).last_synced_at = Some("2026-11-12T10:00:00Z".into());
                pr_sync(&mut snapshot).last_error = Some("gh: rate limited".into());

                assert_eq!(snapshot.overview().pr_sync, PrSync::Failed);
            }

            #[test]
            fn should_report_a_logged_out_gh_over_a_failure_and_an_earlier_sync() {
                let mut snapshot = Snapshot::default();
                pr_sync(&mut snapshot).last_synced_at = Some("2026-11-12T10:00:00Z".into());
                pr_sync(&mut snapshot).last_error = Some("gh is not logged in".into());
                pr_sync(&mut snapshot).login = agentdeck_client::types::GhLogin::LoggedOut;

                assert_eq!(snapshot.overview().pr_sync, PrSync::LoggedOut);
            }

            #[test]
            fn should_report_a_running_sync_over_a_logged_out_gh() {
                let mut snapshot = Snapshot::default();
                pr_sync(&mut snapshot).login = agentdeck_client::types::GhLogin::LoggedOut;
                pr_sync(&mut snapshot).progress = Some(agentdeck_client::types::PrSyncProgress {
                    prs_done: 0,
                    prs: 0,
                    stage: agentdeck_client::types::PrSyncStage::Listing,
                });

                assert!(matches!(
                    snapshot.overview().pr_sync,
                    PrSync::Syncing { .. }
                ));
            }

            #[test]
            fn should_report_none_before_the_first_sync() {
                assert_eq!(Snapshot::default().overview().pr_sync, PrSync::NotYet);
            }

            #[test]
            fn should_report_a_running_sync_over_the_last_one() {
                let mut snapshot = Snapshot::default();
                pr_sync(&mut snapshot).last_synced_at = Some("2026-11-12T10:00:00Z".into());
                pr_sync(&mut snapshot).last_error = Some("gh: rate limited".into());
                pr_sync(&mut snapshot).progress = Some(agentdeck_client::types::PrSyncProgress {
                    prs_done: 100,
                    prs: 271,
                    stage: agentdeck_client::types::PrSyncStage::Listing,
                });

                assert_eq!(
                    snapshot.overview().pr_sync,
                    PrSync::Syncing {
                        prs_done: 100,
                        prs: 271,
                        details: false,
                    }
                );
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
            assert_eq!(synced("2026-11-12T11:59:30Z"), "Synced just now");
        }

        #[test]
        fn should_count_minutes_within_an_hour() {
            assert_eq!(synced("2026-11-12T11:59:00Z"), "Synced 1 min ago");
            assert_eq!(synced("2026-11-12T11:01:00Z"), "Synced 59 min ago");
        }

        #[test]
        fn should_count_hours_within_a_day() {
            assert_eq!(synced("2026-11-12T09:30:00Z"), "Synced 2 h ago");
        }

        #[test]
        fn should_count_days_after_that() {
            assert_eq!(synced("2026-11-10T12:00:00Z"), "Synced 2 d ago");
        }

        #[test]
        fn should_say_just_now_for_a_sync_stamped_after_now() {
            assert_eq!(synced("2026-11-12T12:00:05Z"), "Synced just now");
        }

        #[test]
        fn should_say_when_no_sync_has_finished_or_the_last_failed() {
            assert_eq!(PrSync::NotYet.label(at(NOW)), "Not synced yet");
            assert_eq!(PrSync::Failed.label(at(NOW)), "Sync failed");
        }

        #[test]
        fn should_say_not_signed_in() {
            assert_eq!(PrSync::LoggedOut.label(at(NOW)), "Not signed in");
        }

        #[test]
        fn should_say_a_sync_is_fetching_details_without_a_count() {
            let details = PrSync::Syncing {
                prs_done: 271,
                prs: 271,
                details: true,
            };

            assert_eq!(details.label(at(NOW)), "Syncing details…");
        }

        #[test]
        fn should_say_how_far_a_running_sync_is_once_it_knows() {
            let syncing = |prs_done, prs| PrSync::Syncing {
                prs_done,
                prs,
                details: false,
            };

            assert_eq!(syncing(100, 271).label(at(NOW)), "Syncing… 100 of 271 PRs");
            assert_eq!(syncing(0, 0).label(at(NOW)), "Syncing…");
        }
    }
}
