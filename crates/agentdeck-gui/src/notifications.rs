//! Desktop notifications (spec §11): what changed between two reads of the daemon that the
//! user should hear about, as [`Alert`]s.
//!
//! - **Panes:** an agent pane turning `blocked` or `done`, once per change. Whether its tab is
//!   in view is the window's call, so [`pane_alert`] doesn't know. [`Reattached`] leaves out a
//!   pane settling back, just after it reattached, into the state it held then.
//! - **PRs:** a PR task's checks turning `failure`, or a review arriving (`approved`,
//!   `changes_requested`). [`PrBatch`] holds them while a sync runs, and lets them out as one
//!   alert per change once it ends, or as one "N PRs changed" when more than 3 PRs changed.
//!
//! A muted workspace's panes and PR tasks never alert; an Inbox task has no workspace to mute.
//! Only what both reads hold can change, so the first read alerts nothing.
//!
//! The shell shows them through an `agentdeck_notify::Notifier`: the desktop's notification
//! server over one shared D-Bus connection, or, in the unit tier, `fake::FakeNotifier`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use agentdeck_client::types::Task;
use agentdeck_core::{AgentState, ChecksState, PaneId, ReviewState, TaskId, WorkspaceId};
use agentdeck_notify::Notification;
use gpui::SharedString;

use crate::overview::{PaneState, Snapshot};
use crate::task_sheet::agent_name;

/// More PRs than this changing in one sync make one alert.
const COALESCE_OVER: usize = 3;

/// How long after a pane reattaches its agent settling back into the state it held then is
/// no news.
pub const REATTACH_QUIET: Duration = Duration::from_secs(10);

/// What clicking a notification shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The pane, in its tab and workspace.
    Pane {
        /// The workspace it sits in.
        workspace: WorkspaceId,
        /// The pane.
        pane: PaneId,
    },
    /// The task sheet on a task.
    Task(TaskId),
    /// The Tasks view, for several PRs at once.
    Tasks,
}

/// A notification to show, and what clicking it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    /// Names what it is about, so a click finds its [`Target`] again.
    pub tag: SharedString,
    /// The headline.
    pub title: SharedString,
    /// The line below it.
    pub body: SharedString,
    /// What a click shows.
    pub target: Target,
}

impl Alert {
    /// The OS notification: no buttons, since a click on its body is the one action.
    pub fn notification(&self) -> Notification {
        Notification {
            tag: self.tag.clone(),
            title: self.title.clone(),
            body: self.body.clone(),
        }
    }
}

/// Whether `workspace`'s notifications are muted; the Inbox (`None`) never is.
fn muted(snapshot: &Snapshot, workspace: Option<WorkspaceId>) -> bool {
    workspace.is_some_and(|id| (snapshot.workspaces.iter()).any(|w| w.id == id && w.muted))
}

/// The alert for `pane`, now as `snapshot` holds it, having been `before`: "Claude Code is
/// blocked" over "\<task or tab> · \<workspace>", when an agent pane turned `blocked` or `done`
/// and its workspace isn't muted.
pub fn pane_alert(before: AgentState, pane: &PaneState, snapshot: &Snapshot) -> Option<Alert> {
    let verb = match pane.state {
        AgentState::Blocked => "blocked",
        AgentState::Done => "done",
        _ => return None,
    };
    let agent = pane.agent?;
    if before == pane.state || muted(snapshot, Some(pane.workspace)) {
        return None;
    }
    let workspace = (snapshot.workspaces.iter()).find(|w| w.id == pane.workspace)?;
    let task = pane
        .task
        .and_then(|task| snapshot.tasks.iter().find(|t| t.id == task));
    let what = task.map_or(pane.tab.as_str(), |task| task.title.as_str());
    Some(Alert {
        tag: format!("pane:{}", pane.pane).into(),
        title: format!("{} is {verb}", agent_name(agent)).into(),
        body: format!("{what} · {}", workspace.name).into(),
        target: Target::Pane {
            workspace: pane.workspace,
            pane: pane.pane,
        },
    })
}

/// The alerts for the panes both reads hold whose state changed.
pub fn pane_alerts(before: &Snapshot, after: &Snapshot) -> Vec<Alert> {
    (after.panes.iter())
        .filter_map(|pane| {
            let was = before.panes.iter().find(|p| p.pane == pane.pane)?;
            pane_alert(was.state, pane, after)
        })
        .collect()
}

/// The panes reattached in the last [`REATTACH_QUIET`], and the state each held then.
///
/// Attaching resizes a pane to its view, and an agent redraws on a resize: Claude Code done
/// reads working, then done again. That is the state the user already heard about, from this
/// GUI or the one before a restart, so the pane's alert for it is left out.
#[derive(Debug, Default)]
pub struct Reattached {
    panes: HashMap<PaneId, (AgentState, Instant)>,
}

