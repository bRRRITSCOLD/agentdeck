//! The rows the Tasks view, the Inbox and the right panel list (spec §11), from the sidebar's
//! [`Snapshot`]: a pure function of it, which the views render.
//!
//! A row carries what its cells show: the task's status, kind and title, the state of the
//! agent panes that link it, its PR (repository, number, state, draft, checks, review) and its
//! workspace.

use agentdeck_client::types::Task;
use agentdeck_core::{
    AgentState, ChecksState, PrState, ReviewState, TaskId, TaskKind, TaskStatus, WorkspaceId,
};
use gpui::SharedString;

use crate::overview::Snapshot;

/// The statuses in the order the Tasks view groups them, as Penpot's "Tasks" board does.
pub const GROUP_ORDER: [TaskStatus; 4] = [
    TaskStatus::Doing,
    TaskStatus::Review,
    TaskStatus::Todo,
    TaskStatus::Done,
];

/// The agent panes linking a task, summed up for its row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCell {
    /// Their states, rolled up as a workspace's are.
    pub state: AgentState,
    /// The agent the first of them runs, as the daemon names it (`claude`).
    pub name: SharedString,
}

/// A PR task's PR, for its row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrCell {
    /// The repository's name without its owner: `ts-packages`.
    pub repo: SharedString,
    /// The repository's name without its owner, then the number: `ts-packages #142`.
    pub reference: SharedString,
    /// Open, merged or closed: with `draft`, the row's icon.
    pub state: PrState,
    /// Whether it is a draft.
    pub draft: bool,
    /// Its checks, once known.
    pub checks: Option<ChecksState>,
    /// Its review, once known.
    pub review: Option<ReviewState>,
    /// Its GitHub page, as stored: what Open on GitHub and the task sheet's link open.
    pub url: SharedString,
}

/// One task's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The task.
    pub id: TaskId,
    /// Its status, which the row's picker shows.
    pub status: TaskStatus,
    /// Plain or PR: the row's icon.
    pub kind: TaskKind,
    /// Its title.
    pub title: SharedString,
    /// The agents working on it, if any pane links it.
    pub agent: Option<AgentCell>,
    /// Its PR, if it is a PR task the sync has filled in.
    pub pr: Option<PrCell>,
    /// Its workspace's name; `None` in the inbox.
    pub workspace: Option<SharedString>,
    /// Whether an agent commented on it since the user last opened it: the row's unseen mark.
    pub unseen: bool,
    /// Whether its PR can be closed from here: a PR task whose PR is stored open, aged out or
    /// not (spec §11, Closing PRs). Only such a row is selectable, and only its menu holds
    /// Close PR.
    pub closable: bool,
}

/// The rows of one status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The status.
    pub status: TaskStatus,
    /// Its rows, in the daemon's order.
    pub rows: Vec<Row>,
}

/// Which tasks a list holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every task: the Tasks view.
    All,
    /// Tasks with no workspace: the Inbox.
    Inbox,
    /// One workspace's tasks: the right panel.
    Workspace(WorkspaceId),
}

/// What a list shows of the snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query<'a> {
    /// Which tasks.
    pub scope: Scope,
    /// Text a row's title, PR reference or workspace must contain, ignoring case; empty
    /// matches every row.
    pub filter: &'a str,
    /// Whether done tasks show.
    pub show_done: bool,
}

/// Each status's rows that `query` picks, in [`GROUP_ORDER`]; a status with none is left out.
pub fn groups(snapshot: &Snapshot, query: &Query) -> Vec<Group> {
    let rows = rows(snapshot, query);
    GROUP_ORDER
        .into_iter()
        .map(|status| Group {
            status,
            rows: rows
                .iter()
                .filter(|r| r.status == status)
                .cloned()
                .collect(),
        })
        .filter(|group| !group.rows.is_empty())
        .collect()
}

