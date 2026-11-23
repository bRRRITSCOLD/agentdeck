//! The Pull requests view (spec §11), Penpot "Screens · Main" → "Pull requests": every open
//! pull request you have, each under its own Repository group. Groups go alphabetically by the
//! Repository's name; a group with no PR to show is left out. Each group's header reads "Not in
//! a workspace" until Links come, and collapses at a click, remembered per Repository across
//! restarts in the GUI's own state ([`gui_state`]).
//!
//! Finished pull requests are hidden until **Show finished** is on (Penpot "Pull requests ·
//! show finished"): one preference, kept in the GUI's state too. A Finished row shows GitHub's
//! state with its state icon: merged, closed, or open past the age cutoff.
//!
//! A row's reference (`#63`) opens the PR on GitHub; nothing else on the row reacts to a click.
//! Its `…` menu has **Open on GitHub**, **Copy PR details** (the three lines
//! [`pr_details`] gives) and, while the PR can close, **Close PR** after a separator, which
//! asks first ([`ClosePrs`]). A PR the GitHub sign-in can't see is dimmed with its tooltip,
//! left out of the sidebar's count and never closed here.
//!
//! The header ends in Show finished, when PRs last synced and **Sync now**, which runs a full
//! sync and emits
//! [`Synced`] once done, so the window can have the sidebar read again: a sync that changes no
//! PR broadcasts nothing, yet moves the time it last ran. While no GitHub token works, the
//! logged-out banner sits over the list, its **Open Settings** asking for Settings with
//! [`OpenSettings`], as the cutoff line's "Settings" link does.
//!
//! Like the Inbox it shows the sidebar's [`Snapshot`], passed on by the window, and draws only
//! the rows in view.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use agentdeck_client::types::{GhLogin, PullRequest};
use agentdeck_core::{PrState, RepositoryId, pr_details};
use brrritscold_core_gpui::{
    ActiveTheme as _, Button, ButtonSize, ButtonVariant, DropdownMenu, DropdownMenuAlign,
    DropdownMenuContent, DropdownMenuItem, DropdownMenuItemVariant, DropdownMenuSeparator,
    IconName, Label, Switch,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, ListState, ParentElement as _, Render, SharedString, Styled as _, Task, Window,
    div, list,
};

use crate::api::Api;
use crate::close_prs::{ClosePrs, Target};
use crate::gui_state;
use crate::open_pr;
use crate::overview::Snapshot;
use crate::pr_table;
use crate::sync_parts::{self, OpenSettings, SETTINGS_TAIL, cutoff, logged_out_title};
use crate::task_row::{self, Header};
use crate::workspace::failed;

/// How often "Synced N min ago" is redrawn.
const SYNCED_TICK: Duration = Duration::from_secs(30);

/// The description's first line.
const ABOUT: &str = "Your open PRs, by repository.";

/// The description's first line while Show finished is on.
const ABOUT_FINISHED: &str = "Your open and finished PRs, by repository.";

/// What the view says with no PR to show (Penpot "Pull requests · empty").
pub const EMPTY: &str = "No open PRs. PRs you open on GitHub show here.";

/// A sync asked for on the Pull requests view has finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Synced;

/// A Repository's group: its PRs to show, by number.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// The Repository, by GitHub's id.
    pub repository: RepositoryId,
    /// `owner/name`, as GitHub calls it now.
    pub name: SharedString,
    /// What its header calls it: its name without the owner, or `owner/name` while another
    /// group's name without the owner is the same.
    pub label: SharedString,
    /// Its PRs, by number.
    pub prs: Vec<PullRequest>,
}

/// The groups of `prs` the view shows: one per Repository with a PR to show, alphabetical by
/// the Repository's name (without the owner, ignoring case, then by owner), its PRs by number.
/// Finished PRs are shown only with `show_finished`; merged and closed ones are Finished.
pub fn groups(prs: &[PullRequest], show_finished: bool) -> Vec<Group> {
    let shown = |pr: &&PullRequest| show_finished || (pr.state == PrState::Open && !pr.finished);
    let mut groups: Vec<Group> = Vec::new();
    for pr in prs.iter().filter(shown) {
        match groups.iter_mut().find(|g| g.repository == pr.repository.id) {
            Some(group) => group.prs.push(pr.clone()),
            None => groups.push(Group {
                repository: pr.repository.id,
                name: pr.repository.name.to_string().into(),
                label: pr.repository.name.name().to_owned().into(),
                prs: vec![pr.clone()],
            }),
        }
    }
    let shared: BTreeSet<String> = {
        let mut seen = BTreeSet::new();
        (groups.iter())
            .map(|g| g.label.to_lowercase())
            .filter(|label| !seen.insert(label.clone()))
            .collect()
    };
    for group in &mut groups {
        if shared.contains(&group.label.to_lowercase()) {
            group.label = group.name.clone();
        }
        group.prs.sort_by_key(|pr| pr.number);
    }
    groups.sort_by_key(|g| (g.label.to_lowercase(), g.name.to_lowercase(), g.repository));
    groups
}

/// One item of the view's list: a group's header, or a PR under an open group.
#[derive(Debug, Clone, PartialEq)]
enum Item {
    /// A group's header.
    Header {
        repository: RepositoryId,
        name: SharedString,
        label: SharedString,
        count: usize,
        collapsed: bool,
    },
    /// A PR.
    Row(Box<PullRequest>),
}

