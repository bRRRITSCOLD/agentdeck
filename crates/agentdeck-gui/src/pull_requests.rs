//! The Pull requests view (spec §11), Penpot "Screens · Main" → "Pull requests": every open
//! pull request you have, each under its own Repository group; and, with [`Scope::Workspace`],
//! a Workspace's Pull requests (Penpot "Workspace · Pull requests"), the same list over the
//! Repositories that Workspace links.
//!
//! Groups go linked Repositories first, then those Not in a workspace, each alphabetically by
//! the Repository's name. A group's header names the Workspaces linking its Repository (archived
//! ones link nothing), each a link to that Workspace's Pull requests ([`OpenWorkspace`]), or
//! reads "Not in a workspace". On the Pull requests view a group with no PR to show is left out;
//! on a Workspace's Pull requests every linked Repository shows, one with none to show over a
//! muted "No open PRs" row. A group collapses at a click, remembered per view and per
//! Repository across restarts in the GUI's own state ([`gui_state`]).
//!
//! Finished pull requests are hidden until **Show finished** is on (Penpot "Pull requests ·
//! show finished"): one preference, kept in the GUI's state too. A Finished row shows GitHub's
//! state with its state icon: merged, closed, or open past the age cutoff.
//!
//! A row's reference (`#63`) opens the PR on GitHub; nothing else on the row reacts to a click,
//! but in Select mode.
//! Its `…` menu has **Open on GitHub**, **Copy PR details** (the three lines
//! [`pr_details`] gives) and, while the PR can close, **Close PR** after a separator, which
//! asks first ([`ClosePrs`]). A PR the GitHub sign-in can't see is dimmed with its tooltip,
//! left out of the sidebar's count and never closed here.
//!
//! **Select** mode (Penpot "Pull requests · select") picks PRs by [`PrKey`]: a row can be picked
//! while its PR is open and the GitHub sign-in can see it, Finished or not. The columns' and
//! each group's checkbox pick or clear their rows, folded or not, and a click on a row toggles
//! it. The selection bar copies the picked PRs' details, a blank line apart, or closes them by
//! reference after asking ([`ClosePrs`]); a PR that didn't close stays picked, marked with why.
//! Select is disabled while there is no PR to show.
//!
//! A Workspace's Pull requests has no sync controls: its header ends in Select, Show finished
//! and **Edit workspace**. With no Repository linked it says so, with **Link repository**,
//! which opens Edit workspace too.
//!
//! The Pull requests view's header ends in Select, Show finished, when PRs last synced and
//! **Sync now**, which runs a full sync and emits
//! [`Synced`] once done, so the window can have the sidebar read again: a sync that changes no
//! PR broadcasts nothing, yet moves the time it last ran. While no GitHub token works, the
//! logged-out banner sits over the list, its **Open Settings** asking for Settings with
//! [`OpenSettings`], as the cutoff line's "Settings" link does.
//!
//! It shows the sidebar's [`Snapshot`], passed on by the window, and draws only the rows in
//! view.

use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use agentdeck_client::types::{GhLogin, PullRequest};
use agentdeck_core::{PrState, RepositoryId, WorkspaceId, pr_details, prs_details};
use brrritscold_core_gpui::{
    ActiveTheme as _, Button, ButtonSize, ButtonVariant, DropdownMenu, DropdownMenuAlign,
    DropdownMenuContent, DropdownMenuItem, DropdownMenuItemVariant, DropdownMenuSeparator,
    IconName, Label, Switch,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, AppContext as _, Context, Entity, EventEmitter,
    InteractiveElement as _, IntoElement, ListOffset, ListState, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, Window, div, list, px,
};

use crate::api::Api;
use crate::close_prs::{self, ClosePrs, Target};
use crate::gui_state;
use crate::open_pr;
use crate::overview::Snapshot;
use crate::pr_table::{self, Linker};
use crate::top_bar::EditWorkspace;
use crate::sync_parts::{self, OpenSettings, SETTINGS_TAIL, cutoff, logged_out_title};
use crate::table::{self, Header, RowParts, SelectCell};
use crate::workspace::failed;

/// How often "Synced N min ago" is redrawn.
const SYNCED_TICK: Duration = Duration::from_secs(30);

/// The description's first line.
const ABOUT: &str = "Your open PRs, by repository.";

/// The description's first line while Show finished is on.
const ABOUT_FINISHED: &str = "Your open and finished PRs, by repository.";

/// How long a row a PR alert's click landed on stays highlighted before it fades.
pub const HIGHLIGHT_HOLD: Duration = Duration::from_secs(2);

/// How long the highlight takes to fade.
pub const HIGHLIGHT_FADE: Duration = Duration::from_millis(400);

/// What the view says with no PR to show (Penpot "Pull requests · empty").
pub const EMPTY: &str = "No open PRs. PRs you open on GitHub show here.";

/// What a Workspace's Pull requests says while the Workspace links no Repository (Penpot
/// "Workspace · no repositories"), over its Link repository button.
pub const NO_REPOSITORIES: [&str; 2] = [
    "No repositories linked yet. Link one to see its PRs here.",
    "They stay in Pull requests too.",
];

/// The row under a linked Repository's group with no PR to show, on a Workspace's Pull
/// requests.
pub const NO_OPEN_PRS: &str = "No open PRs";

/// The second line of a Workspace's Pull requests' description.
const LINK_OR_UNLINK: &str = "Link or unlink repositories in Edit workspace.";

/// Which pull requests a list shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The Pull requests view: every one.
    All,
    /// A Workspace's Pull requests: those of the Repositories it links.
    Workspace(WorkspaceId),
}

/// A Workspace's name in a group header was clicked: show that Workspace's Pull requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenWorkspace(pub WorkspaceId);

/// A pull request, by its Repository's GitHub id and its number: the row a PR alert's click
/// lands on, the same PR through a rename of its Repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PrKey {
    /// Its Repository, by GitHub's id.
    pub repository: RepositoryId,
    /// Its number in the Repository.
    pub number: u32,
}

impl PrKey {
    /// `pr`'s key.
    pub fn of(pr: &PullRequest) -> PrKey {
        PrKey {
            repository: pr.repository.id,
            number: pr.number,
        }
    }
}

/// Whether `pr` can be closed, so picked in Select mode: open on GitHub, as last synced, and
/// seen by the GitHub sign-in. A Finished PR still open can.
pub fn closable(pr: &PullRequest) -> bool {
    pr.state == PrState::Open && !pr.unreachable
}

/// `pr` as the confirm dialog asks about it.
fn target(pr: &PullRequest) -> Target<PrKey> {
    let repo = pr.repository.name.name().to_owned();
    Target {
        id: Some(PrKey::of(pr)),
        pr: pr.reference.clone(),
        reference: format!("{repo} #{}", pr.number).into(),
        repo: repo.into(),
    }
}

/// `pr`'s Copy PR details text.
fn details(pr: &PullRequest) -> String {
    pr_details(
        &pr.reference,
        &pr.title,
        &pr.url,
        &pr.head_ref,
        &pr.base_ref,
    )
}

/// A sync asked for on the Pull requests view has finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Synced;

/// A Repository's group: its PRs to show, by number, and the Workspaces linking it.
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
    /// The Workspaces linking it, archived ones left out, in the sidebar's order; none while
    /// it is Not in a workspace.
    pub workspaces: Vec<Linker>,
}