impl Reattached {
    /// `pane` attached at `now`, holding `state`.
    pub fn attached(&mut self, pane: PaneId, state: AgentState, now: Instant) {
        self.panes.insert(pane, (state, now));
    }

    /// Leaves out of `alerts` each pane's whose state in `snapshot` is the one it held when it
    /// reattached, under [`REATTACH_QUIET`] before `now`. Each left out is logged at debug.
    pub fn retain(&mut self, alerts: &mut Vec<Alert>, snapshot: &Snapshot, now: Instant) {
        self.panes
            .retain(|_, (_, at)| now.saturating_duration_since(*at) < REATTACH_QUIET);
        alerts.retain(|alert| {
            let Target::Pane { pane, .. } = alert.target else {
                return true;
            };
            let state = (snapshot.panes.iter()).find(|p| p.pane == pane).map(|p| p.state);
            let echo = state.is_some() && self.panes.get(&pane).map(|(held, _)| *held) == state;
            if echo {
                tracing::debug!(tag = %alert.tag, "leaving out a desktop alert: its pane settled back after a reattach");
            }
            !echo
        });
    }
}

/// What happened to a PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrEvent {
    ChecksFailed,
    Approved,
    ChangesRequested,
}

impl PrEvent {
    fn title(self) -> &'static str {
        match self {
            PrEvent::ChecksFailed => "Checks failed",
            PrEvent::Approved => "Approved",
            PrEvent::ChangesRequested => "Changes requested",
        }
    }
}

/// One PR task's change, waiting for its sync to end.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PrChange {
    task: TaskId,
    event: PrEvent,
    /// `<repo> #<n>`.
    pr: String,
    /// The task's title, the PR's.
    title: String,
}

/// The changes to the PR tasks both reads hold, outside muted workspaces.
fn pr_changes(before: &Snapshot, after: &Snapshot) -> Vec<PrChange> {
    let mut changes = Vec::new();
    for task in &after.tasks {
        let (Some(pr), Some(was)) = (&task.pr, before.tasks.iter().find(|t| t.id == task.id))
        else {
            continue;
        };
        if muted(after, task.workspace_id) {
            continue;
        }
        let was = was.pr.as_ref();
        let mut push = |event| changes.push(change(task, event));
        let failing = |checks: Option<ChecksState>| checks == Some(ChecksState::Failure);
        if failing(pr.checks) && !was.is_some_and(|was| failing(was.checks)) {
            push(PrEvent::ChecksFailed);
        }
        let review = match pr.review {
            Some(ReviewState::Approved) => Some(PrEvent::Approved),
            Some(ReviewState::ChangesRequested) => Some(PrEvent::ChangesRequested),
            Some(ReviewState::ReviewRequired) | None => None,
        };
        if let Some(event) = review
            && was.is_none_or(|was| was.review != pr.review)
        {
            push(event);
        }
    }
    changes
}

/// `task`'s `event`, with what its alert says.
fn change(task: &Task, event: PrEvent) -> PrChange {
    let pr =
        (task.pr.as_ref()).map_or_else(String::new, |pr| format!("{} #{}", pr.repo, pr.number));
    PrChange {
        task: task.id,
        event,
        pr,
        title: task.title.clone(),
    }
}

/// One alert per change: "Checks failed" over "\<repo> #\<n> · \<title>"; or, when more than 3
/// PRs changed, one "N PRs changed" over their numbers that opens the Tasks view.
fn pr_alerts(changes: Vec<PrChange>) -> Vec<Alert> {
    let mut prs: Vec<TaskId> = changes.iter().map(|change| change.task).collect();
    prs.sort();
    prs.dedup();
    if prs.len() > COALESCE_OVER {
        let mut numbers: Vec<&str> = changes.iter().map(|change| change.pr.as_str()).collect();
        numbers.dedup();
        return vec![Alert {
            tag: "prs".into(),
            title: format!("{} PRs changed", prs.len()).into(),
            body: numbers.join(", ").into(),
            target: Target::Tasks,
        }];
    }
    (changes.into_iter())
        .map(|change| Alert {
            tag: format!("task:{}", change.task).into(),
            title: change.event.title().into(),
            body: format!("{} · {}", change.pr, change.title).into(),
            target: Target::Task(change.task),
        })
        .collect()
}

/// The PR changes of the sync running, until it ends.
#[derive(Debug, Default)]
pub struct PrBatch {
    changes: Vec<PrChange>,
}