/// The list's items for `groups`, each group's rows under its header unless `collapsed`.
fn items(groups: &[Group], collapsed: &BTreeSet<RepositoryId>) -> Vec<Item> {
    let mut items = Vec::new();
    for group in groups {
        let folded = collapsed.contains(&group.repository);
        items.push(Item::Header {
            repository: group.repository,
            name: group.name.clone(),
            label: group.label.clone(),
            count: group.prs.len(),
            collapsed: folded,
        });
        if !folded {
            items.extend(group.prs.iter().map(|pr| Item::Row(Box::new(pr.clone()))));
        }
    }
    items
}

/// The Pull requests view.
pub struct PullRequestsView {
    api: Arc<dyn Api>,
    snapshot: Snapshot,
    /// Whether a sync asked for here is still running.
    syncing: bool,
    /// The items the list draws from, as last rendered.
    shown: Vec<Item>,
    list: ListState,
    /// Close PR's confirm dialog.
    close: Entity<ClosePrs>,
    /// Redraws "Synced N min ago" as time passes.
    _tick: Task<()>,
}

impl EventEmitter<Synced> for PullRequestsView {}
impl EventEmitter<OpenSettings> for PullRequestsView {}

impl PullRequestsView {
    /// A view that syncs and closes PRs through `api`; it shows nothing until given a
    /// snapshot.
    pub fn new(api: Arc<dyn Api>, cx: &mut Context<Self>) -> Self {
        let close = cx.new(|_| ClosePrs::new(api.clone(), "pull-requests"));
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(SYNCED_TICK).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        PullRequestsView {
            api,
            snapshot: Snapshot::default(),
            syncing: false,
            shown: Vec::new(),
            list: task_row::list_state(),
            close,
            _tick: tick,
        }
    }

    /// Shows `snapshot`.
    pub fn set_snapshot(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.snapshot = snapshot;
        cx.notify();
    }

    /// The groups shown.
    pub fn groups(&self, cx: &App) -> Vec<Group> {
        groups(&self.snapshot.prs, show_finished(cx))
    }

    /// Close PR's confirm dialog.
    #[cfg(test)]
    pub fn close_prs(&self) -> &Entity<ClosePrs> {
        &self.close
    }

    /// Collapses `repository`'s group, or opens it, remembering it for the next start.
    pub fn toggle(&mut self, repository: RepositoryId, cx: &mut Context<Self>) {
        gui_state::update(cx, |state| {
            let collapsed = state.collapsed.pull_requests.contains(&repository);
            state.collapsed.set_pull_requests(repository, !collapsed);
        });
        cx.notify();
    }

    /// Turns Show finished on or off, remembering it for the next start.
    pub fn set_show_finished(&mut self, show: bool, cx: &mut Context<Self>) {
        gui_state::update(cx, |state| state.show_finished = show);
        cx.notify();
    }

    /// Syncs PRs now, in full: what "Sync now" does. A sync already asked for isn't repeated.
    pub fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.syncing {
            return;
        }
        self.syncing = true;
        cx.notify();
        let syncing = self.api.sync_prs(true, cx);
        cx.spawn_in(window, async move |this, cx| {
            let synced = syncing.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.syncing = false;
                match synced {
                    Ok(()) => cx.emit(Synced),
                    Err(err) => failed("Couldn't sync PRs", &err, window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Where PR sync stands, at `now`: how far a running sync is, whoever started it, or
    /// "Syncing…" from the moment "Sync now" is clicked until the daemon says more.
    pub fn sync_label(&self, now: chrono::DateTime<chrono::Utc>) -> String {
        let sync = self.snapshot.overview().pr_sync;
        if self.syncing && !matches!(sync, crate::overview::PrSync::Syncing { .. }) {
            return "Syncing…".to_owned();
        }
        sync.label(now)
    }

    /// The lines under the heading: what the view holds, then which PRs the age cutoff leaves
    /// out, once the PR sync status is read.
    pub fn description(&self, cx: &App) -> Vec<SharedString> {
        let cutoff = (self.snapshot.pr_sync.as_ref()).map(|status| cutoff(status.max_age_days));
        let about = if show_finished(cx) {
            ABOUT_FINISHED
        } else {
            ABOUT
        };
        std::iter::once(SharedString::new_static(about))
            .chain(cutoff.map(SharedString::from))
            .collect()
    }

    /// The list's `ix`th item, spaced from the next.
    fn item(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let item = match self.shown.get(ix) {
            Some(Item::Header {
                repository,
                name,
                label,
                count,
                collapsed,
            }) => {
                let repository = *repository;
                pr_table::group_header(
                    name.clone(),
                    label.clone(),
                    *count,
                    *collapsed,
                    cx.listener(move |this, _, _, cx| this.toggle(repository, cx)),
                    cx,
                )
            }
            Some(Item::Row(pr)) => {
                let url = pr.url.clone();
                let menu = row_menu(&self.close, pr);
                pr_table::row(
                    pr,
                    menu,
                    move |_, window, cx| open_pr::open(&url, window, cx),
                    cx,
                )
            }
            None => return div().into_any_element(),
        };
        task_row::spaced(item)
    }

    /// Penpot `Header`: heading and description, a spacer, Show finished, when PRs last
    /// synced, "Sync now", `space.3` apart.
    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let synced = self.sync_label(chrono::Utc::now());
        let finished = show_finished(cx);
        let toggle =
            cx.listener(|this, checked: &bool, _, cx| this.set_show_finished(*checked, cx));
        let lines: Vec<AnyElement> = (self.description(cx).into_iter().enumerate())
            .map(|(ix, line)| {
                let line_div = div()
                    .debug_selector(move || format!("pull requests description {ix}"))
                    .text_size(font_size::SM)
                    .text_color(s.muted_foreground);
                match line.strip_suffix(SETTINGS_TAIL) {
                    Some(lead) => line_div
                        .flex()
                        .child(SharedString::from(lead.to_owned()))
                        .child(sync_parts::settings_link("pull-requests", cx))
                        .child(".")
                        .into_any_element(),
                    None => line_div.child(line).into_any_element(),
                }
            })
            .collect();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(space::S3)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::S1)
                    .child(
                        div()
                            .text_size(font_size::XL)
                            .font_weight(font_weight::MEDIUM)
                            .text_color(s.foreground)
                            .child("Pull requests"),
                    )
                    // Penpot's `Description` is one text of two lines: no gap between them.
                    .child(div().flex().flex_col().children(lines)),
            )
            .child(div().flex_1())
            // Penpot `ShowFinished`: a Switch and its Label, `space.2` apart.
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::S2)
                    .child(
                        div().debug_selector(|| "show finished".into()).child(
                            Switch::new("show-finished")
                                .checked(finished)
                                .accessibility_label("Show finished")
                                .on_change(move |checked, window, cx| {
                                    toggle(&checked, window, cx);
                                }),
                        ),
                    )
                    .child(Label::new().child("Show finished")),
            )
            .child(
                div()
                    .debug_selector(|| "pull requests synced".into())
                    .text_size(font_size::SM)
                    .text_color(s.muted_foreground)
                    .child(synced),
            )
            .child(
                div().debug_selector(|| "sync now".into()).child(
                    Button::new("sync-now")
                        .variant(ButtonVariant::Outline)
                        .label("Sync now")
                        .disabled(self.syncing)
                        .on_click(cx.listener(|this, _, window, cx| this.sync(window, cx))),
                ),
            )
    }
}

