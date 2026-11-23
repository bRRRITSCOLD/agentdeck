//! Desktop notifications (spec §11): what changed between two reads of the daemon that the
//! user should hear about, as [`Alert`]s.
//!
//! - **Panes:** an agent pane turning `blocked` or `done`, once per change. Whether its tab is
//!   in view is the window's call, so [`pane_alert`] doesn't know. [`Reattached`] leaves out a
//!   pane settling back, just after it reattached, into the state it held then.
//! - **PRs:** a PR you authored (its author is the login PR sync runs as) whose checks turn
//!   `failure`, or whose review turns `approved` or `changes_requested`. [`PrBatch`] holds
//!   them while a sync runs, and lets them out as one alert per PR once it ends, tagged by the
//!   PR ([`pr_tag`]), or as one "N PRs changed" when more than 3 PRs changed.
//!
//! A muted workspace's panes never alert. A PR alert is muted only when every unarchived
//! Workspace linking its Repository is muted; a Repository linked nowhere, or only by archived
//! Workspaces, always alerts. Only what both reads hold can change, so the first read alerts
//! nothing.
//!
//! The shell shows them through an `agentdeck_notify::Notifier`: the desktop's notification
//! server over one shared D-Bus connection, or, in the unit tier, `fake::FakeNotifier`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use agentdeck_client::types::PullRequest;
use agentdeck_core::{
    AgentState, ChecksState, Linking, PaneId, RepositoryId, ReviewState, WorkspaceId,
    pr_alerts_muted,
};
use agentdeck_notify::Notification;
use gpui::SharedString;

use crate::overview::{PaneState, Snapshot};
use crate::pull_requests::PrKey;
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
    /// The Pull requests view at a PR's row.
    Pr(PrKey),
    /// The Pull requests view, for several PRs at once.
    PullRequests,
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

/// Whether `workspace`'s pane notifications are muted.
fn muted(snapshot: &Snapshot, workspace: WorkspaceId) -> bool {
    (snapshot.workspaces.iter()).any(|w| w.id == workspace && w.muted)
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
    if before == pane.state || muted(snapshot, pane.workspace) {
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

/// What happened to a PR, most pressing first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PrEvent {
    ChecksFailed,
    ChangesRequested,
    Approved,
}

impl PrEvent {
    fn title(self) -> &'static str {
        match self {
            PrEvent::ChecksFailed => "Checks failed",
            PrEvent::ChangesRequested => "Changes requested",
            PrEvent::Approved => "Approved",
        }
    }
}

/// One PR's change, waiting for its sync to end.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PrChange {
    pr: PrKey,
    event: PrEvent,
    /// `<owner/name> #<n>`.
    reference: String,
    /// The PR's title.
    title: String,
}

/// Whether a PR alert for `repository` is muted: only when every unarchived Workspace linking
/// it is muted (spec §11). A Repository linked nowhere, or only by archived Workspaces, alerts.
fn pr_muted(snapshot: &Snapshot, repository: RepositoryId) -> bool {
    let linked = (snapshot.repositories.iter()).find(|r| r.id == repository);
    let linking = linked.map_or(&[][..], |r| r.workspaces.as_slice());
    pr_alerts_muted(linking.iter().filter_map(|id| {
        let workspace = snapshot.workspaces.iter().find(|w| w.id == *id)?;
        Some(Linking {
            archived: workspace.archived_at.is_some(),
            muted: workspace.muted,
        })
    }))
}

/// Whether `pr` is one the signed-in login authored: its author is who the last good sync ran
/// as (`synced_as`), as GitHub matches logins, ignoring case.
fn authored(pr: &PullRequest, snapshot: &Snapshot) -> bool {
    let me = (snapshot.pr_sync.as_ref()).and_then(|status| status.synced_as.as_deref());
    match (me, pr.author.as_deref()) {
        (Some(me), Some(author)) => me.eq_ignore_ascii_case(author),
        _ => false,
    }
}