impl PrBatch {
    /// Adds what changed from `before` to `after`, and lets the batch out as alerts unless
    /// `after` has a sync still running.
    pub fn read(&mut self, before: &Snapshot, after: &Snapshot) -> Vec<Alert> {
        self.changes.extend(pr_changes(before, after));
        let syncing = (after.pr_sync.as_ref()).is_some_and(|status| status.progress.is_some());
        if syncing {
            return Vec::new();
        }
        pr_alerts(std::mem::take(&mut self.changes))
    }
}

/// The unit tier's [`Notifier`](agentdeck_notify::Notifier), held to its contract by
/// `agentdeck-notify`'s integration tests of the real one.
#[cfg(test)]
pub(crate) mod fake {
    use std::cell::RefCell;

    use agentdeck_notify::{ClickHandler, Notification, Notifier};
    use gpui::{App, SharedString};

    /// Records what it shows and which are still open, one per tag, as the contract has it;
    /// [`click`](FakeNotifier::click) stands in for the user.
    #[derive(Default)]
    pub struct FakeNotifier {
        shown: RefCell<Vec<Notification>>,
        open: RefCell<Vec<SharedString>>,
        on_click: RefCell<Option<ClickHandler>>,
    }

    impl FakeNotifier {
        /// Everything shown, in order, replaced ones included.
        pub fn shown(&self) -> Vec<Notification> {
            self.shown.borrow().clone()
        }

        /// The tags of the notifications still open: neither withdrawn nor clicked.
        pub fn open(&self) -> Vec<SharedString> {
            self.open.borrow().clone()
        }

        /// Clicks the body of the notification tagged `tag`, which must be open. A click
        /// withdraws it.
        pub fn click(&self, tag: &str, cx: &mut App) {
            let open = self.open.borrow().iter().any(|open| open == tag);
            assert!(open, "no notification tagged {tag} is open");
            self.withdraw(tag);
            let mut handler = (self.on_click.borrow_mut().take()).expect("a click handler");
            handler(tag.to_owned().into(), cx);
            self.on_click.borrow_mut().get_or_insert(handler);
        }
    }

    impl Notifier for FakeNotifier {
        fn show(&self, notification: Notification) {
            let mut open = self.open.borrow_mut();
            if !open.contains(&notification.tag) {
                open.push(notification.tag.clone());
            }
            self.shown.borrow_mut().push(notification);
        }

        fn withdraw(&self, tag: &str) {
            self.open.borrow_mut().retain(|open| open != tag);
        }

        fn on_click(&self, handler: ClickHandler) {
            *self.on_click.borrow_mut() = Some(handler);
        }
    }
}

#[cfg(test)]
mod tests {
    use agentdeck_client::types::{PrSyncProgress, PrSyncStage, TaskPr};
    use agentdeck_core::{Agent, PrState, TaskKind};

    use super::*;
    use crate::overview::fixtures::*;

    /// Workspace `web` (id 1) with an idle Claude Code pane (id 10) in tab `tab 10`.
    fn web() -> Snapshot {
        let web = workspace(1, "web");
        Snapshot {
            panes: vec![PaneState {
                agent: Some(Agent::Claude),
                ..pane(10, web.id, AgentState::Idle)
            }],
            workspaces: vec![web],
            ..Snapshot::default()
        }
    }

    /// `snapshot` with pane 10 in `state`.
    fn with_state(mut snapshot: Snapshot, state: AgentState) -> Snapshot {
        snapshot.set_state(id(10), state);
        snapshot
    }

    fn muted_web() -> Snapshot {
        let mut snapshot = web();
        snapshot.workspaces[0].muted = true;
        snapshot
    }

    mod pane_alerts {
        use super::*;

        #[test]
        fn should_say_the_agent_is_blocked_over_its_tab_and_workspace() {
            let alerts = pane_alerts(&web(), &with_state(web(), AgentState::Blocked));

            assert_eq!(
                alerts,
                [Alert {
                    tag: format!("pane:{}", id::<PaneId>(10)).into(),
                    title: "Claude Code is blocked".into(),
                    body: "tab 10 · web".into(),
                    target: Target::Pane {
                        workspace: id(1),
                        pane: id(10),
                    },
                }]
            );
        }

        #[test]
        fn should_say_done_and_name_the_linked_task() {
            let mut before = web();
            before.panes[0].task = Some(id(20));
            before.tasks = vec![task(20, TaskKind::Plain, Some(id(1)))];

            let alerts = pane_alerts(&before, &with_state(before.clone(), AgentState::Done));

            assert_eq!(alerts[0].title, "Claude Code is done");
            assert_eq!(alerts[0].body, "task 20 · web");
        }