/// Whether Show finished is on.
fn show_finished(cx: &App) -> bool {
    gui_state::read(cx, |state| state.show_finished)
}

/// One entry in a row's `…` menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuEntry {
    /// **Open on GitHub**.
    OpenOnGitHub,
    /// **Copy PR details**, with no shortcut.
    CopyPrDetails,
    /// A line between the entries.
    Separator,
    /// **Close PR**, which asks first.
    ClosePr,
}

/// `pr`'s `…` menu, top to bottom: **Open on GitHub** and **Copy PR details** on every PR, then,
/// while it is open and the GitHub sign-in can see it, a separator and **Close PR**.
pub fn menu_entries(pr: &PullRequest) -> Vec<MenuEntry> {
    let mut entries = vec![MenuEntry::OpenOnGitHub, MenuEntry::CopyPrDetails];
    if pr.state == PrState::Open && !pr.unreachable {
        entries.extend([MenuEntry::Separator, MenuEntry::ClosePr]);
    }
    entries
}

/// `pr`'s `…` menu of its [`menu_entries`], Close PR in `destructive`. Tests find it as
/// `pr menu <reference>`.
fn row_menu(close: &Entity<ClosePrs>, pr: &PullRequest) -> AnyElement {
    let key = pr.reference.to_string();
    let content = (menu_entries(pr).into_iter()).fold(
        DropdownMenuContent::new(),
        |content, entry| match entry {
            MenuEntry::OpenOnGitHub => {
                let url = pr.url.clone();
                content.item(
                    DropdownMenuItem::new("Open on GitHub")
                        .on_click(move |_, window, cx| open_pr::open(&url, window, cx)),
                )
            }
            MenuEntry::CopyPrDetails => {
                let details = pr_details(
                    &pr.reference,
                    &pr.title,
                    &pr.url,
                    &pr.head_ref,
                    &pr.base_ref,
                );
                content.item(DropdownMenuItem::new("Copy PR details").on_click(
                    move |_, window, cx| {
                        crate::notice::copy(details.clone(), "Copied PR details", window, cx);
                    },
                ))
            }
            MenuEntry::Separator => content.separator(DropdownMenuSeparator::new()),
            MenuEntry::ClosePr => {
                let repo = pr.repository.name.name().to_owned();
                let target = Target {
                    id: None,
                    pr: pr.reference.clone(),
                    reference: format!("{repo} #{}", pr.number).into(),
                    repo: repo.into(),
                };
                let close = close.clone();
                content.item(
                    DropdownMenuItem::new("Close PR")
                        .variant(DropdownMenuItemVariant::Destructive)
                        .on_click(move |_, window, cx| {
                            let target = target.clone();
                            close.update(cx, |close, cx| close.ask_one(target, window, cx));
                        }),
                )
            }
        },
    );
    let selector = key.clone();
    div()
        .debug_selector(move || format!("pr menu {selector}"))
        .child(
            // Penpot "Pull requests · row menu" ends it on the trigger's right edge.
            DropdownMenu::new(SharedString::from(format!("pull-requests-menu-{key}")))
                .align(DropdownMenuAlign::End)
                .trigger(
                    Button::new(SharedString::from(format!(
                        "pull-requests-menu-trigger-{key}"
                    )))
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::IconXs)
                    .icon(IconName::Ellipsis),
                )
                .content(content),
        )
        .into_any_element()
}