/// The rows `query` picks, in the daemon's order, ungrouped.
pub fn rows(snapshot: &Snapshot, query: &Query) -> Vec<Row> {
    let in_scope = |task: &&Task| match query.scope {
        Scope::All => true,
        Scope::Inbox => task.workspace_id.is_none(),
        Scope::Workspace(ws) => task.workspace_id == Some(ws),
    };
    let filter = query.filter.trim().to_lowercase();
    let matches = |row: &Row| {
        filter.is_empty()
            || [
                Some(&row.title),
                row.pr.as_ref().map(|pr| &pr.reference),
                row.workspace.as_ref(),
            ]
            .into_iter()
            .flatten()
            .any(|text| text.to_lowercase().contains(&filter))
    };
    snapshot
        .tasks
        .iter()
        .filter(in_scope)
        .filter(|task| query.show_done || task.status != TaskStatus::Done)
        .map(|task| row(snapshot, task))
        .filter(matches)
        .collect()
}

/// The row for `task`.
fn row(snapshot: &Snapshot, task: &Task) -> Row {
    let linked: Vec<_> = (snapshot.panes.iter())
        .filter(|pane| pane.task == Some(task.id))
        .collect();
    let agent = linked.first().map(|first| AgentCell {
        state: AgentState::rollup(linked.iter().map(|pane| pane.state)),
        name: first.agent.map_or("shell", |agent| agent.as_str()).into(),
    });
    let pr = task.pr.as_ref().map(|pr| {
        let repo = pr.repo.rsplit('/').next().unwrap_or(&pr.repo);
        PrCell {
            repo: repo.to_owned().into(),
            reference: format!("{repo} #{}", pr.number).into(),
            state: pr.state,
            draft: pr.draft,
            checks: pr.checks,
            review: pr.review,
            url: pr.url.clone().into(),
        }
    });
    let workspace = task.workspace_id.and_then(|ws| {
        (snapshot.workspaces.iter())
            .find(|w| w.id == ws)
            .map(|w| SharedString::from(w.name.clone()))
    });
    Row {
        id: task.id,
        status: task.status,
        kind: task.kind,
        title: task.title.clone().into(),
        agent,
        pr,
        workspace,
        unseen: task.unseen_agent_comments > 0,
        closable: task.pr.as_ref().is_some_and(|pr| pr.state == PrState::Open),
    }
}

#[cfg(test)]
mod tests {
    use agentdeck_client::types::PullRequest;
    use agentdeck_core::Agent;

    use super::*;
    use crate::overview::fixtures::{id, pane, task, workspace};

