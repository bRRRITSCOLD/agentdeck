//! What the sidebar shows (spec §11): the Pull requests view's count, every workspace with its
//! rolled-up state, the archived ones apart, and where PR sync stands for the Pull requests
//! view's header.
//!
//! A [`Snapshot`] is the daemon's data as read over REST; [`Snapshot::overview`] sums it up,
//! a pure function the views render. Live `state` and `exit` messages patch the snapshot's
//! pane states in place; anything else that changes is read again.

use std::collections::HashMap;

use agentdeck_client::types::{PrSyncStatus, PullRequest, Repository, RepositoryLinks, Workspace};
use agentdeck_core::{Agent, AgentState, PaneId, PrState, RepositoryId, WorkspaceId};
use chrono::{DateTime, Utc};
use gpui::SharedString;

/// One pane's state, and the workspace and tab that hold it.
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
    /// PR sync's last run and its settings; `None` until read.
    pub pr_sync: Option<PrSyncStatus>,
    /// Every pull request PR sync keeps, by Repository name, then number (spec §4).
    pub prs: Vec<PullRequest>,
    /// Every Repository the daemon keeps, with the Workspaces linking it, archived ones
    /// included: what the PR alert mute rule reads (spec §11).
    pub repositories: Vec<RepositoryLinks>,
    /// Each unarchived Workspace's Suggestions, by name, as the daemon works them out: not
    /// filtered by what was dismissed (spec §11).
    pub suggestions: HashMap<WorkspaceId, Vec<Repository>>,
}

/// A row's counts; a zero hides its count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Open pull requests, drafts included, but for those the GitHub sign-in can't see and
    /// Finished ones.
    pub prs: usize,
}