impl Render for PullRequestsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let groups = self.groups(cx);
        let collapsed = gui_state::read(cx, |state| state.collapsed.pull_requests.clone());
        let items = items(&groups, &collapsed);
        if items != self.shown {
            task_row::relist(&self.list, items.len());
            self.shown = items;
        }
        let banner = (self.snapshot.pr_sync.as_ref())
            .filter(|status| status.login == GhLogin::LoggedOut)
            .map(|status| logged_out_title(status.synced_as.as_deref()));
        // Penpot `Content`: Header / Columns / List, `space.4` apart, padded `space.6`.
        div()
            .size_full()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(space::S4)
            .p(space::S6)
            .bg(s.background)
            .child(self.header(cx))
            // Penpot "Pull requests · logged out": a default `Alert` over the list.
            .when_some(banner, |content, title| {
                content.child(sync_parts::logged_out_banner("pull-requests", title, cx))
            })
            .map(|content| {
                if self.shown.is_empty() {
                    // Penpot "Pull requests · empty": the words alone, no columns.
                    return content.child(
                        div()
                            .debug_selector(|| "pull requests empty".into())
                            .px(space::S2)
                            .text_size(font_size::SM)
                            .text_color(s.muted_foreground)
                            .child(EMPTY),
                    );
                }
                let rows = div()
                    .debug_selector(|| "pull requests list".into())
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        list(
                            self.list.clone(),
                            cx.processor(|this, ix, _, cx| this.item(ix, cx)),
                        )
                        .flex_1()
                        .min_h_0(),
                    );
                let header = Header {
                    columns: pr_table::columns(cx),
                    ghost: pr_table::columns(cx),
                    min_width: pr_table::min_width(),
                };
                content.child(task_row::table_of(
                    "pull-requests-table",
                    "pull requests table",
                    header,
                    rows,
                    &self.list,
                    |_, _, _| {},
                    cx,
                ))
            })
            // The confirm dialog's host: out of the flow, so it takes no gap.
            .child(div().absolute().child(self.close.clone()))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use agentdeck_client::types::{PrSyncProgress, PrSyncStage};
    use agentdeck_core::{ChecksState, PrRef, ReviewState};
    use brrritscold_core_gpui::{Root, Theme, ThemePreference};
    use gpui::{Modifiers, TestAppContext, VisualTestContext, point, px};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::overview::fixtures::{pr_status, pr_sync, pull_request, repository_id};

    struct Harness {
        api: Arc<FakeApi>,
        view: Entity<PullRequestsView>,
        synced: Rc<RefCell<usize>>,
        settings: Rc<RefCell<usize>>,
        _events: [gpui::Subscription; 2],
    }

    impl Harness {
        /// A `Root` window on the view showing `snapshot`, over a fake daemon holding it.
        fn open(snapshot: Snapshot, cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            let api = Arc::new(FakeApi::with(snapshot.clone()));
            let mut view = None;
            let window = cx.add_window(|window, cx| {
                let api: Arc<dyn Api> = api.clone();
                let prs = cx.new(|cx| {
                    let mut prs = PullRequestsView::new(api, cx);
                    prs.set_snapshot(snapshot, cx);
                    prs
                });
                view = Some(prs.clone());
                Root::new(prs, window, cx)
            });
            let cx = VisualTestContext::from_window(*window, cx).into_mut();
            let view = view.unwrap();
            let (synced, settings) = (Rc::new(RefCell::new(0)), Rc::new(RefCell::new(0)));
            let (seen_sync, seen_settings) = (synced.clone(), settings.clone());
            let events = cx.update(|_, cx| {
                [
                    cx.subscribe(&view, move |_, _: &Synced, _| *seen_sync.borrow_mut() += 1),
                    cx.subscribe(&view, move |_, _: &OpenSettings, _| {
                        *seen_settings.borrow_mut() += 1;
                    }),
                ]
            });
            cx.run_until_parked();
            let harness = Harness {
                api,
                view,
                synced,
                settings,
                _events: events,
            };
            harness.draw(cx);
            (harness, cx)
        }

        fn draw(&self, cx: &mut VisualTestContext) {
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        /// Shows `snapshot`, as the window does when the sidebar reads it.
        fn show(&self, snapshot: Snapshot, cx: &mut VisualTestContext) {
            cx.update(|_, cx| {
                self.view
                    .update(cx, |view, cx| view.set_snapshot(snapshot, cx))
            });
            self.draw(cx);
        }

        fn asking(&self, cx: &mut VisualTestContext) -> bool {
            self.view
                .read_with(cx, |view, cx| view.close_prs().read(cx).asking())
        }
    }

    fn shows(selector: String, cx: &mut VisualTestContext) -> bool {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.debug_bounds(selector.leak()).is_some()
    }

    fn bounds(selector: String, cx: &mut VisualTestContext) -> gpui::Bounds<gpui::Pixels> {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.debug_bounds(selector.clone().leak()).expect(&selector)
    }

    fn click(selector: String, cx: &mut VisualTestContext) {
        let at = bounds(selector, cx).center();
        cx.simulate_click(at, Modifiers::default());
        cx.run_until_parked();
    }

    fn toasts(cx: &mut VisualTestContext) -> Vec<String> {
        cx.update(|_, cx| crate::notice::shown(cx))
            .into_iter()
            .map(|notice| notice.title.to_string())
            .collect()
    }

    fn pr_ref(text: &str) -> PrRef {
        text.parse().expect("a PR reference")
    }

    /// `acme/widgets` #1 and #4, `acme/gadgets` #2, and `acme/gizmos` #3, merged and so
    /// Finished, signed in.
    fn prs() -> Snapshot {
        Snapshot {
            prs: vec![
                pull_request(2, "acme/gadgets"),
                PullRequest {
                    state: PrState::Merged,
                    finished: true,
                    ..pull_request(3, "acme/gizmos")
                },
                pull_request(4, "acme/widgets"),
                pull_request(1, "acme/widgets"),
            ],
            pr_sync: Some(pr_status()),
            ..Snapshot::default()
        }
    }

    fn names(groups: &[Group]) -> Vec<String> {
        groups.iter().map(|g| g.label.to_string()).collect()
    }

    mod groups {
        use super::*;

        #[test]
        fn should_put_each_pr_under_its_repository_alphabetically_by_name() {
            let groups = groups(&prs().prs, false);

            assert_eq!(names(&groups), ["gadgets", "widgets"]);
            let numbers: Vec<u32> = groups[1].prs.iter().map(|pr| pr.number).collect();
            assert_eq!(numbers, [1, 4]);
        }

        #[test]
        fn should_sort_by_name_ignoring_its_case_and_owner() {
            let prs = [
                pull_request(1, "zed/Beta"),
                pull_request(2, "acme/gamma"),
                pull_request(3, "bee/alpha"),
            ];

            assert_eq!(names(&groups(&prs, false)), ["alpha", "Beta", "gamma"]);
        }

        #[test]
        fn should_leave_out_a_repository_with_no_open_pr() {
            let groups = groups(&prs().prs, false);

            assert!(!names(&groups).contains(&"gizmos".to_owned()));
        }

        #[test]
        fn should_leave_out_finished_prs_unless_asked_for_them() {
            let prs = [
                pull_request(1, "acme/widgets"),
                PullRequest {
                    finished: true,
                    ..pull_request(2, "acme/widgets")
                },
                PullRequest {
                    state: PrState::Closed,
                    finished: true,
                    ..pull_request(5, "acme/widgets")
                },
                PullRequest {
                    state: PrState::Merged,
                    finished: true,
                    ..pull_request(3, "acme/gizmos")
                },
            ];
            let numbers = |groups: Vec<Group>| -> Vec<u32> {
                (groups.iter())
                    .flat_map(|g| g.prs.iter().map(|pr| pr.number))
                    .collect()
            };

            assert_eq!(numbers(groups(&prs, false)), [1]);
            assert_eq!(numbers(groups(&prs, true)), [3, 1, 2, 5]);
        }

        #[test]
        fn should_name_groups_that_share_a_name_by_owner_too() {
            let prs = [
                pull_request(1, "me/agentdeck"),
                pull_request(2, "upstream/agentdeck"),
            ];

            assert_eq!(
                names(&groups(&prs, false)),
                ["me/agentdeck", "upstream/agentdeck"]
            );
        }

        #[test]
        fn should_keep_drafts_and_prs_the_sign_in_cannot_see() {
            let prs = [
                PullRequest {
                    draft: true,
                    ..pull_request(1, "acme/widgets")
                },
                PullRequest {
                    unreachable: true,
                    ..pull_request(2, "acme/widgets")
                },
            ];

            assert_eq!(groups(&prs, false)[0].prs.len(), 2);
        }
    }

    mod menu_entries {
        use super::*;

        #[test]
        fn should_offer_open_copy_then_close_on_an_open_pr() {
            assert_eq!(
                menu_entries(&pull_request(1, "acme/widgets")),
                [
                    MenuEntry::OpenOnGitHub,
                    MenuEntry::CopyPrDetails,
                    MenuEntry::Separator,
                    MenuEntry::ClosePr
                ]
            );
        }

        #[test]
        fn should_offer_to_close_a_finished_pr_still_open() {
            let stale = PullRequest {
                finished: true,
                ..pull_request(1, "acme/widgets")
            };

            assert_eq!(menu_entries(&stale).last(), Some(&MenuEntry::ClosePr));
        }

        #[test]
        fn should_not_offer_to_close_a_merged_pr() {
            let merged = PullRequest {
                state: PrState::Merged,
                finished: true,
                ..pull_request(1, "acme/widgets")
            };

            assert_eq!(
                menu_entries(&merged),
                [MenuEntry::OpenOnGitHub, MenuEntry::CopyPrDetails]
            );
        }

        #[test]
        fn should_not_offer_to_close_a_pr_the_sign_in_cannot_see() {
            let unreachable = PullRequest {
                unreachable: true,
                ..pull_request(1, "acme/widgets")
            };

            assert_eq!(
                menu_entries(&unreachable),
                [MenuEntry::OpenOnGitHub, MenuEntry::CopyPrDetails]
            );
        }
    }

    #[gpui::test]
    fn should_draw_in_both_themes(cx: &mut TestAppContext) {
        let (harness, cx) = Harness::open(prs(), cx);

        for preference in [ThemePreference::Light, ThemePreference::Dark] {
            cx.update(|_, cx| Theme::set_preference(preference, cx));
            cx.run_until_parked();
            harness.draw(cx);
        }
    }

    mod list {
        use super::*;

        #[gpui::test]
        fn should_draw_each_group_header_over_its_prs_in_order(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(prs(), cx);

            let tops: Vec<_> = [
                "group acme/gadgets",
                "pr acme/gadgets#2",
                "group acme/widgets",
                "pr acme/widgets#1",
                "pr acme/widgets#4",
            ]
            .into_iter()
            .map(|selector| bounds(selector.into(), cx).top())
            .collect();

            assert!(tops.is_sorted(), "{tops:?}");
            assert!(!shows("pr acme/gizmos#3".into(), cx));
            assert!(!shows("group acme/gizmos".into(), cx));
        }

        #[gpui::test]
        fn should_head_every_group_not_in_a_workspace(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(prs(), cx);

            let header = bounds("group acme/gadgets".into(), cx);
            let workspaces = bounds("group workspaces acme/gadgets".into(), cx);

            assert!(
                workspaces.left() > header.right(),
                "{workspaces:?} after {header:?}"
            );
            assert_eq!(pr_table::NOT_IN_A_WORKSPACE, "Not in a workspace");
        }

        #[gpui::test]
        fn should_name_the_columns(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(prs(), cx);

            for column in ["Title", "PR", "Checks", "Review"] {
                assert!(shows(format!("pr column {column}"), cx), "{column}");
            }
            assert!(!shows("pr column Status".into(), cx));
        }

        #[gpui::test]
        fn should_show_checks_and_review(cx: &mut TestAppContext) {
            let mut snapshot = prs();
            snapshot.prs[0].checks = Some(ChecksState::Failure);
            snapshot.prs[0].review = Some(ReviewState::Approved);
            let (_harness, cx) = Harness::open(snapshot, cx);

            let checks = bounds("checks acme/gadgets#2".into(), cx);
            let review = bounds("review acme/gadgets#2".into(), cx);

            assert!(checks.size.width > px(0.) && review.left() > checks.left());
        }

        #[gpui::test]
        fn should_say_there_are_no_open_prs_with_none_to_show(cx: &mut TestAppContext) {
            let snapshot = Snapshot {
                prs: vec![PullRequest {
                    state: PrState::Closed,
                    ..pull_request(1, "acme/widgets")
                }],
                ..prs()
            };
            let (_harness, cx) = Harness::open(snapshot, cx);

            assert!(shows("pull requests empty".into(), cx));
            assert!(!shows("pull requests table".into(), cx));
            assert_eq!(EMPTY, "No open PRs. PRs you open on GitHub show here.");
        }

        #[gpui::test]
        fn should_list_prs_that_arrive_after_the_empty_state(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(Snapshot::default(), cx);
            assert!(shows("pull requests empty".into(), cx));

            harness.show(prs(), cx);

            assert!(!shows("pull requests empty".into(), cx));
            assert!(shows("pr acme/widgets#1".into(), cx));
        }
    }

    mod collapse {
        use super::*;

        #[gpui::test]
        fn should_hide_a_groups_prs_until_its_header_is_clicked_again(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(prs(), cx);

            click("group acme/widgets".into(), cx);

            assert!(!shows("pr acme/widgets#1".into(), cx));
            assert!(shows("group acme/widgets".into(), cx));
            assert!(shows("pr acme/gadgets#2".into(), cx));

            click("group acme/widgets".into(), cx);

            assert!(shows("pr acme/widgets#1".into(), cx));
        }

        #[gpui::test]
        fn should_remember_a_collapsed_group_across_restarts(cx: &mut TestAppContext) {
            let dir = tempfile::tempdir().expect("a temp dir");
            let path = dir.path().join(gui_state::FILE);
            cx.update(|cx| gui_state::init(Some(path.clone()), cx));
            let (_harness, window) = Harness::open(prs(), cx);
            click("group acme/widgets".into(), window);

            // The next start reads the file again.
            let saved = gui_state::load(&path);
            assert_eq!(
                saved.collapsed.pull_requests,
                BTreeSet::from([repository_id("acme/widgets")])
            );
            cx.update(|cx| gui_state::init(Some(path), cx));
            let (_harness, cx) = Harness::open(prs(), cx);

            assert!(!shows("pr acme/widgets#1".into(), cx));
            assert!(shows("pr acme/gadgets#2".into(), cx));
        }

        #[gpui::test]
        fn should_keep_a_renamed_repositorys_group_collapsed(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            click("group acme/widgets".into(), cx);

            let mut renamed = prs();
            for pr in renamed.prs.iter_mut().filter(|pr| pr.number != 2) {
                pr.repository.name = "acme/gizmos-v2".parse().unwrap();
            }
            harness.show(renamed, cx);

            assert!(shows("group acme/gizmos-v2".into(), cx));
            assert!(!shows("pr acme/widgets#1".into(), cx));
        }
    }

    mod row {
        use super::*;

        #[gpui::test]
        fn should_open_the_pr_on_github_from_its_reference(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(prs(), cx);
            let opener = cx.update(|_, cx| crate::terminal::open::fake::install(cx));

            click("pr link acme/widgets#4".into(), cx);

            assert_eq!(
                *opener.started.lock().unwrap(),
                [[
                    "xdg-open".to_owned(),
                    "https://github.com/acme/widgets/pull/4".to_owned()
                ]]
            );
        }

        #[gpui::test]
        fn should_do_nothing_on_a_click_elsewhere_on_the_row(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            let opener = cx.update(|_, cx| crate::terminal::open::fake::install(cx));

            click("pr title acme/widgets#4".into(), cx);
            let row = bounds("pr acme/widgets#4".into(), cx);
            cx.simulate_click(
                point(row.left() + px(2.), row.center().y),
                Modifiers::default(),
            );
            cx.run_until_parked();

            assert!(opener.started.lock().unwrap().is_empty());
            assert!(!harness.asking(cx));
            assert!(toasts(cx).is_empty());
        }

        #[gpui::test]
        fn should_read_the_reference_as_its_number(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(prs(), cx);
            let link = bounds("pr link acme/widgets#4".into(), cx);
            let column = bounds("pr column PR".into(), cx);

            assert_eq!(link.left(), column.left());
        }
    }

    mod row_menu {
        use super::*;

        /// Opens `pr`'s `…` menu and picks its `n`th item by keyboard.
        fn pick(pr: &str, n: usize, cx: &mut VisualTestContext) {
            click(format!("pr menu {pr}"), cx);
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.simulate_keystrokes(&format!("{}enter", "down ".repeat(n)));
            cx.run_until_parked();
        }

        #[gpui::test]
        fn should_open_the_pr_on_github_from_its_first_item(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            let opener = cx.update(|_, cx| crate::terminal::open::fake::install(cx));

            pick("acme/gadgets#2", 1, cx);

            assert_eq!(
                *opener.started.lock().unwrap(),
                [[
                    "xdg-open".to_owned(),
                    "https://github.com/acme/gadgets/pull/2".to_owned()
                ]]
            );
            assert!(!harness.asking(cx));
        }

        #[gpui::test]
        fn should_copy_the_pr_details_from_its_second_item(cx: &mut TestAppContext) {
            let mut snapshot = prs();
            snapshot.prs[0].title = "Fix pane restore".into();
            snapshot.prs[0].head_ref = "fix/pane-restore".into();
            let (_harness, cx) = Harness::open(snapshot, cx);

            pick("acme/gadgets#2", 2, cx);

            let copied = cx.read_from_clipboard().and_then(|item| item.text());
            assert_eq!(
                copied.as_deref(),
                Some(
                    "acme/gadgets#2: Fix pane restore\n\
                     https://github.com/acme/gadgets/pull/2\n\
                     fix/pane-restore → main"
                )
            );
            assert_eq!(toasts(cx), ["Copied PR details"]);
        }

        #[gpui::test]
        fn should_ask_then_close_the_pr_from_the_item_after_the_separator(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);

            pick("acme/widgets#4", 3, cx);
            assert!(harness.asking(cx));
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();

            assert_eq!(
                *harness.api.closes.lock().unwrap(),
                [vec![pr_ref("acme/widgets#4")]]
            );
            assert_eq!(toasts(cx).last().map(String::as_str), Some("Closed 1 PR"));
        }

        #[gpui::test]
        fn should_close_nothing_when_the_dialog_is_cancelled(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);

            pick("acme/widgets#4", 3, cx);
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();

            assert!(!harness.asking(cx));
            assert!(harness.api.closes.lock().unwrap().is_empty());
        }

        #[gpui::test]
        fn should_name_the_pr_when_github_refuses_to_close_it(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            (harness.api).refuse_close_pr(&pr_ref("acme/widgets#4"), "Must have admin rights");

            pick("acme/widgets#4", 3, cx);
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();

            let shown = cx.update(|_, cx| crate::notice::shown(cx));
            let toast = shown.last().expect("a toast");
            assert_eq!(toast.title, "Couldn't close widgets #4");
            assert_eq!(toast.description.as_deref(), Some("Must have admin rights"));
        }

        #[gpui::test]
        fn should_leave_the_closed_pr_out_once_the_daemon_says_it_closed(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            pick("acme/widgets#4", 3, cx);
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();

            // The close's `changed` reads the snapshot again, the PR stored closed.
            harness.show(harness.api.snapshot_now(), cx);

            assert!(!shows("pr acme/widgets#4".into(), cx));
            assert!(shows("pr acme/widgets#1".into(), cx));
        }
    }

    mod unreachable {
        use super::*;

        /// [`prs`] with `acme/secret` #5, open but out of the GitHub sign-in's sight.
        fn with_unreachable() -> Snapshot {
            let mut snapshot = prs();
            snapshot.prs.push(PullRequest {
                unreachable: true,
                ..pull_request(5, "acme/secret")
            });
            snapshot
        }

        /// The tooltip shown while the pointer is on `at`, then the pointer moves off.
        fn tooltip_on(
            at: gpui::Point<gpui::Pixels>,
            cx: &mut VisualTestContext,
        ) -> Option<gpui::Bounds<gpui::Pixels>> {
            cx.simulate_mouse_move(at, None, Modifiers::default());
            cx.run_until_parked();
            let tooltip = cx.debug_bounds("tooltip-content");
            cx.simulate_mouse_move(point(px(1.), px(1.)), None, Modifiers::default());
            cx.run_until_parked();
            tooltip
        }

        #[gpui::test]
        fn should_list_it_with_its_tooltip_above_its_title(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(with_unreachable(), cx);
            let title = bounds("pr title acme/secret#5".into(), cx);

            let tooltip = tooltip_on(title.center(), cx).expect("a tooltip on hover");

            assert!(
                tooltip.bottom() < title.top(),
                "{tooltip:?} above {title:?}"
            );
            let other = bounds("pr title acme/widgets#1".into(), cx);
            assert!(tooltip_on(other.center(), cx).is_none());
        }

        #[gpui::test]
        fn should_open_and_copy_it_but_not_close_it(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(with_unreachable(), cx);
            let opener = cx.update(|_, cx| crate::terminal::open::fake::install(cx));

            click("pr menu acme/secret#5".into(), cx);
            cx.simulate_keystrokes("down enter");
            cx.run_until_parked();
            click("pr menu acme/secret#5".into(), cx);
            cx.simulate_keystrokes("down down enter");
            cx.run_until_parked();

            assert_eq!(opener.started.lock().unwrap().len(), 1);
            let copied = cx.read_from_clipboard().and_then(|item| item.text());
            assert!(copied.is_some_and(|text| text.starts_with("acme/secret#5: PR 5\n")));
            assert!(!harness.asking(cx));
        }
    }

    mod show_finished {
        use super::*;

        #[gpui::test]
        fn should_hide_finished_prs_until_it_is_on(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(prs(), cx);
            assert!(!shows("pr acme/gizmos#3".into(), cx));

            click("show finished".into(), cx);

            assert!(shows("group acme/gizmos".into(), cx));
            assert!(shows("pr acme/gizmos#3".into(), cx));
            assert!(shows("pr acme/widgets#1".into(), cx));

            click("show finished".into(), cx);

            assert!(!shows("pr acme/gizmos#3".into(), cx));
        }

        #[gpui::test]
        fn should_remember_it_across_restarts(cx: &mut TestAppContext) {
            let dir = tempfile::tempdir().expect("a temp dir");
            let path = dir.path().join(gui_state::FILE);
            cx.update(|cx| gui_state::init(Some(path.clone()), cx));
            let (_harness, window) = Harness::open(prs(), cx);
            click("show finished".into(), window);

            // The next start reads the file again.
            assert!(gui_state::load(&path).show_finished);
            cx.update(|cx| gui_state::init(Some(path), cx));
            let (_harness, cx) = Harness::open(prs(), cx);

            assert!(shows("pr acme/gizmos#3".into(), cx));
        }

        #[gpui::test]
        fn should_sit_in_the_header_before_when_prs_synced(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(prs(), cx);

            let toggle = bounds("show finished".into(), cx);
            let synced = bounds("pull requests synced".into(), cx);

            assert!(
                toggle.right() < synced.left(),
                "{toggle:?} before {synced:?}"
            );
        }

        #[gpui::test]
        fn should_say_the_list_holds_finished_prs_while_on(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);

            click("show finished".into(), cx);

            let lines = harness.view.read_with(cx, |view, cx| view.description(cx));
            assert_eq!(lines[0], ABOUT_FINISHED);
            assert_eq!(ABOUT_FINISHED, "Your open and finished PRs, by repository.");
        }

        #[gpui::test]
        fn should_count_the_finished_prs_in_their_group(cx: &mut TestAppContext) {
            let mut snapshot = prs();
            snapshot.prs.push(PullRequest {
                state: PrState::Closed,
                finished: true,
                ..pull_request(5, "acme/widgets")
            });
            let (harness, cx) = Harness::open(snapshot, cx);

            click("show finished".into(), cx);

            let groups = harness.view.read_with(cx, |view, cx| view.groups(cx));
            let widgets = groups
                .iter()
                .find(|g| g.label == "widgets")
                .expect("widgets");
            assert_eq!(widgets.prs.len(), 3);
        }
    }

    mod sync_now {
        use super::*;

        #[gpui::test]
        fn should_run_a_full_sync_and_say_so(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);

            click("sync now".into(), cx);

            assert_eq!(*harness.api.syncs.lock().unwrap(), [true]);
            assert_eq!(*harness.synced.borrow(), 1);
        }

        #[gpui::test]
        fn should_sit_at_the_headers_end_after_when_prs_synced(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(prs(), cx);

            let synced = bounds("pull requests synced".into(), cx);
            let button = bounds("sync now".into(), cx);

            assert!(
                button.left() > synced.right(),
                "{button:?} after {synced:?}"
            );
        }

        #[gpui::test]
        fn should_say_how_far_a_sync_it_did_not_start_is(cx: &mut TestAppContext) {
            let mut snapshot = prs();
            pr_sync(&mut snapshot).progress = Some(PrSyncProgress {
                prs_done: 50,
                prs: 271,
                stage: PrSyncStage::Listing,
            });
            let (harness, cx) = Harness::open(snapshot, cx);

            let label = harness
                .view
                .read_with(cx, |view, _| view.sync_label(chrono::Utc::now()));
            assert_eq!(label, "Syncing… 50 of 271 PRs");
        }

        #[gpui::test]
        fn should_report_a_failed_sync_without_saying_synced(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            harness.api.fail_next("gh is logged out");

            click("sync now".into(), cx);

            assert_eq!(*harness.synced.borrow(), 0);
            assert_eq!(toasts(cx), ["Couldn't sync PRs"]);
        }
    }

    mod logged_out {
        use super::*;

        /// [`prs`] signed out of GitHub.
        fn logged_out() -> Snapshot {
            let mut snapshot = prs();
            pr_sync(&mut snapshot).login = GhLogin::LoggedOut;
            pr_sync(&mut snapshot).last_error = Some("gh is not logged in".into());
            snapshot
        }

        #[gpui::test]
        fn should_show_the_banner_over_the_list_and_say_not_signed_in(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(logged_out(), cx);

            let banner = bounds("logged out banner".into(), cx);
            let table = bounds("pull requests table".into(), cx);
            assert!(banner.bottom() <= table.top(), "{banner:?} over {table:?}");
            let label = harness
                .view
                .read_with(cx, |view, _| view.sync_label(chrono::Utc::now()));
            assert_eq!(label, "Not signed in");
        }

        #[gpui::test]
        fn should_ask_for_settings_from_open_settings(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(logged_out(), cx);

            click("open settings".into(), cx);

            assert_eq!(*harness.settings.borrow(), 1);
        }

        #[gpui::test]
        fn should_show_the_banner_over_the_empty_state_too(cx: &mut TestAppContext) {
            let mut snapshot = logged_out();
            snapshot.prs.clear();
            let (_harness, cx) = Harness::open(snapshot, cx);

            assert!(shows("logged out banner".into(), cx));
            assert!(shows("pull requests empty".into(), cx));
        }

        #[gpui::test]
        fn should_hide_the_banner_once_signed_in(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(logged_out(), cx);

            harness.show(prs(), cx);

            assert!(!shows("logged out banner".into(), cx));
        }
    }

    mod description {
        use super::*;

        #[gpui::test]
        fn should_say_what_it_lists_then_the_cutoff_with_a_settings_link(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);

            let lines = harness.view.read_with(cx, |view, cx| view.description(cx));
            assert_eq!(
                lines,
                [
                    ABOUT,
                    "PRs not updated in the last 90 days are left out; change that in Settings."
                ]
            );
            click("pull-requests settings link".into(), cx);
            assert_eq!(*harness.settings.borrow(), 1);
        }
    }
}