/// The changes to the PRs you authored that both reads hold, unless muted: one per PR, the
/// most pressing when several happened at once.
fn pr_changes(before: &Snapshot, after: &Snapshot) -> Vec<PrChange> {
    let mut changes = Vec::new();
    for pr in (after.prs.iter()).filter(|pr| authored(pr, after)) {
        let key = PrKey::of(pr);
        let Some(was) = before.prs.iter().find(|was| PrKey::of(was) == key) else {
            continue;
        };
        if pr_muted(after, pr.repository.id) {
            continue;
        }
        let failing = |checks: Option<ChecksState>| checks == Some(ChecksState::Failure);
        let checks = (failing(pr.checks) && !failing(was.checks)).then_some(PrEvent::ChecksFailed);
        let review = match pr.review {
            Some(ReviewState::Approved) => Some(PrEvent::Approved),
            Some(ReviewState::ChangesRequested) => Some(PrEvent::ChangesRequested),
            Some(ReviewState::ReviewRequired) | None => None,
        };
        let review = review.filter(|_| was.review != pr.review);
        if let Some(event) = checks.into_iter().chain(review).min() {
            changes.push(PrChange {
                pr: key,
                event,
                reference: format!("{} #{}", pr.repository.name, pr.number),
                title: pr.title.clone(),
            });
        }
    }
    changes
}

/// One alert per PR: "Checks failed" over "\<owner/name> #\<n> · \<title>", tagged by the PR;
/// or, when more than 3 PRs changed, one "N PRs changed" over their references that opens the
/// Pull requests view.
fn pr_alerts(changes: Vec<PrChange>) -> Vec<Alert> {
    if changes.len() > COALESCE_OVER {
        let references: Vec<&str> = (changes.iter()).map(|c| c.reference.as_str()).collect();
        return vec![Alert {
            tag: "prs".into(),
            title: format!("{} PRs changed", changes.len()).into(),
            body: references.join(", ").into(),
            target: Target::PullRequests,
        }];
    }
    (changes.into_iter())
        .map(|change| Alert {
            tag: pr_tag(change.pr),
            title: change.event.title().into(),
            body: format!("{} · {}", change.reference, change.title).into(),
            target: Target::Pr(change.pr),
        })
        .collect()
}

/// A PR alert's tag: `pr:<Repository id>#<number>`, by GitHub's id for the Repository, so a
/// rename keeps it the same PR's.
pub fn pr_tag(pr: PrKey) -> SharedString {
    format!("pr:{}#{}", pr.repository, pr.number).into()
}

/// The PR changes of the sync running, until it ends: one per PR, a later one replacing it.
#[derive(Debug, Default)]
pub struct PrBatch {
    changes: Vec<PrChange>,
}