        #[test]
        fn should_alert_once_while_the_state_holds() {
            let blocked = with_state(web(), AgentState::Blocked);

            assert_eq!(pane_alerts(&blocked, &blocked), []);
        }

        #[test]
        fn should_not_alert_for_other_states() {
            for state in [AgentState::Working, AgentState::Idle, AgentState::Exited] {
                assert_eq!(
                    pane_alerts(&web(), &with_state(web(), state)),
                    [],
                    "{state:?}"
                );
            }
        }

        #[test]
        fn should_not_alert_on_the_first_read() {
            let alerts = pane_alerts(
                &Snapshot::default(),
                &with_state(web(), AgentState::Blocked),
            );

            assert_eq!(alerts, []);
        }

        #[test]
        fn should_not_alert_for_a_muted_workspace() {
            let alerts = pane_alerts(&muted_web(), &with_state(muted_web(), AgentState::Blocked));

            assert_eq!(alerts, []);
        }

        #[test]
        fn should_not_alert_for_a_shell() {
            let mut shell = web();
            shell.panes[0].agent = None;

            let alerts = pane_alerts(&shell, &with_state(shell.clone(), AgentState::Done));

            assert_eq!(alerts, []);
        }
    }

    mod pane_alert {
        use super::*;

        #[test]
        fn should_alert_for_a_live_state_change() {
            let after = with_state(web(), AgentState::Blocked);

            let alert = pane_alert(AgentState::Working, &after.panes[0], &after);

            assert_eq!(
                alert.map(|a| a.title),
                Some("Claude Code is blocked".into())
            );
        }
    }

    mod reattached {
        use super::*;

        /// pane 10's alert for `state`, and the snapshot it came from.
        fn alert(state: AgentState) -> (Vec<Alert>, Snapshot) {
            let after = with_state(web(), state);
            (pane_alerts(&web(), &after), after)
        }

        fn after(start: Instant, secs: u64) -> Instant {
            start + Duration::from_secs(secs)
        }

        #[test]
        fn should_leave_out_the_state_a_pane_held_when_it_reattached() {
            let start = Instant::now();
            let mut reattached = Reattached::default();
            reattached.attached(id(10), AgentState::Done, start);
            let (mut alerts, snapshot) = alert(AgentState::Done);

            reattached.retain(&mut alerts, &snapshot, after(start, 2));

            assert_eq!(alerts, []);
        }

        #[test]
        fn should_keep_it_once_the_quiet_has_passed() {
            let start = Instant::now();
            let mut reattached = Reattached::default();
            reattached.attached(id(10), AgentState::Done, start);
            let (mut alerts, snapshot) = alert(AgentState::Done);

            reattached.retain(&mut alerts, &snapshot, after(start, 10));

            assert_eq!(alerts.len(), 1);
        }

        #[test]
        fn should_keep_another_state() {
            let start = Instant::now();
            let mut reattached = Reattached::default();
            reattached.attached(id(10), AgentState::Done, start);
            let (mut alerts, snapshot) = alert(AgentState::Blocked);

            reattached.retain(&mut alerts, &snapshot, after(start, 2));

            assert_eq!(alerts.len(), 1);
        }

        #[test]
        fn should_keep_a_pane_that_did_not_reattach() {
            let start = Instant::now();
            let mut reattached = Reattached::default();
            reattached.attached(id(11), AgentState::Done, start);
            let (mut alerts, snapshot) = alert(AgentState::Done);

            reattached.retain(&mut alerts, &snapshot, after(start, 2));

            assert_eq!(alerts.len(), 1);
        }

        #[test]
        fn should_keep_every_pr_alert() {
            let mut reattached = Reattached::default();
            reattached.attached(id(10), AgentState::Done, Instant::now());
            let mut alerts = vec![Alert {
                tag: "prs".into(),
                title: "4 PRs changed".into(),
                body: "".into(),
                target: Target::Tasks,
            }];

            reattached.retain(&mut alerts, &web(), Instant::now());

            assert_eq!(alerts.len(), 1);
        }
    }

    mod pr_batch {
        use super::*;

        /// A PR task `n` in `workspace`, `<repo> #n`, with its checks and review.
        fn pr_task(
            n: u32,
            workspace: Option<WorkspaceId>,
            checks: Option<ChecksState>,
            review: Option<ReviewState>,
        ) -> Task {
            Task {
                pr: Some(TaskPr {
                    checks,
                    draft: false,
                    head_ref: "feat/x".into(),
                    number: i64::from(n),
                    repo: "acme/web".into(),
                    review,
                    state: PrState::Open,
                    synced_at: "2026-11-14T00:00:00Z".into(),
                    unreachable: false,
                    updated_at: "2026-11-14T00:00:00Z".into(),
                    url: format!("https://github.com/acme/web/pull/{n}"),
                }),
                title: format!("PR {n}"),
                ..task(n, TaskKind::Pr, workspace)
            }
        }