    fn all() -> Query<'static> {
        Query {
            scope: Scope::All,
            filter: "",
            show_done: false,
        }
    }

    fn with_status(n: u32, status: TaskStatus, ws: Option<WorkspaceId>) -> Task {
        Task {
            status,
            ..task(n, TaskKind::Plain, ws)
        }
    }

    fn pr(repo: &str, number: i64) -> PullRequest {
        PullRequest {
            checks: Some(ChecksState::Failure),
            draft: true,
            head_ref: "feat/x".into(),
            number,
            repo: repo.into(),
            review: Some(ReviewState::ChangesRequested),
            state: PrState::Open,
            synced_at: "2026-11-13T00:00:00Z".into(),
            updated_at: "2026-11-13T00:00:00Z".into(),
            url: format!("https://github.com/{repo}/pull/{number}"),
        }
    }

    fn titles(groups: &[Group]) -> Vec<(TaskStatus, Vec<SharedString>)> {
        groups
            .iter()
            .map(|g| (g.status, g.rows.iter().map(|r| r.title.clone()).collect()))
            .collect()
    }

    mod groups {
        use super::*;

        #[test]
        fn should_group_doing_then_review_then_todo_leaving_out_empty_ones() {
            let snapshot = Snapshot {
                tasks: vec![
                    with_status(1, TaskStatus::Todo, None),
                    with_status(2, TaskStatus::Doing, None),
                    with_status(3, TaskStatus::Todo, None),
                ],
                ..Snapshot::default()
            };

            assert_eq!(
                titles(&groups(&snapshot, &all())),
                [
                    (TaskStatus::Doing, vec!["task 2".into()]),
                    (TaskStatus::Todo, vec!["task 1".into(), "task 3".into()]),
                ]
            );
        }

        #[test]
        fn should_hide_done_tasks_unless_asked() {
            let snapshot = Snapshot {
                tasks: vec![with_status(1, TaskStatus::Done, None)],
                ..Snapshot::default()
            };
            let show_done = Query {
                show_done: true,
                ..all()
            };

            assert_eq!(groups(&snapshot, &all()), []);
            assert_eq!(
                titles(&groups(&snapshot, &show_done)),
                [(TaskStatus::Done, vec!["task 1".into()])]
            );
        }

        #[test]
        fn should_filter_by_title_pr_or_workspace_ignoring_case() {
            let ws = workspace(1, "gpui-terminal");
            let snapshot = Snapshot {
                tasks: vec![
                    Task {
                        title: "Fix flaky Tabs test".into(),
                        ..task(1, TaskKind::Plain, None)
                    },
                    Task {
                        pr: Some(pr("bRRRITSCOLD/ts-packages", 142)),
                        ..task(2, TaskKind::Pr, None)
                    },
                    task(3, TaskKind::Plain, Some(ws.id)),
                    task(4, TaskKind::Plain, None),
                ],
                workspaces: vec![ws],
                ..Snapshot::default()
            };
            let filtered = |filter| {
                let query = Query { filter, ..all() };
                rows(&snapshot, &query)
                    .into_iter()
                    .map(|r| r.id)
                    .collect::<Vec<TaskId>>()
            };

            assert_eq!(filtered("tabs"), [id(1)]);
            assert_eq!(filtered("TS-PACKAGES #142"), [id(2)]);
            assert_eq!(filtered("gpui"), [id(3)]);
        }
    }

    mod scope {
        use super::*;

        #[test]
        fn should_keep_the_inbox_to_tasks_with_no_workspace() {
            let ws = workspace(1, "a");
            let snapshot = Snapshot {
                tasks: vec![
                    task(1, TaskKind::Plain, Some(ws.id)),
                    task(2, TaskKind::Pr, None),
                ],
                workspaces: vec![ws],
                ..Snapshot::default()
            };
            let query = Query {
                scope: Scope::Inbox,
                ..all()
            };

            let ids: Vec<TaskId> = rows(&snapshot, &query).into_iter().map(|r| r.id).collect();
            assert_eq!(ids, [id(2)]);
        }
    }

    mod workspace_scope {
        use super::*;

        #[test]
        fn should_keep_a_workspaces_list_to_its_tasks() {
            let (a, b) = (workspace(1, "a"), workspace(2, "b"));
            let snapshot = Snapshot {
                tasks: vec![
                    task(1, TaskKind::Plain, Some(a.id)),
                    task(2, TaskKind::Plain, Some(b.id)),
                    task(3, TaskKind::Plain, None),
                ],
                workspaces: vec![a.clone(), b],
                ..Snapshot::default()
            };
            let query = Query {
                scope: Scope::Workspace(a.id),
                ..all()
            };

            let ids: Vec<TaskId> = rows(&snapshot, &query).into_iter().map(|r| r.id).collect();
            assert_eq!(ids, [id(1)]);
        }
    }

    mod row {
        use super::*;

        #[test]
        fn should_flag_a_task_with_agent_comments_unseen() {
            let snapshot = Snapshot {
                tasks: vec![
                    Task {
                        unseen_agent_comments: 2,
                        ..task(1, TaskKind::Plain, None)
                    },
                    task(2, TaskKind::Plain, None),
                ],
                ..Snapshot::default()
            };

            let unseen: Vec<bool> = rows(&snapshot, &all()).iter().map(|r| r.unseen).collect();
            assert_eq!(unseen, [true, false]);
        }

        #[test]
        fn should_name_the_pr_by_repository_and_number_and_keep_its_url() {
            let snapshot = Snapshot {
                tasks: vec![Task {
                    pr: Some(pr("bRRRITSCOLD/rust-packages", 61)),
                    ..task(1, TaskKind::Pr, None)
                }],
                ..Snapshot::default()
            };

            let row = &rows(&snapshot, &all())[0];
            assert_eq!(
                row.pr,
                Some(PrCell {
                    repo: "rust-packages".into(),
                    reference: "rust-packages #61".into(),
                    state: PrState::Open,
                    draft: true,
                    checks: Some(ChecksState::Failure),
                    review: Some(ReviewState::ChangesRequested),
                    url: "https://github.com/bRRRITSCOLD/rust-packages/pull/61".into(),
                })
            );
        }

        #[test]
        fn should_keep_the_prs_state() {
            let snapshot = Snapshot {
                tasks: [PrState::Open, PrState::Merged, PrState::Closed]
                    .into_iter()
                    .zip(1..)
                    .map(|(state, n)| Task {
                        pr: Some(PullRequest {
                            state,
                            ..pr("acme/widgets", i64::from(n))
                        }),
                        ..task(n, TaskKind::Pr, None)
                    })
                    .collect(),
                ..Snapshot::default()
            };

            let states: Vec<_> = (rows(&snapshot, &all()).iter())
                .map(|r| r.pr.as_ref().map(|pr| pr.state))
                .collect();
            assert_eq!(
                states,
                [
                    Some(PrState::Open),
                    Some(PrState::Merged),
                    Some(PrState::Closed)
                ]
            );
        }

        #[test]
        fn should_make_only_an_open_pr_closable() {
            let with_state = |n: u32, state: PrState| Task {
                pr: Some(PullRequest {
                    state,
                    ..pr("acme/widgets", i64::from(n))
                }),
                ..task(n, TaskKind::Pr, None)
            };
            let snapshot = Snapshot {
                tasks: vec![
                    with_state(1, PrState::Open),
                    with_state(2, PrState::Merged),
                    with_state(3, PrState::Closed),
                    task(4, TaskKind::Plain, None),
                ],
                ..Snapshot::default()
            };

            let closable: Vec<bool> = rows(&snapshot, &all()).iter().map(|r| r.closable).collect();
            assert_eq!(closable, [true, false, false, false]);
        }

        #[test]
        fn should_name_the_workspace() {
            let ws = workspace(1, "tabs-a11y");
            let snapshot = Snapshot {
                tasks: vec![task(1, TaskKind::Plain, Some(ws.id))],
                workspaces: vec![ws],
                ..Snapshot::default()
            };

            assert_eq!(
                rows(&snapshot, &all())[0].workspace,
                Some("tabs-a11y".into())
            );
        }

        #[test]
        fn should_roll_up_the_linked_agent_panes() {
            let ws = workspace(1, "a");
            let linked = |n, state| crate::overview::PaneState {
                agent: Some(Agent::Claude),
                task: Some(id(1)),
                ..pane(n, ws.id, state)
            };
            let snapshot = Snapshot {
                tasks: vec![
                    task(1, TaskKind::Plain, Some(ws.id)),
                    task(2, TaskKind::Plain, None),
                ],
                panes: vec![
                    linked(10, AgentState::Working),
                    linked(11, AgentState::Blocked),
                    pane(12, ws.id, AgentState::Done),
                ],
                workspaces: vec![ws],
                ..Snapshot::default()
            };

            let rows = rows(&snapshot, &all());
            assert_eq!(
                rows[0].agent,
                Some(AgentCell {
                    state: AgentState::Blocked,
                    name: "claude".into()
                })
            );
            assert_eq!(rows[1].agent, None);
        }
    }
}