impl PrBatch {
    /// Adds what changed from `before` to `after`, and lets the batch out as alerts unless
    /// `after` has a sync still running.
    pub fn read(&mut self, before: &Snapshot, after: &Snapshot) -> Vec<Alert> {
        for change in pr_changes(before, after) {
            self.changes.retain(|held| held.pr != change.pr);
            self.changes.push(change);
        }
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
    use agentdeck_client::types::{PrSyncProgress, PrSyncStage};
    use agentdeck_core::{Agent, TaskKind};

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
                target: Target::PullRequests,
            }];

            reattached.retain(&mut alerts, &web(), Instant::now());

            assert_eq!(alerts.len(), 1);
        }
    }

    mod pr_batch {
        use agentdeck_client::types::PullRequest;

        use super::*;

        /// The signed-in login in these tests; [`pull_request`] is authored by it.
        const ME: &str = "octocat";

        /// PR `n` of `acme/web`, "PR n", authored by [`ME`], with its checks and review.
        fn pr(n: u32, checks: Option<ChecksState>, review: Option<ReviewState>) -> PullRequest {
            PullRequest {
                checks,
                review,
                ..pull_request(n, "acme/web")
            }
        }

        fn pending(n: u32) -> PullRequest {
            pr(n, Some(ChecksState::Pending), None)
        }

        fn failed(n: u32) -> PullRequest {
            pr(n, Some(ChecksState::Failure), None)
        }

        /// `web` holding `prs`, synced as [`ME`], `acme/web` linked by `web` (unmuted).
        fn holding(prs: Vec<PullRequest>) -> Snapshot {
            let mut status = pr_status();
            status.synced_as = Some(ME.into());
            Snapshot {
                prs,
                pr_sync: Some(status),
                repositories: vec![links("acme/web", &[id(1)])],
                ..web()
            }
        }

        fn key(n: u32) -> PrKey {
            PrKey {
                repository: repository_id("acme/web"),
                number: n,
            }
        }

        fn read(before: Vec<PullRequest>, after: Vec<PullRequest>) -> Vec<Alert> {
            PrBatch::default().read(&holding(before), &holding(after))
        }

        mod triggers {
            use super::*;

            #[test]
            fn should_say_checks_failed_over_the_pr_and_its_title_tagged_by_the_pr() {
                let alerts = read(vec![pending(5)], vec![failed(5)]);

                assert_eq!(
                    alerts,
                    [Alert {
                        tag: format!("pr:{}#5", repository_id("acme/web")).into(),
                        title: "Checks failed".into(),
                        body: "acme/web #5 · PR 5".into(),
                        target: Target::Pr(key(5)),
                    }]
                );
            }

            #[test]
            fn should_say_a_review_turned_approved_or_changes_requested() {
                let review = |review| pr(5, None, review);
                let before = vec![review(Some(ReviewState::ReviewRequired))];

                let approved = read(before.clone(), vec![review(Some(ReviewState::Approved))]);
                let changes = read(before, vec![review(Some(ReviewState::ChangesRequested))]);

                assert_eq!(approved[0].title, "Approved");
                assert_eq!(changes[0].title, "Changes requested");
            }

            #[test]
            fn should_not_alert_for_a_review_turning_required() {
                let review = |review| pr(5, None, review);

                let alerts = read(
                    vec![review(Some(ReviewState::Approved))],
                    vec![review(Some(ReviewState::ReviewRequired))],
                );

                assert_eq!(alerts, []);
            }

            #[test]
            fn should_not_alert_again_while_checks_stay_failed() {
                assert_eq!(read(vec![failed(5)], vec![failed(5)]), []);
            }

            #[test]
            fn should_not_alert_for_a_pr_first_seen() {
                assert_eq!(read(vec![], vec![failed(5)]), []);
            }
        }

        mod authored {
            use super::*;

            #[test]
            fn should_not_alert_for_a_pr_someone_else_authored() {
                let theirs = |checks| PullRequest {
                    author: Some("hubot".into()),
                    ..pr(5, Some(checks), None)
                };

                let alerts = read(
                    vec![theirs(ChecksState::Pending)],
                    vec![theirs(ChecksState::Failure)],
                );

                assert_eq!(alerts, []);
            }

            #[test]
            fn should_match_the_login_ignoring_case() {
                let mine = |checks| PullRequest {
                    author: Some("OctoCat".into()),
                    ..pr(5, Some(checks), None)
                };

                let alerts = read(
                    vec![mine(ChecksState::Pending)],
                    vec![mine(ChecksState::Failure)],
                );

                assert_eq!(alerts.len(), 1);
            }

            #[test]
            fn should_not_alert_before_a_sync_has_run_as_a_login() {
                let mut before = holding(vec![pending(5)]);
                let mut after = holding(vec![failed(5)]);
                for snapshot in [&mut before, &mut after] {
                    pr_sync(snapshot).synced_as = None;
                }

                assert_eq!(PrBatch::default().read(&before, &after), []);
            }
        }

        mod one_per_pr {
            use super::*;

            #[test]
            fn should_make_one_alert_when_checks_and_review_change_at_once() {
                let alerts = read(
                    vec![pending(5)],
                    vec![pr(
                        5,
                        Some(ChecksState::Failure),
                        Some(ReviewState::Approved),
                    )],
                );

                assert_eq!(alerts.len(), 1, "{alerts:?}");
                assert_eq!(alerts[0].title, "Checks failed");
            }

            #[test]
            fn should_keep_a_prs_latest_change_while_a_sync_runs() {
                let syncing = |mut snapshot: Snapshot| {
                    pr_sync(&mut snapshot).progress = Some(PrSyncProgress {
                        prs: 1,
                        prs_done: 0,
                        stage: PrSyncStage::Details,
                    });
                    snapshot
                };
                let mut batch = PrBatch::default();
                let start = holding(vec![pending(5)]);
                let half = syncing(holding(vec![failed(5)]));
                let approved = pr(5, Some(ChecksState::Failure), Some(ReviewState::Approved));
                let done = holding(vec![approved]);

                assert_eq!(batch.read(&start, &half), []);
                let alerts = batch.read(&half, &done);

                assert_eq!(alerts.len(), 1, "{alerts:?}");
                assert_eq!(alerts[0].title, "Approved");
                assert_eq!(alerts[0].tag, pr_tag(key(5)));
            }
        }

        mod muting {
            use super::*;

            /// `holding` with workspaces `web` (1) and `api` (2), `acme/web` linked by `linking`,
            /// each workspace muted and archived as `set` says.
            fn linked(
                prs: Vec<PullRequest>,
                linking: &[u32],
                set: &[(u32, bool, bool)],
            ) -> Snapshot {
                let mut snapshot = holding(prs);
                snapshot.workspaces = vec![workspace(1, "web"), workspace(2, "api")];
                for (n, muted, archived) in set {
                    let w = (snapshot.workspaces.iter_mut())
                        .find(|w| w.id == id(*n))
                        .unwrap();
                    w.muted = *muted;
                    w.archived_at = archived.then(|| "2026-11-20T00:00:00Z".into());
                }
                let linking: Vec<WorkspaceId> = linking.iter().map(|n| id(*n)).collect();
                snapshot.repositories = vec![links("acme/web", &linking)];
                snapshot
            }

            fn alerts(linking: &[u32], set: &[(u32, bool, bool)]) -> Vec<Alert> {
                let before = linked(vec![pending(5)], linking, set);
                let after = linked(vec![failed(5)], linking, set);
                PrBatch::default().read(&before, &after)
            }

            #[test]
            fn should_mute_when_every_linking_workspace_is_muted() {
                assert_eq!(alerts(&[1, 2], &[(1, true, false), (2, true, false)]), []);
            }

            #[test]
            fn should_alert_while_one_linking_workspace_is_unmuted() {
                assert_eq!(alerts(&[1, 2], &[(1, true, false)]).len(), 1);
            }

            #[test]
            fn should_alert_when_only_archived_muted_workspaces_link_it() {
                assert_eq!(alerts(&[1], &[(1, true, true)]).len(), 1);
            }

            #[test]
            fn should_alert_for_a_repository_linked_nowhere() {
                assert_eq!(alerts(&[], &[(1, true, false), (2, true, false)]).len(), 1);
            }

            #[test]
            fn should_leave_an_archived_unmuted_workspace_out_of_the_rule() {
                assert_eq!(alerts(&[1, 2], &[(1, false, true), (2, true, false)]), []);
            }
        }

        mod coalescing {
            use super::*;

            #[test]
            fn should_coalesce_more_than_three_prs_into_one_that_opens_the_view() {
                let alerts = read(
                    (1..=4).map(pending).collect(),
                    (1..=4).map(failed).collect(),
                );

                assert_eq!(
                    alerts,
                    [Alert {
                        tag: "prs".into(),
                        title: "4 PRs changed".into(),
                        body: "acme/web #1, acme/web #2, acme/web #3, acme/web #4".into(),
                        target: Target::PullRequests,
                    }]
                );
            }

            #[test]
            fn should_keep_three_prs_apart() {
                let alerts = read(
                    (1..=3).map(pending).collect(),
                    (1..=3).map(failed).collect(),
                );

                assert_eq!(alerts.len(), 3);
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
}