        /// `web` holding `tasks`.
        fn holding(tasks: Vec<Task>) -> Snapshot {
            Snapshot { tasks, ..web() }
        }

        fn pending(n: u32) -> Task {
            pr_task(n, Some(id(1)), Some(ChecksState::Pending), None)
        }

        fn failed(n: u32) -> Task {
            pr_task(n, Some(id(1)), Some(ChecksState::Failure), None)
        }

        #[test]
        fn should_say_checks_failed_over_the_pr_and_its_title() {
            let alerts =
                PrBatch::default().read(&holding(vec![pending(5)]), &holding(vec![failed(5)]));

            assert_eq!(
                alerts,
                [Alert {
                    tag: format!("task:{}", id::<TaskId>(5)).into(),
                    title: "Checks failed".into(),
                    body: "acme/web #5 · PR 5".into(),
                    target: Target::Task(id(5)),
                }]
            );
        }

        #[test]
        fn should_say_a_review_arrived() {
            let review = |review| pr_task(5, Some(id(1)), None, review);
            let before = holding(vec![review(Some(ReviewState::ReviewRequired))]);

            let approved = PrBatch::default()
                .read(&before, &holding(vec![review(Some(ReviewState::Approved))]));
            let changes = PrBatch::default().read(
                &before,
                &holding(vec![review(Some(ReviewState::ChangesRequested))]),
            );

            assert_eq!(approved[0].title, "Approved");
            assert_eq!(changes[0].title, "Changes requested");
        }

        #[test]
        fn should_not_alert_again_while_checks_stay_failed() {
            let alerts =
                PrBatch::default().read(&holding(vec![failed(5)]), &holding(vec![failed(5)]));

            assert_eq!(alerts, []);
        }

        #[test]
        fn should_not_alert_for_a_pr_first_seen() {
            let alerts = PrBatch::default().read(&holding(vec![]), &holding(vec![failed(5)]));

            assert_eq!(alerts, []);
        }

        #[test]
        fn should_not_alert_for_a_muted_workspaces_pr() {
            let before = Snapshot {
                tasks: vec![pending(5)],
                ..muted_web()
            };
            let after = Snapshot {
                tasks: vec![failed(5)],
                ..muted_web()
            };

            assert_eq!(PrBatch::default().read(&before, &after), []);
        }

        #[test]
        fn should_alert_for_an_inbox_pr() {
            let inbox = |checks| pr_task(5, None, Some(checks), None);

            let alerts = PrBatch::default().read(
                &holding(vec![inbox(ChecksState::Success)]),
                &holding(vec![inbox(ChecksState::Failure)]),
            );

            assert_eq!(alerts.len(), 1);
        }

        #[test]
        fn should_coalesce_more_than_three_prs_into_one() {
            let before = holding((1..=4).map(pending).collect());
            let after = holding((1..=4).map(failed).collect());

            let alerts = PrBatch::default().read(&before, &after);

            assert_eq!(
                alerts,
                [Alert {
                    tag: "prs".into(),
                    title: "4 PRs changed".into(),
                    body: "acme/web #1, acme/web #2, acme/web #3, acme/web #4".into(),
                    target: Target::Tasks,
                }]
            );
        }

        #[test]
        fn should_keep_three_prs_apart() {
            let before = holding((1..=3).map(pending).collect());
            let after = holding((1..=3).map(failed).collect());

            assert_eq!(PrBatch::default().read(&before, &after).len(), 3);
        }

        #[test]
        fn should_hold_a_syncs_changes_until_it_ends() {
            let syncing = |mut snapshot: Snapshot| {
                pr_sync(&mut snapshot).progress = Some(PrSyncProgress {
                    prs: 4,
                    prs_done: 2,
                    stage: PrSyncStage::Details,
                });
                snapshot
            };
            let mut batch = PrBatch::default();
            let start = holding((1..=4).map(pending).collect());
            let half = syncing(holding(vec![failed(1), failed(2), pending(3), pending(4)]));
            let done = holding((1..=4).map(failed).collect());

            assert_eq!(batch.read(&start, &half), []);
            let alerts = batch.read(&half, &done);

            assert_eq!(alerts.len(), 1, "{alerts:?}");
            assert_eq!(alerts[0].title, "4 PRs changed");
        }
    }
}