/// The groups of `snapshot`'s PRs a list over `scope` shows, its PRs by number: one per
/// Repository with a PR to show, and on a Workspace's Pull requests one per Repository it links
/// too, PRs or none. Linked Repositories come first, then those Not in a workspace, each
/// alphabetical by the Repository's name (without the owner, ignoring case, then by owner).
/// Archived Workspaces link nothing. Finished PRs are shown only with `show_finished`, or the
/// one `revealed` by a PR alert's click; merged and closed ones are Finished.
pub fn groups(
    snapshot: &Snapshot,
    scope: Scope,
    show_finished: bool,
    revealed: Option<PrKey>,
) -> Vec<Group> {
    let shown = |pr: &&PullRequest| {
        show_finished
            || (pr.state == PrState::Open && !pr.finished)
            || revealed == Some(PrKey::of(pr))
    };
    let in_scope = |repository: RepositoryId| match scope {
        Scope::All => true,
        Scope::Workspace(workspace) => snapshot.links(workspace, repository),
    };
    let group = |repository, name: &agentdeck_core::RepositoryName| Group {
        repository,
        name: name.to_string().into(),
        label: name.name().to_owned().into(),
        prs: Vec::new(),
        workspaces: Vec::new(),
    };
    let mut groups: Vec<Group> = Vec::new();
    for pr in (snapshot.prs.iter()).filter(shown) {
        if !in_scope(pr.repository.id) {
            continue;
        }
        match groups.iter_mut().find(|g| g.repository == pr.repository.id) {
            Some(group) => group.prs.push(pr.clone()),
            None => groups.push(Group {
                prs: vec![pr.clone()],
                ..group(pr.repository.id, &pr.repository.name)
            }),
        }
    }
    if let Scope::Workspace(_) = scope {
        for links in &snapshot.repositories {
            if in_scope(links.id) && !groups.iter().any(|g| g.repository == links.id) {
                groups.push(group(links.id, &links.name));
            }
        }
    }
    for group in &mut groups {
        group.workspaces = (snapshot.linking(group.repository).into_iter())
            .map(|w| Linker {
                id: w.id,
                name: w.name.clone().into(),
            })
            .collect();
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
    groups.sort_by_key(|g| {
        let unlinked = g.workspaces.is_empty();
        (unlinked, g.label.to_lowercase(), g.name.to_lowercase(), g.repository)
    });
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
        /// Its PRs Select mode can pick, folded or not.
        selectable: Vec<PrKey>,
        /// The Workspaces linking it.
        workspaces: Vec<Linker>,
    },
    /// A PR.
    Row(Box<PullRequest>),
    /// [`NO_OPEN_PRS`], under an open group with no PR to show, by its Repository's name.
    NoOpenPrs(SharedString),
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
            selectable: (group.prs.iter())
                .filter(|pr| closable(pr))
                .map(PrKey::of)
                .collect(),
            workspaces: group.workspaces.clone(),
        });
        if folded {
            continue;
        }
        if group.prs.is_empty() {
            items.push(Item::NoOpenPrs(group.name.clone()));
        }
        items.extend(group.prs.iter().map(|pr| Item::Row(Box::new(pr.clone()))));
    }
    items
}

/// The Pull requests view, or a Workspace's Pull requests.
pub struct PullRequestsView {
    api: Arc<dyn Api>,
    /// Which PRs it shows.
    scope: Scope,
    snapshot: Snapshot,
    /// Whether a sync asked for here is still running.
    syncing: bool,
    /// The items the list draws from, as last rendered.
    shown: Vec<Item>,
    list: ListState,
    /// Select mode, the selection and the Close PRs confirm dialog.
    close: Entity<ClosePrs<PrKey>>,
    _close: Subscription,
    /// The row a PR alert's click landed on: shown even when Finished with Show finished off,
    /// until the view is left or another alert lands.
    revealed: Option<PrKey>,
    /// Whether the list still has to scroll `revealed`'s row into view.
    reveal_pending: bool,
    /// The highlighted row, and whether its highlight is fading.
    highlight: Option<(PrKey, bool)>,
    /// Fades the highlight, then takes it off.
    _highlighting: Option<Task<()>>,
    /// Redraws "Synced N min ago" as time passes.
    _tick: Task<()>,
}

impl EventEmitter<Synced> for PullRequestsView {}
impl EventEmitter<OpenSettings> for PullRequestsView {}
impl EventEmitter<OpenWorkspace> for PullRequestsView {}