/// A workspace's sidebar row.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceRow {
    /// The workspace.
    pub id: WorkspaceId,
    /// Its name.
    pub name: SharedString,
    /// Its panes' states, rolled up.
    pub state: AgentState,
    /// Whether it is archived.
    pub archived: bool,
    /// Whether its notifications are muted.
    pub muted: bool,
    /// Its counts: open pull requests in the Repositories it links, as the Pull requests
    /// view counts them; none while archived, since an archived Workspace links nothing.
    pub counts: Counts,
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

    /// The Workspaces linking `repository`, as the views count them (spec §11): Archived ones
    /// link nothing, so they are left out. In the sidebar's order, oldest first. None means the
    /// Repository is Not in a workspace.
    pub fn linking(&self, repository: RepositoryId) -> Vec<&Workspace> {
        let Some(links) = (self.repositories.iter()).find(|links| links.id == repository) else {
            return Vec::new();
        };
        (self.workspaces.iter())
            .filter(|w| w.archived_at.is_none() && links.workspaces.contains(&w.id))
            .collect()
    }

    /// `workspace`'s Suggestions, by name, but for any it links by now: the daemon's next
    /// word on them may not be read yet. None while it is archived.
    pub fn suggested(&self, workspace: WorkspaceId) -> Vec<&Repository> {
        let archived =
            (self.workspaces.iter()).any(|w| w.id == workspace && w.archived_at.is_some());
        if archived {
            return Vec::new();
        }
        (self.suggestions.get(&workspace).into_iter().flatten())
            .filter(|r| !self.links(workspace, r.id))
            .collect()
    }

    /// Whether `workspace` links `repository` and isn't archived.
    pub fn links(&self, workspace: WorkspaceId, repository: RepositoryId) -> bool {
        self.linking(repository).iter().any(|w| w.id == workspace)
    }

    /// The sidebar's content.
    pub fn overview(&self) -> Overview {
        let counted =
            |pr: &&PullRequest| pr.state == PrState::Open && !pr.unreachable && !pr.finished;
        let row = |workspace: &Workspace| {
            let archived = workspace.archived_at.is_some();
            let prs = match archived {
                true => 0,
                false => (self.prs.iter())
                    .filter(counted)
                    .filter(|pr| self.links(workspace.id, pr.repository.id))
                    .count(),
            };
            WorkspaceRow {
                id: workspace.id,
                name: workspace.name.clone().into(),
                state: AgentState::rollup(
                    self.panes
                        .iter()
                        .filter(|pane| pane.workspace == workspace.id)
                        .map(|pane| pane.state),
                ),
                archived,
                muted: workspace.muted,
                counts: Counts { prs },
            }
        };
        let (archived, workspaces): (Vec<_>, Vec<_>) = self
            .workspaces
            .iter()
            .map(row)
            .partition(|row| row.archived);
        let pull_requests = Counts {
            prs: self.prs.iter().filter(counted).count(),
        };
        let Some(status) = &self.pr_sync else {
            return Overview {
                pull_requests,
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
        PrSyncStatus, PullRequest, Repository, RepositoryLinks, Workspace,
    };
    use agentdeck_core::{
        AgentState, PaneId, PrRef, PrState, RepositoryId, RepositoryName, WorkspaceId,
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
            tab: format!("tab {n}"),
        }
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

    /// Repository `repo` (`owner/name`) as a Suggestion carries it, public, with the id
    /// [`repository_id`] gives it.
    pub fn suggestion(repo: &str) -> Repository {
        Repository {
            archived: false,
            id: repository_id(repo),
            name: repo.parse().expect("owner/name"),
            private: false,
            url: format!("https://github.com/{repo}"),
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
}

#[cfg(test)]
mod tests {
    use agentdeck_core::AgentState::*;

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
            fn should_list_archived_workspaces_apart_oldest_first() {
                let archived = |n, name| Workspace {
                    archived_at: Some("2026-11-12T01:00:00Z".into()),
                    ..workspace(n, name)
                };
                let snapshot = Snapshot {
                    workspaces: vec![workspace(1, "a"), archived(2, "old"), archived(3, "older")],
                    ..Snapshot::default()
                };

                let overview = snapshot.overview();
                let rows: Vec<_> = (overview.archived.iter())
                    .map(|row| (row.name.to_string(), row.archived))
                    .collect();

                assert_eq!(rows, [("old".into(), true), ("older".into(), true)]);
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

        mod workspace_counts {
            use super::*;

            fn archived(n: u32, name: &str) -> Workspace {
                Workspace {
                    archived_at: Some("2026-11-12T01:00:00Z".into()),
                    ..workspace(n, name)
                }
            }

            /// `a` links widgets and gadgets, `b` links widgets, `old` (archived) links gizmos.
            fn linked() -> Snapshot {
                Snapshot {
                    workspaces: vec![workspace(1, "a"), workspace(2, "b"), archived(3, "old")],
                    repositories: vec![
                        links("acme/widgets", &[id(1), id(2)]),
                        links("acme/gadgets", &[id(1)]),
                        links("acme/gizmos", &[id(3)]),
                    ],
                    ..Snapshot::default()
                }
            }

            fn counts(snapshot: &Snapshot) -> Vec<(String, usize)> {
                let overview = snapshot.overview();
                (overview.workspaces.iter())
                    .chain(&overview.archived)
                    .map(|row| (row.name.to_string(), row.counts.prs))
                    .collect()
            }

            #[test]
            fn should_count_the_open_prs_in_each_workspaces_linked_repositories() {
                let snapshot = Snapshot {
                    prs: vec![
                        pull_request(1, "acme/widgets"),
                        pull_request(2, "acme/widgets"),
                        pull_request(3, "acme/gadgets"),
                        pull_request(4, "acme/elsewhere"),
                    ],
                    ..linked()
                };

                assert_eq!(
                    counts(&snapshot),
                    [("a".into(), 3), ("b".into(), 2), ("old".into(), 0)]
                );
            }

            #[test]
            fn should_leave_out_finished_unreachable_and_closed_prs() {
                let snapshot = Snapshot {
                    prs: vec![
                        PullRequest {
                            draft: true,
                            ..pull_request(1, "acme/gadgets")
                        },
                        PullRequest {
                            finished: true,
                            ..pull_request(2, "acme/gadgets")
                        },
                        PullRequest {
                            unreachable: true,
                            ..pull_request(3, "acme/gadgets")
                        },
                        PullRequest {
                            state: PrState::Merged,
                            finished: true,
                            ..pull_request(4, "acme/gadgets")
                        },
                    ],
                    ..linked()
                };

                assert_eq!(counts(&snapshot)[0], ("a".into(), 1));
            }

            #[test]
            fn should_count_nothing_for_an_archived_workspace() {
                let snapshot = Snapshot {
                    prs: vec![pull_request(1, "acme/gizmos")],
                    ..linked()
                };

                assert_eq!(counts(&snapshot)[2], ("old".into(), 0));
            }
        }

        mod linking {
            use super::*;

            #[test]
            fn should_list_the_workspaces_linking_a_repository_in_the_sidebars_order() {
                let snapshot = Snapshot {
                    workspaces: vec![workspace(1, "a"), workspace(2, "b")],
                    repositories: vec![links("acme/widgets", &[id(2), id(1)])],
                    ..Snapshot::default()
                };

                let names: Vec<_> = (snapshot.linking(repository_id("acme/widgets")).iter())
                    .map(|w| w.name.clone())
                    .collect();

                assert_eq!(names, ["a", "b"]);
            }

            #[test]
            fn should_count_an_archived_workspace_as_linking_nothing() {
                let snapshot = Snapshot {
                    workspaces: vec![
                        workspace(1, "a"),
                        Workspace {
                            archived_at: Some("2026-11-12T01:00:00Z".into()),
                            ..workspace(2, "old")
                        },
                    ],
                    repositories: vec![
                        links("acme/widgets", &[id(1), id(2)]),
                        links("acme/gizmos", &[id(2)]),
                    ],
                    ..Snapshot::default()
                };

                let names = |repo| -> Vec<String> {
                    (snapshot.linking(repository_id(repo)).iter())
                        .map(|w| w.name.clone())
                        .collect()
                };
                assert_eq!(names("acme/widgets"), ["a"]);
                assert!(names("acme/gizmos").is_empty());
                assert!(names("acme/unknown").is_empty());
                assert!(!snapshot.links(id(2), repository_id("acme/widgets")));
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