impl PullRequestsView {
    /// A view of the PRs in `scope` that syncs and closes PRs through `api`; it shows nothing
    /// until given a snapshot.
    pub fn new(api: Arc<dyn Api>, scope: Scope, cx: &mut Context<Self>) -> Self {
        let id = match scope {
            Scope::All => "pull-requests",
            Scope::Workspace(_) => "workspace-pull-requests",
        };
        let close = cx.new(|_| ClosePrs::new(api.clone(), id));
        let _close = cx.observe(&close, |this, _, cx| {
            this.list.remeasure();
            cx.notify();
        });
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
            scope,
            snapshot: Snapshot::default(),
            syncing: false,
            shown: Vec::new(),
            list: table::list_state(),
            close,
            _close,
            revealed: None,
            reveal_pending: false,
            highlight: None,
            _highlighting: None,
            _tick: tick,
        }
    }

    /// Lands on `pr`'s row, as a PR alert's click asks: opens its group, shows it even when
    /// Finished with Show finished off (the preference stays as it is), scrolls it into view and
    /// highlights it for [`HIGHLIGHT_HOLD`], then fades the highlight over [`HIGHLIGHT_FADE`].
    pub fn reveal(&mut self, pr: PrKey, cx: &mut Context<Self>) {
        let collapsed = gui_state::read(cx, |state| {
            state.collapsed.pull_requests.contains(&pr.repository)
        });
        if collapsed {
            gui_state::update(cx, |state| {
                state.collapsed.set_pull_requests(pr.repository, false);
            });
        }
        self.revealed = Some(pr);
        self.reveal_pending = true;
        self.highlight = Some((pr, false));
        self._highlighting = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HIGHLIGHT_HOLD).await;
            let fading = this.update(cx, |this, cx| {
                this.highlight = this.highlight.map(|(pr, _)| (pr, true));
                cx.notify();
            });
            if fading.is_err() {
                return;
            }
            cx.background_executor().timer(HIGHLIGHT_FADE).await;
            let _ = this.update(cx, |this, cx| {
                this.highlight = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Forgets the row a PR alert's click landed on, once the view is left: a Finished one
    /// hides again while Show finished is off.
    pub fn forget_revealed(&mut self, cx: &mut Context<Self>) {
        if self.revealed.take().is_some() {
            cx.notify();
        }
    }

    /// The highlighted row, while it is.
    #[cfg(test)]
    pub fn highlighted(&self) -> Option<PrKey> {
        self.highlight.map(|(pr, _)| pr)
    }

    /// Scrolls the revealed row into view once the list holds it, waiting a frame after
    /// `relisted` for the list to measure the rows it gained.
    fn scroll_to_revealed(&mut self, relisted: bool, window: &mut Window) {
        if !self.reveal_pending {
            return;
        }
        let Some(revealed) = self.revealed else {
            self.reveal_pending = false;
            return;
        };
        let row = (self.shown.iter()).position(|item| match item {
            Item::Row(pr) => PrKey::of(pr) == revealed,
            Item::Header { .. } | Item::NoOpenPrs(_) => false,
        });
        let Some(ix) = row else {
            // Not read yet: it lands with the next snapshot.
            return;
        };
        if relisted {
            window.request_animation_frame();
            return;
        }
        self.reveal_pending = false;
        let view = self.list.viewport_bounds();
        let in_view = (self.list.bounds_for_item(ix))
            .is_some_and(|row| row.top() >= view.top() && row.bottom() <= view.bottom());
        if !in_view {
            // Rows out of view aren't measured, so `scroll_to_reveal_item` can't place one:
            // put it at the top, under its group's header when that is just above it.
            let header_above = ix > 0 && matches!(self.shown[ix - 1], Item::Header { .. });
            self.list.scroll_to(ListOffset {
                item_ix: if header_above { ix - 1 } else { ix },
                offset_in_item: px(0.),
            });
        }
    }

    /// Shows `snapshot`.
    pub fn set_snapshot(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        let closed: Vec<PrKey> = (snapshot.prs.iter())
            .filter(|pr| pr.state == PrState::Closed)
            .map(PrKey::of)
            .collect();
        self.close
            .update(cx, |close, cx| close.saw_closed(closed, cx));
        self.snapshot = snapshot;
        cx.notify();
    }

    /// The groups shown.
    pub fn groups(&self, cx: &App) -> Vec<Group> {
        groups(&self.snapshot, self.scope, show_finished(cx), self.revealed)
    }

    /// The groups collapsed in this view.
    fn collapsed(&self, cx: &App) -> BTreeSet<RepositoryId> {
        gui_state::read(cx, |state| match self.scope {
            Scope::All => state.collapsed.pull_requests.clone(),
            Scope::Workspace(workspace) => state.collapsed.workspace(workspace),
        })
    }

    /// The Workspace whose Pull requests it shows, by name, as last read.
    fn workspace_name(&self) -> Option<SharedString> {
        let Scope::Workspace(id) = self.scope else {
            return None;
        };
        (self.snapshot.workspaces.iter())
            .find(|w| w.id == id)
            .map(|w| w.name.clone().into())
    }

    /// Whether the list holds a PR row.
    fn has_rows(&self) -> bool {
        self.shown.iter().any(|item| matches!(item, Item::Row(_)))
    }

    /// Select mode, the selection and the Close PRs confirm dialog.
    #[cfg(test)]
    pub fn close_prs(&self) -> &Entity<ClosePrs<PrKey>> {
        &self.close
    }

    /// Collapses `repository`'s group, or opens it, remembering it for the next start.
    pub fn toggle(&mut self, repository: RepositoryId, cx: &mut Context<Self>) {
        let collapsed = self.collapsed(cx).contains(&repository);
        let scope = self.scope;
        gui_state::update(cx, |state| match scope {
            Scope::All => state.collapsed.set_pull_requests(repository, !collapsed),
            Scope::Workspace(workspace) => {
                (state.collapsed).set_workspace(workspace, repository, !collapsed);
            }
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
    /// out, once the PR sync status is read. On a Workspace's Pull requests, which Repositories
    /// it lists and where to change them, or, while it links none, [`NO_REPOSITORIES`].
    pub fn description(&self, cx: &App) -> Vec<SharedString> {
        if let Scope::Workspace(_) = self.scope {
            if self.shown.is_empty() {
                return NO_REPOSITORIES.map(SharedString::new_static).to_vec();
            }
            let name = self.workspace_name().unwrap_or_default();
            let which = if show_finished(cx) {
                "open and finished"
            } else {
                "open"
            };
            return vec![
                format!("Your {which} PRs from the repositories {name} links.").into(),
                LINK_OR_UNLINK.into(),
            ];
        }
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
                selectable,
                workspaces,
            }) => {
                let repository = *repository;
                let select = close_prs::select_all(&self.close, name, selectable.clone(), cx);
                let this = cx.entity().downgrade();
                let on_workspace: pr_table::OnWorkspace = Rc::new(move |workspace, _, cx| {
                    let _ = this.update(cx, |_, cx| cx.emit(OpenWorkspace(workspace)));
                });
                pr_table::group_header(
                    name.clone(),
                    label.clone(),
                    *count,
                    *collapsed,
                    select,
                    workspaces,
                    cx.listener(move |this, _, _, cx| this.toggle(repository, cx)),
                    on_workspace,
                    cx,
                )
            }
            // Under its group's header, inset as a row's state icon is.
            Some(Item::NoOpenPrs(name)) => {
                let name = name.clone();
                div()
                    .debug_selector(move || format!("no open prs {name}"))
                    .flex()
                    .items_center()
                    .h(space::S10)
                    .px(space::S2)
                    .text_size(font_size::SM)
                    .text_color(cx.theme().semantic().muted_foreground)
                    .child(NO_OPEN_PRS)
                    .into_any_element()
            }
            Some(Item::Row(pr)) => {
                let url = pr.url.clone();
                let key = PrKey::of(pr);
                let pickable = closable(pr);
                let (active, closing, error) = {
                    let close = self.close.read(cx);
                    (
                        close.active(),
                        close.is_closing(),
                        close.error(key).cloned(),
                    )
                };
                let parts = RowParts {
                    select: close_prs::select_cell(&self.close, key, pickable, &pr.reference, cx),
                    menu: row_menu(&self.close, pr),
                    error,
                };
                let on_toggle: Option<pr_table::OnToggle> =
                    (active && pickable && !closing).then(|| {
                        let close = self.close.clone();
                        Box::new(move |_: &mut Window, cx: &mut App| {
                            close.update(cx, |close, cx| close.toggle(key, cx));
                        }) as pr_table::OnToggle
                    });
                let row = pr_table::row(
                    pr,
                    parts,
                    move |_, window, cx| open_pr::open(&url, window, cx),
                    on_toggle,
                    cx,
                );
                match self.highlight {
                    Some((key, fading)) if key == PrKey::of(pr) => {
                        highlighted(row, &pr.reference.to_string(), fading, cx)
                    }
                    _ => row,
                }
            }
            None => return div().into_any_element(),
        };
        table::spaced(item)
    }

    /// Penpot `Header`: heading and description, a spacer, Select (disabled with no PR to
    /// show), Show finished, when PRs last synced, "Sync now", `space.3` apart. The heading and
    /// description take what the rest leave, the description wrapping, so the header fits the
    /// narrowest window (spec §11, Window). A Workspace's Pull requests has no heading, since
    /// the top bar names it, and ends in Edit workspace in place of the sync controls; while it
    /// links no Repository, only its description and Link repository.
    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let finished = show_finished(cx);
        let linked = !self.shown.is_empty();
        let lines: Vec<AnyElement> = (self.description(cx).into_iter().enumerate())
            .map(|(ix, line)| {
                let line_div = div()
                    .debug_selector(move || format!("pull requests description {ix}"))
                    .text_size(font_size::SM)
                    .text_color(s.muted_foreground);
                match line.strip_suffix(SETTINGS_TAIL) {
                    // The lead wraps in what the header leaves it, "Settings." after its last
                    // line.
                    Some(lead) => line_div
                        .flex()
                        .items_end()
                        .child(
                            div()
                                .debug_selector(move || {
                                    format!("pull requests description {ix} lead")
                                })
                                .min_w_0()
                                .child(SharedString::from(lead.to_owned())),
                        )
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
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(space::S1)
                    .when(self.scope == Scope::All, |heading| {
                        heading.child(
                            div()
                                .text_size(font_size::XL)
                                .font_weight(font_weight::MEDIUM)
                                .text_color(s.foreground)
                                .child("Pull requests"),
                        )
                    })
                    // Penpot's `Description` is one text of two lines: no gap between them.
                    .child(div().flex().flex_col().children(lines)),
            )
            .when(self.scope == Scope::All || linked, |header| {
                header
                    .child(close_prs::select_toggle(&self.close, self.has_rows(), cx))
                    .child(show_finished_switch(finished, cx))
            })
            .map(|header| match self.scope {
                Scope::All => header.children(self.sync_controls(cx)),
                Scope::Workspace(_) if linked => header.child(edit_workspace(
                    "edit workspace",
                    "Edit workspace",
                )),
                Scope::Workspace(_) => {
                    header.child(edit_workspace("link repository", "Link repository"))
                }
            })
    }

    /// When PRs last synced and "Sync now", on the Pull requests view's header.
    fn sync_controls(&self, cx: &mut Context<Self>) -> [AnyElement; 2] {
        let s = cx.theme().semantic();
        let synced = self.sync_label(chrono::Utc::now());
        [
            div()
                .debug_selector(|| "pull requests synced".into())
                .flex_none()
                .whitespace_nowrap()
                .text_size(font_size::SM)
                .text_color(s.muted_foreground)
                .child(synced)
                .into_any_element(),
            div()
                .flex_none()
                .debug_selector(|| "sync now".into())
                .child(
                    Button::new("sync-now")
                        .variant(ButtonVariant::Outline)
                        .label("Sync now")
                        .disabled(self.syncing)
                        .on_click(cx.listener(|this, _, window, cx| this.sync(window, cx))),
                )
                .into_any_element(),
        ]
    }
}

/// Penpot `ShowFinished`: a Switch and its Label, `space.2` apart, `finished` or not; flipping
/// it turns Show finished on or off for every PR list.
fn show_finished_switch(finished: bool, cx: &mut Context<PullRequestsView>) -> impl IntoElement {
    let toggle = cx.listener(|this, checked: &bool, _, cx| this.set_show_finished(*checked, cx));
    div()
                    .flex_none()
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
        .child(Label::new().child("Show finished").whitespace_nowrap())
}

/// An outline button reading `label` that opens Edit workspace on the Workspace shown, as its
/// top bar's menu does; tests find it as `name`.
fn edit_workspace(name: &'static str, label: &'static str) -> impl IntoElement {
    div().flex_none().debug_selector(move || name.into()).child(
        Button::new(name)
            .variant(ButtonVariant::Outline)
            .label(label)
            .on_click(|_, window, cx| window.dispatch_action(Box::new(EditWorkspace), cx)),
    )
}

/// `row` highlighted after a PR alert's click (Penpot "Pull requests · highlighted row"): an
/// `accent` fill behind it, at its `radius-md`, fading out over [`HIGHLIGHT_FADE`] once
/// `fading`. Tests find it as `highlight <reference>`.
fn highlighted(row: AnyElement, reference: &str, fading: bool, cx: &App) -> AnyElement {
    let s = cx.theme().semantic();
    let accent = s.accent;
    let selector = reference.to_owned();
    let highlight = div()
        .debug_selector(move || format!("highlight {selector}"))
        .rounded(s.radius_md)
        .bg(accent)
        .child(row);
    if !fading {
        return highlight.into_any_element();
    }
    highlight
        .with_animation(
            SharedString::from(format!("highlight-fade-{reference}")),
            Animation::new(HIGHLIGHT_FADE),
            move |highlight, delta| highlight.bg(accent.opacity(1. - delta)),
        )
        .into_any_element()
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
    if closable(pr) {
        entries.extend([MenuEntry::Separator, MenuEntry::ClosePr]);
    }
    entries
}

/// `pr`'s `…` menu of its [`menu_entries`], Close PR in `destructive`. Tests find it as
/// `pr menu <reference>`.
fn row_menu(close: &Entity<ClosePrs<PrKey>>, pr: &PullRequest) -> AnyElement {
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
                let details = details(pr);
                content.item(DropdownMenuItem::new("Copy PR details").on_click(
                    move |_, window, cx| {
                        crate::notice::copy(details.clone(), "Copied PR details", window, cx);
                    },
                ))
            }
            MenuEntry::Separator => content.separator(DropdownMenuSeparator::new()),
            MenuEntry::ClosePr => {
                let target = target(pr);
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let groups = self.groups(cx);
        let collapsed = self.collapsed(cx);
        let items = items(&groups, &collapsed);
        let relisted = items != self.shown;
        // Every PR Select mode can pick, folded or not, in the list's order.
        let pickable: Vec<&PullRequest> = (groups.iter())
            .flat_map(|group| &group.prs)
            .filter(|pr| closable(pr))
            .collect();
        let keys: Vec<PrKey> = pickable.iter().map(|pr| PrKey::of(pr)).collect();
        if relisted {
            table::relist(&self.list, items.len());
            self.shown = items;
            self.close.update(cx, |close, cx| close.retain(&keys, cx));
        }
        // With no PR to show, Select is disabled, so Select mode ends.
        if !self.has_rows() && self.close.read(cx).active() {
            self.close
                .update(cx, |close, cx| close.set_active(false, cx));
        }
        let select = self.close.read(cx).active();
        let bar = {
            let picked: Vec<&PullRequest> = {
                let close = self.close.read(cx);
                (pickable.iter().copied())
                    .filter(|pr| close.picked(PrKey::of(pr)))
                    .collect()
            };
            let copy = prs_details(picked.iter().map(|pr| details(pr)));
            let targets = picked.into_iter().map(target).collect();
            close_prs::selection_bar(&self.close, targets, Some(copy), cx)
        };
        let select_all = close_prs::select_all(&self.close, "all", keys, cx);
        self.scroll_to_revealed(relisted, window);
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
                if self.shown.is_empty() && self.scope != Scope::All {
                    // Penpot "Workspace · no repositories": the header says it all.
                    return content;
                }
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
                let ghost = if select {
                    SelectCell::Space
                } else {
                    SelectCell::Off
                };
                let header = Header {
                    columns: pr_table::columns(select_all, cx),
                    ghost: pr_table::columns(ghost, cx),
                    min_width: pr_table::min_width(select),
                };
                content.child(table::table_of(
                    "pull-requests-table",
                    "pull requests table",
                    header,
                    rows,
                    &self.list,
                    |_, _, _| {},
                    cx,
                ))
            })
            .children(bar)
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
    use crate::overview::fixtures::{
        id, links, pr_status, pr_sync, pull_request, repository_id, workspace,
    };

    struct Harness {
        api: Arc<FakeApi>,
        view: Entity<PullRequestsView>,
        synced: Rc<RefCell<usize>>,
        settings: Rc<RefCell<usize>>,
        opened: Rc<RefCell<Vec<WorkspaceId>>>,
        _events: [gpui::Subscription; 3],
    }

    impl Harness {
        /// A `Root` window on the Pull requests view showing `snapshot`, over a fake daemon
        /// holding it.
        fn open(snapshot: Snapshot, cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            Harness::open_in(Scope::All, snapshot, cx)
        }

        /// A `Root` window on a view of `scope` showing `snapshot`, over a fake daemon holding
        /// it.
        fn open_in(
            scope: Scope,
            snapshot: Snapshot,
            cx: &mut TestAppContext,
        ) -> (Harness, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            let api = Arc::new(FakeApi::with(snapshot.clone()));
            let mut view = None;
            let window = cx.add_window(|window, cx| {
                let api: Arc<dyn Api> = api.clone();
                let prs = cx.new(|cx| {
                    let mut prs = PullRequestsView::new(api, scope, cx);
                    prs.set_snapshot(snapshot, cx);
                    prs
                });
                view = Some(prs.clone());
                Root::new(prs, window, cx)
            });
            let cx = VisualTestContext::from_window(*window, cx).into_mut();
            let view = view.unwrap();
            let (synced, settings) = (Rc::new(RefCell::new(0)), Rc::new(RefCell::new(0)));
            let opened = Rc::new(RefCell::new(Vec::new()));
            let (seen_sync, seen_settings) = (synced.clone(), settings.clone());
            let seen_opened = opened.clone();
            let events = cx.update(|_, cx| {
                [
                    cx.subscribe(&view, move |_, _: &Synced, _| *seen_sync.borrow_mut() += 1),
                    cx.subscribe(&view, move |_, _: &OpenSettings, _| {
                        *seen_settings.borrow_mut() += 1;
                    }),
                    cx.subscribe(&view, move |_, OpenWorkspace(id): &OpenWorkspace, _| {
                        seen_opened.borrow_mut().push(*id);
                    }),
                ]
            });
            cx.run_until_parked();
            let harness = Harness {
                api,
                view,
                synced,
                settings,
                opened,
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

    /// A snapshot holding `prs` alone.
    fn of(prs: &[PullRequest]) -> Snapshot {
        Snapshot {
            prs: prs.to_vec(),
            ..Snapshot::default()
        }
    }

    /// Workspaces `a` (1) and `b` (2), and `old` (3), archived. `a` and `b` link widgets, `a`
    /// links tools too, which has no PR, and only `old` links gadgets. apples and zebra are
    /// linked by none. Each of widgets, gadgets, apples and zebra has one open PR.
    fn linked() -> Snapshot {
        let old = agentdeck_client::types::Workspace {
            archived_at: Some("2026-11-13T00:00:00Z".into()),
            ..workspace(3, "old")
        };
        Snapshot {
            workspaces: vec![workspace(1, "a"), workspace(2, "b"), old],
            repositories: vec![
                links("acme/widgets", &[id(1), id(2)]),
                links("acme/tools", &[id(1)]),
                links("acme/gadgets", &[id(3)]),
            ],
            prs: vec![
                pull_request(1, "acme/zebra"),
                pull_request(2, "acme/gadgets"),
                pull_request(3, "acme/widgets"),
                pull_request(4, "acme/apples"),
            ],
            pr_sync: Some(pr_status()),
            ..Snapshot::default()
        }
    }

    /// Workspace `a`'s Pull requests.
    fn a() -> Scope {
        Scope::Workspace(id(1))
    }

    /// Whether the window dispatches [`EditWorkspace`] from now on, as the top bar's menu does.
    fn edit_workspace_asked(cx: &mut VisualTestContext) -> Rc<RefCell<usize>> {
        let asked = Rc::new(RefCell::new(0));
        let seen = asked.clone();
        cx.update(|_, cx| {
            cx.on_action(move |_: &EditWorkspace, _| *seen.borrow_mut() += 1);
        });
        asked
    }

    mod linked {
        use super::*;

        fn linkers(group: &Group) -> Vec<String> {
            group.workspaces.iter().map(|w| w.name.to_string()).collect()
        }

        #[test]
        fn should_put_linked_repositories_first_then_the_rest_each_alphabetical() {
            let groups = groups(&linked(), Scope::All, false, None);

            assert_eq!(names(&groups), ["widgets", "apples", "gadgets", "zebra"]);
        }

        #[test]
        fn should_name_the_workspaces_linking_each_repository_in_the_sidebars_order() {
            let groups = groups(&linked(), Scope::All, false, None);

            assert_eq!(linkers(&groups[0]), ["a", "b"]);
            assert!(linkers(&groups[1]).is_empty());
        }

        #[test]
        fn should_count_archived_workspaces_as_linking_nothing() {
            let groups = groups(&linked(), Scope::All, false, None);

            let gadgets = groups.iter().find(|g| g.label == "gadgets").unwrap();
            assert!(linkers(gadgets).is_empty());
            assert_eq!(names(&groups)[1..], ["apples", "gadgets", "zebra"]);
        }

        #[test]
        fn should_list_a_workspaces_linked_repositories_alone_prs_or_none() {
            let groups = groups(&linked(), a(), false, None);

            assert_eq!(names(&groups), ["tools", "widgets"]);
            assert!(groups[0].prs.is_empty());
            assert_eq!(groups[1].prs.len(), 1);
            assert_eq!(linkers(&groups[1]), ["a", "b"]);
        }

        #[test]
        fn should_list_nothing_for_a_workspace_linking_nothing() {
            assert!(groups(&linked(), Scope::Workspace(id(9)), false, None).is_empty());
            assert!(groups(&linked(), Scope::Workspace(id(3)), false, None).is_empty());
        }

        #[gpui::test]
        fn should_name_each_linking_workspace_in_its_groups_header(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(linked(), cx);

            let header = bounds("group acme/widgets".into(), cx);
            let a = bounds("workspace link acme/widgets a".into(), cx);
            let b = bounds("workspace link acme/widgets b".into(), cx);
            assert!(a.left() > header.right() && b.left() > a.right(), "{a:?} {b:?}");
            assert!(shows("group workspaces acme/gadgets".into(), cx));
            assert!(!shows("workspace link acme/gadgets old".into(), cx));
        }

        #[gpui::test]
        fn should_open_a_workspaces_pull_requests_from_its_name(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(linked(), cx);

            click("workspace link acme/widgets b".into(), cx);

            assert_eq!(*harness.opened.borrow(), [id(2)]);
            assert!(shows("pr acme/widgets#3".into(), cx), "the group stays open");
        }

        #[gpui::test]
        fn should_draw_the_groups_in_their_order(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(linked(), cx);

            let tops: Vec<_> = ["widgets", "apples", "gadgets", "zebra"]
                .map(|name| bounds(format!("group acme/{name}"), cx).top())
                .to_vec();

            assert!(tops.is_sorted(), "{tops:?}");
        }
    }

    mod workspace_side {
        use super::*;

        #[gpui::test]
        fn should_list_the_prs_of_the_repositories_it_links(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open_in(a(), linked(), cx);

            assert!(shows("pr acme/widgets#3".into(), cx));
            for other in ["pr acme/zebra#1", "pr acme/gadgets#2", "pr acme/apples#4"] {
                assert!(!shows(other.into(), cx), "{other}");
            }
        }

        #[gpui::test]
        fn should_show_a_linked_repository_with_no_open_pr_over_a_muted_row(
            cx: &mut TestAppContext,
        ) {
            let (_harness, cx) = Harness::open_in(a(), linked(), cx);

            let header = bounds("group acme/tools".into(), cx);
            let row = bounds("no open prs acme/tools".into(), cx);
            let next = bounds("group acme/widgets".into(), cx);
            assert!(header.bottom() <= row.top() && row.bottom() <= next.top());
            assert_eq!(NO_OPEN_PRS, "No open PRs");
        }

        #[gpui::test]
        fn should_show_no_open_prs_once_its_last_pr_finishes(cx: &mut TestAppContext) {
            let mut snapshot = linked();
            snapshot.prs[2].finished = true;
            snapshot.prs[2].state = PrState::Merged;
            let (_harness, cx) = Harness::open_in(a(), snapshot, cx);

            assert!(shows("no open prs acme/widgets".into(), cx));
            click("show finished".into(), cx);
            assert!(!shows("no open prs acme/widgets".into(), cx));
            assert!(shows("pr acme/widgets#3".into(), cx));
        }

        #[gpui::test]
        fn should_head_it_with_select_show_finished_and_edit_workspace(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open_in(a(), linked(), cx);

            let tops = ["workspace-pull-requests select", "show finished", "edit workspace"]
                .map(|selector| bounds(selector.into(), cx));
            assert!(tops.windows(2).all(|w| w[0].right() <= w[1].left()), "{tops:?}");
            assert!(!shows("sync now".into(), cx));
            assert!(!shows("pull requests synced".into(), cx));
        }

        #[gpui::test]
        fn should_say_which_repositories_it_lists(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open_in(a(), linked(), cx);

            let lines = harness.view.read_with(cx, |view, cx| view.description(cx));

            assert_eq!(
                lines,
                [
                    "Your open PRs from the repositories a links.",
                    "Link or unlink repositories in Edit workspace."
                ]
            );
        }

        #[gpui::test]
        fn should_open_edit_workspace_from_its_header(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open_in(a(), linked(), cx);
            let asked = edit_workspace_asked(cx);

            click("edit workspace".into(), cx);

            assert_eq!(*asked.borrow(), 1);
        }

        #[gpui::test]
        fn should_pick_and_copy_its_prs_in_select_mode(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open_in(a(), linked(), cx);

            click("workspace-pull-requests select".into(), cx);
            click("pr title acme/widgets#3".into(), cx);

            let picked = harness.view.read_with(cx, |view, cx| {
                view.close_prs().read(cx).picked(PrKey::of(&linked().prs[2]))
            });
            assert!(picked);
        }

        #[gpui::test]
        fn should_show_the_logged_out_banner_over_it(cx: &mut TestAppContext) {
            let mut snapshot = linked();
            pr_sync(&mut snapshot).login = GhLogin::LoggedOut;
            let (_harness, cx) = Harness::open_in(a(), snapshot, cx);

            let banner = bounds("logged out banner".into(), cx);
            let table = bounds("pull requests table".into(), cx);
            assert!(banner.bottom() <= table.top(), "{banner:?} over {table:?}");
        }

        #[gpui::test]
        fn should_remember_collapse_apart_from_the_pull_requests_view(cx: &mut TestAppContext) {
            let (_harness, window) = Harness::open_in(a(), linked(), cx);

            click("group acme/widgets".into(), window);

            assert!(!shows("pr acme/widgets#3".into(), window));
            let state = window.read(|cx| gui_state::read(cx, Clone::clone));
            assert_eq!(
                state.collapsed.workspace(id(1)),
                BTreeSet::from([repository_id("acme/widgets")])
            );
            assert!(state.collapsed.pull_requests.is_empty());
            let (_harness, cx) = Harness::open(linked(), cx);
            assert!(shows("pr acme/widgets#3".into(), cx));
        }

        mod no_repositories {
            use super::*;

            /// Workspace `c` (4), linking nothing, beside [`linked`]'s.
            fn c() -> (Scope, Snapshot) {
                let mut snapshot = linked();
                snapshot.workspaces.push(workspace(4, "c"));
                (Scope::Workspace(id(4)), snapshot)
            }

            #[gpui::test]
            fn should_say_it_links_none_with_link_repository(cx: &mut TestAppContext) {
                let (scope, snapshot) = c();
                let (harness, cx) = Harness::open_in(scope, snapshot, cx);

                let lines = harness.view.read_with(cx, |view, cx| view.description(cx));
                assert_eq!(lines, NO_REPOSITORIES);
                assert!(shows("link repository".into(), cx));
                for gone in [
                    "pull requests table",
                    "workspace-pull-requests select",
                    "show finished",
                    "edit workspace",
                ] {
                    assert!(!shows(gone.into(), cx), "{gone}");
                }
            }

            #[gpui::test]
            fn should_open_edit_workspace_from_link_repository(cx: &mut TestAppContext) {
                let (scope, snapshot) = c();
                let (_harness, cx) = Harness::open_in(scope, snapshot, cx);
                let asked = edit_workspace_asked(cx);

                click("link repository".into(), cx);

                assert_eq!(*asked.borrow(), 1);
            }
        }
    }

    mod groups {
        use super::*;

        #[test]
        fn should_put_each_pr_under_its_repository_alphabetically_by_name() {
            let groups = groups(&prs(), Scope::All, false, None);

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

            assert_eq!(
                names(&groups(&of(&prs), Scope::All, false, None)),
                ["alpha", "Beta", "gamma"]
            );
        }

        #[test]
        fn should_leave_out_a_repository_with_no_open_pr() {
            let groups = groups(&prs(), Scope::All, false, None);

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

            assert_eq!(numbers(groups(&of(&prs), Scope::All, false, None)), [1]);
            assert_eq!(numbers(groups(&of(&prs), Scope::All, true, None)), [3, 1, 2, 5]);
        }

        #[test]
        fn should_name_groups_that_share_a_name_by_owner_too() {
            let prs = [
                pull_request(1, "me/agentdeck"),
                pull_request(2, "upstream/agentdeck"),
            ];

            assert_eq!(
                names(&groups(&of(&prs), Scope::All, false, None)),
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

            assert_eq!(groups(&of(&prs), Scope::All, false, None)[0].prs.len(), 2);
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

    mod narrowest_window {
        use gpui::size;

        use super::*;

        /// The view at the width the narrowest window leaves it beside the sidebar, in Select
        /// mode, its widest.
        fn narrowest(cx: &mut TestAppContext) -> &mut VisualTestContext {
            let (_harness, cx) = Harness::open(prs(), cx);
            let (width, height) = crate::window_state::MIN_SIZE;
            let width = px(width) - crate::sidebar::WIDTH - px(1.);
            cx.simulate_resize(size(width, px(height)));
            click("pull-requests select".into(), cx);
            cx
        }

        #[gpui::test]
        fn should_fit_the_header_whole(cx: &mut TestAppContext) {
            let cx = narrowest(cx);
            let width = cx.update(|window, _| window.viewport_size().width);

            let description = bounds("pull requests description 1".into(), cx);
            let select = bounds("pull-requests select".into(), cx);
            let synced = bounds("pull requests synced".into(), cx);
            let sync_now = bounds("sync now".into(), cx);

            let lead = bounds("pull requests description 1 lead".into(), cx);
            assert!(
                description.right() <= select.left(),
                "{description:?} {select:?}"
            );
            assert!(lead.right() <= select.left(), "{lead:?} {select:?}");
            assert!(synced.right() < sync_now.left(), "{synced:?} {sync_now:?}");
            assert!(
                sync_now.right() <= width - space::S6,
                "{sync_now:?} in {width:?}"
            );
        }

        #[gpui::test]
        fn should_fit_the_table_whole(cx: &mut TestAppContext) {
            let cx = narrowest(cx);
            let width = cx.update(|window, _| window.viewport_size().width);

            let review = bounds("pr column Review".into(), cx);
            let table = bounds("pull requests table".into(), cx);

            assert!(table.right() <= width - space::S6, "{table:?} in {width:?}");
            assert!(review.right() <= table.right(), "{review:?} in {table:?}");
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

    /// Select mode (spec §11, Pull requests view → Select mode).
    mod select {
        use super::*;

        fn key(n: u32, repo: &str) -> PrKey {
            PrKey {
                repository: repository_id(repo),
                number: n,
            }
        }

        fn picked(harness: &Harness, cx: &mut VisualTestContext) -> Vec<PrKey> {
            harness.view.read_with(cx, |view, cx| {
                view.close_prs().read(cx).selection().ids().to_vec()
            })
        }

        fn selecting(harness: &Harness, cx: &mut VisualTestContext) -> bool {
            harness
                .view
                .read_with(cx, |view, cx| view.close_prs().read(cx).active())
        }

        fn bar_label(harness: &Harness, cx: &mut VisualTestContext) -> String {
            harness
                .view
                .read_with(cx, |view, cx| view.close_prs().read(cx).bar_label())
        }

        /// [`prs`] with `acme/widgets` #5, open but Finished (past the age cutoff), and
        /// `acme/widgets` #6, open but out of the GitHub sign-in's sight.
        fn mixed() -> Snapshot {
            let mut snapshot = prs();
            snapshot.prs.extend([
                PullRequest {
                    finished: true,
                    ..pull_request(5, "acme/widgets")
                },
                PullRequest {
                    unreachable: true,
                    ..pull_request(6, "acme/widgets")
                },
            ]);
            snapshot
        }

        #[gpui::test]
        fn should_enter_select_mode_from_the_header_and_leave_it_from_cancel(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(prs(), cx);
            assert!(!shows("pull-requests selection bar".into(), cx));
            assert!(!shows("pull-requests select acme/widgets#1".into(), cx));

            click("pull-requests select".into(), cx);

            assert!(selecting(&harness, cx));
            assert!(shows("pull-requests selection bar".into(), cx));
            assert!(shows("pull-requests select acme/widgets#1".into(), cx));

            click("pull-requests select acme/widgets#1".into(), cx);
            click("pull-requests cancel select".into(), cx);

            assert!(!selecting(&harness, cx));
            assert!(picked(&harness, cx).is_empty());
            assert!(!shows("pull-requests selection bar".into(), cx));
        }

        #[gpui::test]
        fn should_put_select_before_show_finished(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(prs(), cx);

            let select = bounds("pull-requests select".into(), cx);
            let finished = bounds("show finished".into(), cx);

            assert!(
                select.right() < finished.left(),
                "{select:?} before {finished:?}"
            );
        }

        #[gpui::test]
        fn should_leave_select_mode_when_select_is_pressed_again(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            click("pull-requests select".into(), cx);
            click("pull-requests select acme/widgets#1".into(), cx);

            click("pull-requests select".into(), cx);

            assert!(!selecting(&harness, cx));
            assert!(picked(&harness, cx).is_empty());
        }

        #[gpui::test]
        fn should_toggle_a_row_on_a_click_on_it(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            click("pull-requests select".into(), cx);

            click("pr title acme/widgets#4".into(), cx);
            assert_eq!(picked(&harness, cx), [key(4, "acme/widgets")]);

            let row = bounds("pr acme/widgets#4".into(), cx);
            cx.simulate_click(
                point(row.left() + px(400.), row.center().y),
                Modifiers::default(),
            );
            cx.run_until_parked();
            assert!(picked(&harness, cx).is_empty());
        }

        #[gpui::test]
        fn should_open_the_pr_from_its_reference_without_toggling_it(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            let opener = cx.update(|_, cx| crate::terminal::open::fake::install(cx));
            click("pull-requests select".into(), cx);

            click("pr link acme/widgets#4".into(), cx);

            assert_eq!(opener.started.lock().unwrap().len(), 1);
            assert!(picked(&harness, cx).is_empty());
        }

        #[gpui::test]
        fn should_not_toggle_a_row_from_its_menu(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            click("pull-requests select".into(), cx);

            click("pr menu acme/widgets#4".into(), cx);

            assert!(picked(&harness, cx).is_empty());
        }

        #[gpui::test]
        fn should_not_toggle_a_row_out_of_select_mode(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);

            click("pr title acme/widgets#4".into(), cx);

            assert!(picked(&harness, cx).is_empty());
            assert!(!selecting(&harness, cx));
        }

        #[gpui::test]
        fn should_offer_open_and_reachable_prs_finished_ones_included(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(mixed(), cx);
            click("show finished".into(), cx);
            click("pull-requests select".into(), cx);

            for selectable in ["acme/widgets#1", "acme/widgets#5"] {
                assert!(
                    shows(format!("pull-requests select {selectable}"), cx),
                    "{selectable}"
                );
            }
            for not in ["acme/gizmos#3", "acme/widgets#6"] {
                assert!(!shows(format!("pull-requests select {not}"), cx), "{not}");
            }
            click("pr title acme/widgets#6".into(), cx);
            click("pr title acme/gizmos#3".into(), cx);
            click("pr title acme/widgets#5".into(), cx);
            assert_eq!(picked(&harness, cx), [key(5, "acme/widgets")]);
        }

        #[gpui::test]
        fn should_not_offer_a_closed_pr(cx: &mut TestAppContext) {
            let mut snapshot = prs();
            snapshot.prs.push(PullRequest {
                state: PrState::Closed,
                finished: true,
                ..pull_request(7, "acme/widgets")
            });
            let (_harness, cx) = Harness::open(snapshot, cx);
            click("show finished".into(), cx);
            click("pull-requests select".into(), cx);

            assert!(shows("pr acme/widgets#7".into(), cx));
            assert!(!shows("pull-requests select acme/widgets#7".into(), cx));
        }

        #[gpui::test]
        fn should_keep_titles_aligned_on_rows_with_nothing_to_select(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(mixed(), cx);
            click("pull-requests select".into(), cx);

            let left =
                |pr: &str, cx: &mut VisualTestContext| bounds(format!("pr title {pr}"), cx).left();
            assert_eq!(left("acme/widgets#1", cx), left("acme/widgets#6", cx));
        }

        #[gpui::test]
        fn should_pick_a_groups_rows_from_its_checkbox_then_none(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(mixed(), cx);
            click("pull-requests select".into(), cx);

            click("pull-requests select all acme/widgets".into(), cx);
            assert_eq!(
                picked(&harness, cx),
                [key(1, "acme/widgets"), key(4, "acme/widgets")]
            );

            click("pull-requests select all acme/widgets".into(), cx);
            assert!(picked(&harness, cx).is_empty());
        }

        #[gpui::test]
        fn should_pick_a_folded_groups_rows_from_its_checkbox(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            click("group acme/widgets".into(), cx);
            click("pull-requests select".into(), cx);

            click("pull-requests select all acme/widgets".into(), cx);

            assert_eq!(
                picked(&harness, cx),
                [key(1, "acme/widgets"), key(4, "acme/widgets")]
            );
            assert!(shows("group acme/widgets".into(), cx));
            assert!(!shows("pr acme/widgets#1".into(), cx));
        }

        #[gpui::test]
        fn should_pick_every_row_from_select_all_then_none(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(mixed(), cx);
            click("pull-requests select".into(), cx);
            click("pr title acme/widgets#4".into(), cx);

            click("pull-requests select all all".into(), cx);
            assert_eq!(
                picked(&harness, cx),
                [
                    key(4, "acme/widgets"),
                    key(2, "acme/gadgets"),
                    key(1, "acme/widgets")
                ]
            );

            click("pull-requests select all all".into(), cx);
            assert!(picked(&harness, cx).is_empty());
        }

        #[gpui::test]
        fn should_count_the_picked_prs_in_the_bar(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            click("pull-requests select".into(), cx);
            assert_eq!(bar_label(&harness, cx), "0 selected");

            click("pr title acme/widgets#4".into(), cx);
            click("pr title acme/gadgets#2".into(), cx);

            assert_eq!(bar_label(&harness, cx), "2 selected");
        }

        #[gpui::test]
        fn should_read_n_selected_copy_close_then_cancel(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(prs(), cx);
            click("pull-requests select".into(), cx);

            let lefts: Vec<_> = [
                "pull-requests selection count",
                "pull-requests copy prs",
                "pull-requests close prs",
                "pull-requests cancel select",
            ]
            .into_iter()
            .map(|selector| bounds(selector.into(), cx).left())
            .collect();

            assert!(lefts.is_sorted(), "{lefts:?}");
            let table = bounds("pull requests table".into(), cx);
            let bar = bounds("pull-requests selection bar".into(), cx);
            assert!(bar.top() >= table.bottom(), "{bar:?} under {table:?}");
        }

        #[gpui::test]
        fn should_copy_the_picked_prs_details_a_blank_line_apart(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(prs(), cx);
            click("pull-requests select".into(), cx);
            click("pr title acme/widgets#4".into(), cx);
            click("pr title acme/gadgets#2".into(), cx);

            click("pull-requests copy prs".into(), cx);

            // In the list's order, whatever the order picked.
            let copied = cx.read_from_clipboard().and_then(|item| item.text());
            assert_eq!(
                copied.as_deref(),
                Some(
                    "acme/gadgets#2: PR 2\n\
                     https://github.com/acme/gadgets/pull/2\n\
                     feat/x → main\n\
                     \n\
                     acme/widgets#4: PR 4\n\
                     https://github.com/acme/widgets/pull/4\n\
                     feat/x → main"
                )
            );
            assert_eq!(toasts(cx), ["Copied PR details"]);
        }

        #[gpui::test]
        fn should_copy_nothing_with_nothing_picked(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(prs(), cx);
            click("pull-requests select".into(), cx);

            click("pull-requests copy prs".into(), cx);

            assert!(cx.read_from_clipboard().is_none());
            assert!(toasts(cx).is_empty());
        }

        /// Select mode on, `acme/widgets` #1 and #4 picked, and Close PRs pressed.
        fn asked_to_close_widgets(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            click("pull-requests select".into(), cx);
            click("pull-requests select all acme/widgets".into(), cx);
            click("pull-requests close prs".into(), cx);
            (harness, cx)
        }

        #[gpui::test]
        fn should_ask_then_close_the_picked_prs_by_reference(cx: &mut TestAppContext) {
            let (harness, cx) = asked_to_close_widgets(cx);
            assert!(harness.asking(cx));
            assert!(harness.api.closes.lock().unwrap().is_empty());

            cx.simulate_keystrokes("enter");
            cx.run_until_parked();

            assert_eq!(
                *harness.api.closes.lock().unwrap(),
                [vec![pr_ref("acme/widgets#1"), pr_ref("acme/widgets#4")]]
            );
            assert_eq!(toasts(cx).last().map(String::as_str), Some("Closed 2 PRs"));
            assert!(!selecting(&harness, cx));
        }

        #[gpui::test]
        fn should_close_nothing_when_the_dialog_is_cancelled(cx: &mut TestAppContext) {
            let (harness, cx) = asked_to_close_widgets(cx);

            cx.simulate_keystrokes("escape");
            cx.run_until_parked();

            assert!(harness.api.closes.lock().unwrap().is_empty());
            assert!(selecting(&harness, cx));
            assert_eq!(picked(&harness, cx).len(), 2);
        }

        #[gpui::test]
        fn should_keep_a_failed_pr_picked_and_mark_it_with_why(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            (harness.api).refuse_close_pr(&pr_ref("acme/widgets#4"), "Must have admin rights");
            click("pull-requests select".into(), cx);
            click("pull-requests select all acme/widgets".into(), cx);
            click("pull-requests close prs".into(), cx);

            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
            harness.show(harness.api.snapshot_now(), cx);

            let shown = cx.update(|_, cx| crate::notice::shown(cx));
            let toast = shown.last().expect("a toast");
            assert_eq!(toast.title, "Closed 1 of 2");
            assert_eq!(
                toast.description.as_deref(),
                Some("Hover the marked PR to see why.")
            );
            assert!(selecting(&harness, cx));
            assert_eq!(picked(&harness, cx), [key(4, "acme/widgets")]);
            assert!(shows("pr close error acme/widgets#4".into(), cx));
            assert_eq!(
                harness.view.read_with(cx, |view, cx| {
                    view.close_prs()
                        .read(cx)
                        .error(key(4, "acme/widgets"))
                        .cloned()
                }),
                Some("Must have admin rights".into())
            );

            let title = bounds("pr title acme/widgets#4".into(), cx);
            cx.simulate_mouse_move(title.center(), None, Modifiers::default());
            cx.run_until_parked();
            assert!(cx.debug_bounds("tooltip-content").is_some());
        }

        #[gpui::test]
        fn should_show_a_pr_it_closed_as_closed_at_once(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            click("show finished".into(), cx);
            click("pull-requests select".into(), cx);
            click("pr title acme/widgets#4".into(), cx);
            click("pull-requests close prs".into(), cx);
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();

            // The close's `changed` reads the snapshot again: stored closed, so Finished.
            harness.show(harness.api.snapshot_now(), cx);

            let groups = harness.view.read_with(cx, |view, cx| view.groups(cx));
            let closed = (groups.iter().flat_map(|g| &g.prs))
                .find(|pr| pr.number == 4)
                .expect("#4, shown under Show finished");
            assert_eq!(closed.state, PrState::Closed);
            assert!(closed.finished);
            click("pull-requests select".into(), cx);
            assert!(!shows("pull-requests select acme/widgets#4".into(), cx));

            click("show finished".into(), cx);
            assert!(!shows("pr acme/widgets#4".into(), cx));
        }

        #[gpui::test]
        fn should_count_the_picked_prs_closing_from_the_snapshots_it_is_shown(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = asked_to_close_widgets(cx);
            let release = harness.api.hold_close();
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
            let closing = |cx: &mut VisualTestContext| {
                harness
                    .view
                    .read_with(cx, |view, cx| view.close_prs().read(cx).closing())
            };
            assert_eq!(closing(cx), Some((0, 2)));
            assert_eq!(bar_label(&harness, cx), "Closing 0 of 2…");

            harness.show(harness.api.snapshot_now(), cx);
            assert_eq!(closing(cx), Some((2, 2)));

            release.send(()).unwrap();
            cx.run_until_parked();
            assert_eq!(closing(cx), None);
        }

        #[gpui::test]
        fn should_disable_select_on_the_empty_view(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(Snapshot::default(), cx);
            assert!(shows("pull requests empty".into(), cx));

            click("pull-requests select".into(), cx);

            assert!(!selecting(&harness, cx));
            assert!(!shows("pull-requests selection bar".into(), cx));
        }

        #[gpui::test]
        fn should_leave_select_mode_once_the_view_empties(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            click("pull-requests select".into(), cx);

            harness.show(Snapshot::default(), cx);

            assert!(!selecting(&harness, cx));
        }

        #[gpui::test]
        fn should_drop_a_picked_row_that_hides(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(mixed(), cx);
            click("show finished".into(), cx);
            click("pull-requests select".into(), cx);
            click("pr title acme/widgets#5".into(), cx);
            click("pr title acme/widgets#1".into(), cx);

            click("show finished".into(), cx);

            assert_eq!(picked(&harness, cx), [key(1, "acme/widgets")]);
        }

        #[gpui::test]
        fn should_draw_select_mode_in_both_themes(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(mixed(), cx);
            click("pull-requests select".into(), cx);
            click("pull-requests select all acme/widgets".into(), cx);

            for preference in [ThemePreference::Light, ThemePreference::Dark] {
                cx.update(|_, cx| Theme::set_preference(preference, cx));
                cx.run_until_parked();
                harness.draw(cx);
            }
        }
    }

    /// Landing on a row, as a PR alert's click asks (spec §11).
    mod reveal {
        use super::*;

        fn key(n: u32, repo: &str) -> PrKey {
            PrKey {
                repository: repository_id(repo),
                number: n,
            }
        }

        fn reveal(harness: &Harness, pr: PrKey, cx: &mut VisualTestContext) {
            cx.update(|_, cx| harness.view.update(cx, |view, cx| view.reveal(pr, cx)));
            cx.run_until_parked();
            harness.draw(cx);
            // A list that gained rows measures them in the frame after.
            harness.draw(cx);
        }

        fn numbers(harness: &Harness, cx: &mut VisualTestContext) -> Vec<u32> {
            let groups = harness.view.read_with(cx, |view, cx| view.groups(cx));
            (groups.iter().flat_map(|g| &g.prs))
                .map(|pr| pr.number)
                .collect()
        }

        #[gpui::test]
        fn should_open_the_rows_collapsed_group(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            let widgets = repository_id("acme/widgets");
            cx.update(|_, cx| harness.view.update(cx, |view, cx| view.toggle(widgets, cx)));
            assert!(!shows("pr acme/widgets#4".into(), cx));

            reveal(&harness, key(4, "acme/widgets"), cx);

            assert!(shows("pr acme/widgets#4".into(), cx));
            let collapsed =
                cx.update(|_, cx| gui_state::read(cx, |s| s.collapsed.pull_requests.clone()));
            assert!(!collapsed.contains(&widgets));
        }

        #[gpui::test]
        fn should_show_a_finished_row_and_leave_show_finished_off(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);

            reveal(&harness, key(3, "acme/gizmos"), cx);

            assert!(shows("pr acme/gizmos#3".into(), cx));
            assert_eq!(numbers(&harness, cx), [2, 3, 1, 4]);
            assert!(!cx.update(|_, cx| show_finished(cx)));
        }

        #[gpui::test]
        fn should_hide_the_finished_row_again_once_forgotten(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            reveal(&harness, key(3, "acme/gizmos"), cx);

            cx.update(|_, cx| harness.view.update(cx, |view, cx| view.forget_revealed(cx)));

            assert!(!shows("pr acme/gizmos#3".into(), cx));
        }

        #[gpui::test]
        fn should_scroll_the_row_into_view(cx: &mut TestAppContext) {
            let snapshot = Snapshot {
                prs: (1..=80).map(|n| pull_request(n, "acme/widgets")).collect(),
                pr_sync: Some(pr_status()),
                ..Snapshot::default()
            };
            let (harness, cx) = Harness::open(snapshot, cx);
            assert!(!shows("pr acme/widgets#80".into(), cx));

            reveal(&harness, key(80, "acme/widgets"), cx);

            assert!(shows("pr acme/widgets#80".into(), cx));
        }

        #[gpui::test]
        fn should_highlight_the_row_then_fade_it_after_about_two_seconds(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(prs(), cx);
            let pr = key(4, "acme/widgets");

            reveal(&harness, pr, cx);
            assert!(shows("highlight acme/widgets#4".into(), cx));
            assert!(!shows("highlight acme/widgets#1".into(), cx));

            cx.executor()
                .advance_clock(HIGHLIGHT_HOLD - Duration::from_millis(100));
            cx.run_until_parked();
            assert_eq!(harness.view.read_with(cx, |v, _| v.highlighted()), Some(pr));

            cx.executor()
                .advance_clock(Duration::from_millis(100) + HIGHLIGHT_FADE);
            cx.run_until_parked();
            assert_eq!(harness.view.read_with(cx, |v, _| v.highlighted()), None);
            assert!(!shows("highlight acme/widgets#4".into(), cx));
            assert!(shows("pr acme/widgets#4".into(), cx));
        }
    }
}
