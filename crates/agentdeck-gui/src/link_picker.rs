//! The Repositories part of Edit workspace and New workspace (spec §11, Penpot "Screens ·
//! Dialogs" → "Edit workspace", "Edit workspace · unlinked", "Edit workspace · picker" and
//! the rest): the Repositories the Workspace links, each with an Unlink `×`, and the
//! "Link a repository…" picker. An app-local composition: core-gpui's `Combobox` draws text-only
//! items, and the picker's rows carry a lock and its list a typed row.
//!
//! Nothing is applied here. Links and unlinks are staged until the dialog saves them in one
//! request ([`LinkPicker::change`]): an unlinked row stays crossed out with Undo, and a note
//! counts the open PRs that would leave the Workspace.
//!
//! The picker lists Suggested (the Workspace's Suggestions, Not now or not), then From your
//! pull requests (the Repositories of your kept PRs, but for those Suggested), then All your
//! repositories (every GitHub repository the sign-in can reach, but for those listed above),
//! leaving out what the Workspace links and Unreachable ones; typing filters all three. Typed
//! text naming a Repository (`owner/name` or its GitHub URL) that isn't listed offers a
//! "Link owner/name" row, which asks GitHub ([`Api::resolve_repository`]); signed out, typed
//! names and All your repositories are off.
//!
//! All your repositories is the daemon's GitHub repository list: each [`LinkPicker::reset`]
//! (a dialog opening) asks the daemon to load it again, and each `changed
//! github_repositories` reads it again, so its pages show as they arrive with "Loading your
//! repositories… 400 of about 1,100" under them. A list that failed to load reads "Couldn't
//! load your repositories." with Retry, and the other sections and typed names keep working.
//! The list draws only the rows in view, through GPUI's [`list`](gpui::list): about 1,100
//! rows drawn whole would make every frame slow.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use agentdeck_client::ApiError;
use agentdeck_client::types::{GithubListStatus, GithubRepositoryList};
use agentdeck_core::ws::{Entity as Changed, ServerMessage};
use agentdeck_core::{PrState, RepositoryId, RepositoryName, WorkspaceId};
use brrritscold_core_gpui::{
    ActiveTheme as _, Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, FieldError, Icon,
    IconName, Input, InputState, Label,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, shadow, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Anchor, AnyElement, App, AppContext as _, Bounds, BoxShadow, Context, Entity, EventEmitter,
    InteractiveElement as _, IntoElement, ListOffset, ListState, ParentElement as _, Pixels,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window,
    canvas, div, list, point, px,
};
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::connection::{Connection, DaemonEvent};
use crate::overview::Snapshot;
use crate::rereads::Rereads;
use crate::table;
use crate::workspace::failed;

/// The picker's placeholder.
pub const PLACEHOLDER: &str = "Link a repository…";

/// The heading over the Workspace's Suggestions, first in the list.
pub const SUGGESTED: &str = "Suggested";

/// The heading over the Repositories of your kept PRs.
pub const FROM_PRS: &str = "From your pull requests";

/// The heading over every GitHub repository the sign-in can reach, after the other sections.
pub const ALL_REPOSITORIES: &str = "All your repositories";

/// What All your repositories says while its list loads, before page 1 has answered.
pub const LOADING: &str = "Loading your repositories…";

/// What All your repositories says when its list didn't load; [`RETRY`] follows it.
pub const COULD_NOT_LOAD: &str = "Couldn't load your repositories.";

/// What loads the list again after it didn't load.
pub const RETRY: &str = "Retry";

/// What the list says in place of typed names while signed out of GitHub.
pub const SIGNED_OUT: &str = "Sign in to GitHub to find more repositories.";

/// The list's last line while signed in.
pub const NOT_LISTED: &str = "Not listed? Type owner/name or paste its URL.";

/// What the list says when typing matches nothing it can offer.
pub const NO_MATCH: &str = "No repositories match.";

/// A Repository as the picker shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// GitHub's id for it.
    pub id: RepositoryId,
    /// `owner/name`, as GitHub last called it.
    pub name: RepositoryName,
    /// Private: its row carries a lock.
    pub private: bool,
    /// The GitHub sign-in can't see it any more.
    pub unreachable: bool,
}

/// What the picker offers one Workspace, read from a snapshot.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Offer {
    /// The Repositories it links now, by name ignoring case.
    pub linked: Vec<Choice>,
    /// Its Suggestions, by name, dismissed ones included: opening the picker is deliberate.
    pub suggested: Vec<Choice>,
    /// The Repositories of your kept PRs, by name ignoring case.
    pub from_prs: Vec<Choice>,
    /// Each Repository's open PRs, counted as the sidebar counts them: drafts in, Unreachable
    /// and Finished ones out.
    pub open_prs: HashMap<RepositoryId, usize>,
    /// Whether GitHub accepts the token, so typed names can be looked up.
    pub signed_in: bool,
    /// Every Repository agentdeck keeps that the GitHub sign-in can't see any more.
    pub unreachable: Vec<RepositoryId>,
}

impl Offer {
    /// What `snapshot` offers `workspace`, archived or not, or a Workspace not made yet.
    pub fn read(snapshot: &Snapshot, workspace: Option<WorkspaceId>) -> Offer {
        let choice = |r: &agentdeck_client::types::RepositoryLinks| Choice {
            id: r.id,
            name: r.name.clone(),
            private: r.private,
            unreachable: r.unreachable,
        };
        let mut open_prs = HashMap::new();
        for pr in (snapshot.prs.iter())
            .filter(|pr| pr.state == PrState::Open && !pr.unreachable && !pr.finished)
        {
            *open_prs.entry(pr.repository.id).or_insert(0) += 1;
        }
        let mut repositories: Vec<_> = snapshot.repositories.iter().collect();
        repositories.sort_by_key(|r| (r.name.to_string().to_lowercase(), r.id.get()));
        let linked = (repositories.iter())
            .filter(|r| workspace.is_some_and(|w| r.workspaces.contains(&w)))
            .map(|r| choice(r))
            .collect();
        let from_prs = (repositories.iter())
            .filter(|r| snapshot.prs.iter().any(|pr| pr.repository.id == r.id))
            .map(|r| choice(r))
            .collect();
        let suggested = (workspace.into_iter())
            .flat_map(|w| snapshot.suggested(w))
            .map(|r| Choice {
                id: r.id,
                name: r.name.clone(),
                private: r.private,
                unreachable: false,
            })
            .collect();
        // Unknown until PR status is read: let GitHub answer for the token.
        let signed_in = (snapshot.pr_sync.as_ref())
            .is_none_or(|s| s.login != agentdeck_client::types::GhLogin::LoggedOut);
        let unreachable = (repositories.iter())
            .filter(|r| r.unreachable)
            .map(|r| r.id)
            .collect();
        Offer {
            linked,
            suggested,
            from_prs,
            open_prs,
            signed_in,
            unreachable,
        }
    }
}

/// All your repositories, as last read from the daemon's GitHub repository list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GithubList {
    /// Loading: the repositories loaded so far, in GitHub's order, how many that is, and about
    /// how many there are in all (unknown before page 1 answers).
    Loading {
        repositories: Vec<Choice>,
        loaded: u32,
        total: Option<u32>,
    },
    /// Every repository, in GitHub's order.
    Loaded(Vec<Choice>),
    /// The list didn't load: GitHub failed, or the daemon couldn't be asked.
    Failed,
    /// The daemon has no GitHub token to list with.
    SignedOut,
}

impl Default for GithubList {
    fn default() -> Self {
        GithubList::Loading {
            repositories: Vec::new(),
            loaded: 0,
            total: None,
        }
    }
}

impl GithubList {
    /// What the daemon's `list` holds for the picker. A list not loaded yet reads as loading:
    /// the picker has just asked for it.
    pub fn read(list: GithubRepositoryList) -> GithubList {
        let choices = |list: GithubRepositoryList| {
            (list.repositories.into_iter())
                .map(|r| Choice {
                    id: r.id,
                    name: r.name,
                    private: r.private,
                    unreachable: false,
                })
                .collect()
        };
        match list.status {
            GithubListStatus::Loaded => GithubList::Loaded(choices(list)),
            GithubListStatus::Failed => GithubList::Failed,
            GithubListStatus::SignedOut => GithubList::SignedOut,
            GithubListStatus::NotLoaded | GithubListStatus::Loading => GithubList::Loading {
                loaded: list.loaded,
                total: list.total,
                repositories: choices(list),
            },
        }
    }

    /// The repositories it holds now.
    fn repositories(&self) -> &[Choice] {
        match self {
            GithubList::Loading { repositories, .. } | GithubList::Loaded(repositories) => {
                repositories
            }
            GithubList::Failed | GithubList::SignedOut => &[],
        }
    }
}

/// The line under All your repositories while it loads: "Loading your repositories… 400 of
/// about 1,100", the total rounded to the hundred; before page 1 answers, no count.
pub fn loading_line(loaded: u32, total: Option<u32>) -> String {
    match total.filter(|_| loaded > 0) {
        Some(total) => {
            let about = if total >= 100 {
                (total + 50) / 100 * 100
            } else {
                total
            };
            format!(
                "{LOADING} {} of about {}",
                thousands(loaded),
                thousands(about)
            )
        }
        None => LOADING.to_owned(),
    }
}

/// `n` with a comma between each group of three digits: `1,104`.
fn thousands(n: u32) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Where a row of the list stands until the dialog saves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Staged {
    /// Linked, and staying so.
    Linked,
    /// Linked, and unlinking on Save: crossed out, with Undo.
    Unlinking,
    /// Picked, and linking on Save.
    Linking,
}

/// A row of the Workspace's Repositories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The Repository.
    pub repository: Choice,
    /// Where it stands.
    pub staged: Staged,
}

/// An entry of the open list, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// One of the Workspace's Suggestions.
    Suggested(Choice),
    /// A Repository of your kept PRs.
    Pick(Choice),
    /// A GitHub repository the sign-in can reach: All your repositories.
    All(Choice),
    /// "Link owner/name": a typed Repository the list doesn't hold.
    Typed(RepositoryName),
}

/// The staging changed: the dialog's Save may have become possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Staging;

/// The Repositories part, kept by a dialog.
pub struct LinkPicker {
    api: Arc<dyn Api>,
    input: Entity<InputState>,
    offer: Offer,
    rows: Vec<Row>,
    /// Whether the list shows.
    open: bool,
    /// The typed Repository GitHub is being asked about.
    looking_up: Option<RepositoryName>,
    /// Why the last typed Repository can't be linked, under the field; typing clears it.
    problem: Option<SharedString>,
    /// Whether the field takes the keyboard on the next draw.
    focus: bool,
    lookup: Option<Task<()>>,
    /// The field's bounds last frame: the list is as wide.
    field: Rc<Cell<Bounds<Pixels>>>,
    /// All your repositories, as last read.
    github: GithubList,
    /// Whether a dialog has opened the picker, so `changed github_repositories` reads the list.
    following: bool,
    /// Asking the daemon to load the list again.
    github_refresh: Option<Task<()>>,
    /// Reading the list.
    github_read: Option<Task<()>>,
    github_rereads: Rereads,
    /// The open list's rows, drawn only where in view.
    rows_list: ListState,
    /// What the open list showed last frame.
    shown: Vec<Item>,
    _input: Subscription,
    _events: Subscription,
}

/// A row of the open list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    /// A section's heading: its words and how tests find it.
    Heading(&'static str, &'static str),
    /// A Repository to pick.
    Choice(Entry),
    /// "Couldn't load your repositories." and Retry.
    Failed,
    /// "No repositories match.".
    NoMatch,
}

/// Heights of the open list's rows, as Penpot `agentdeck/LinkPickerList` lays them out: a
/// ComboboxLabel (`py-1.5 text-xs`), a ComboboxItem (`py-1.5 text-sm`) and ComboboxEmpty
/// (`py-2 text-sm`). The list is as tall as its rows, up to [`MAX_LIST_HEIGHT`].
const HEADING_HEIGHT: Pixels = px(30.);
const ITEM_HEIGHT: Pixels = px(33.);
const NO_MATCH_HEIGHT: Pixels = px(37.);

/// ComboboxList's `max-h-[calc(var(--spacing-72)-var(--spacing-9))]`: past it, the rows
/// scroll.
const MAX_LIST_HEIGHT: Pixels = px(252.);

impl Item {
    fn height(&self) -> Pixels {
        match self {
            Item::Heading(..) | Item::Failed => HEADING_HEIGHT,
            Item::Choice(_) => ITEM_HEIGHT,
            Item::NoMatch => NO_MATCH_HEIGHT,
        }
    }
}

impl EventEmitter<Staging> for LinkPicker {}

impl LinkPicker {
    /// An empty part that asks GitHub through `api`, reading the GitHub repository list again
    /// on `connection`'s `changed github_repositories`.
    pub fn new(
        api: Arc<dyn Api>,
        connection: &Entity<Connection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(PLACEHOLDER));
        let _input = cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
            InputEvent::Change => {
                this.problem = None;
                this.open = true;
                this.rows_list.scroll_to(ListOffset::default());
                cx.notify();
            }
            InputEvent::Focus => {
                this.open = true;
                cx.notify();
            }
            InputEvent::Blur => {
                this.open = false;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.pick_first(window, cx),
        });
        let _events = cx.subscribe(connection, |this, _, event, cx| match event {
            DaemonEvent::Message(
                ServerMessage::Changed {
                    entity: Changed::GithubRepositories,
                    ..
                }
                | ServerMessage::Resync,
            ) if this.following => this.read_github(cx),
            _ => {}
        });
        LinkPicker {
            api,
            input,
            offer: Offer::default(),
            rows: Vec::new(),
            open: false,
            looking_up: None,
            problem: None,
            focus: false,
            lookup: None,
            field: Rc::new(Cell::new(Bounds::default())),
            github: GithubList::default(),
            following: false,
            github_refresh: None,
            github_read: None,
            github_rereads: Rereads::default(),
            rows_list: table::list_state(),
            shown: Vec::new(),
            _input,
            _events,
        }
    }

    /// Starts over on `offer`: its linked Repositories as rows, nothing staged, nothing typed.
    pub fn reset(&mut self, offer: Offer, window: &mut Window, cx: &mut Context<Self>) {
        self.rows = (offer.linked.iter())
            .map(|repository| Row {
                repository: repository.clone(),
                staged: Staged::Linked,
            })
            .collect();
        self.offer = offer;
        self.open = false;
        self.looking_up = None;
        self.lookup = None;
        self.problem = None;
        self.focus = false;
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.following = true;
        self.refresh_github(cx);
        cx.emit(Staging);
        cx.notify();
    }

    /// Asks the daemon to load the GitHub repository list again, then reads it: what each
    /// opening and Retry do. A list held already keeps showing; the daemon refreshes it
    /// quietly.
    fn refresh_github(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.github, GithubList::Loaded(_)) {
            self.github = GithubList::default();
        }
        let refreshing = self.api.refresh_github_repositories(cx);
        self.github_refresh = Some(cx.spawn(async move |this, cx| {
            let refreshed = refreshing.await;
            let _ = this.update(cx, |this, cx| match refreshed {
                Ok(()) => this.read_github(cx),
                Err(err) => {
                    tracing::warn!("asking for the GitHub repository list: {err:#}");
                    this.github = GithubList::Failed;
                    cx.notify();
                }
            });
        }));
    }

    /// Reads the GitHub repository list, once more after a read running now if one does.
    fn read_github(&mut self, cx: &mut Context<Self>) {
        if !self.github_rereads.ask() {
            return;
        }
        let reading = self.api.github_repositories(cx);
        self.github_read = Some(cx.spawn(async move |this, cx| {
            let read = reading.await;
            let _ = this.update(cx, |this, cx| {
                this.github = match read {
                    Ok(list) => GithubList::read(list),
                    Err(err) => {
                        tracing::warn!("reading the GitHub repository list: {err:#}");
                        GithubList::Failed
                    }
                };
                if this.github_rereads.landed() {
                    this.read_github(cx);
                }
                cx.notify();
            });
        }));
    }

    /// Retry under a list that didn't load.
    fn retry(&mut self, cx: &mut Context<Self>) {
        self.refresh_github(cx);
        cx.notify();
    }

    /// All your repositories, as last read.
    #[cfg(test)]
    pub fn github(&self) -> &GithubList {
        &self.github
    }

    /// Whether All your repositories and typed names are on: GitHub accepts the token.
    fn signed_in(&self) -> bool {
        self.offer.signed_in && self.github != GithubList::SignedOut
    }

    /// Gives the field the keyboard on the next draw, its list open.
    pub fn focus(&mut self, cx: &mut Context<Self>) {
        self.focus = true;
        self.open = true;
        cx.notify();
    }

    /// The rows: the Workspace's Repositories as staged.
    #[cfg(test)]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Whether the list shows.
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Why the last typed Repository can't be linked, if it can't.
    #[cfg(test)]
    pub fn problem(&self) -> Option<&SharedString> {
        self.problem.as_ref()
    }

    /// Whether GitHub is being asked about a typed Repository; the dialog waits for it.
    pub fn is_looking_up(&self) -> bool {
        self.looking_up.is_some()
    }

    /// The list's entries for what is typed now.
    pub fn entries(&self, cx: &App) -> Vec<Entry> {
        let typed = self.input.read(cx).value();
        let typed = typed.trim();
        let named: Option<RepositoryName> = typed.parse().ok();
        // A pasted URL filters by the name it holds.
        let filter = named
            .as_ref()
            .map_or_else(|| typed.to_lowercase(), |n| n.to_string().to_lowercase());
        let held = |id: RepositoryId| self.rows.iter().any(|r| r.repository.id == id);
        let offered = |c: &&Choice| {
            !c.unreachable && !held(c.id) && c.name.to_string().to_lowercase().contains(&filter)
        };
        let suggested: Vec<&Choice> = self.offer.suggested.iter().filter(offered).collect();
        let picks: Vec<&Choice> = (self.offer.from_prs.iter())
            .filter(|c| !self.offer.suggested.iter().any(|s| s.id == c.id))
            .filter(offered)
            .collect();
        let signed_in = self.signed_in();
        // Every Repository listed above, whether or not it passes the filter, and every one
        // the sign-in can't see.
        let above: std::collections::HashSet<RepositoryId> = (self.offer.suggested.iter())
            .chain(&self.offer.from_prs)
            .map(|c| c.id)
            .chain(self.offer.unreachable.iter().copied())
            .collect();
        let all: Vec<&Choice> = match signed_in {
            true => (self.github.repositories().iter())
                .filter(|c| !above.contains(&c.id))
                .filter(offered)
                .collect(),
            false => Vec::new(),
        };
        let mut entries: Vec<Entry> = (suggested.iter())
            .map(|c| Entry::Suggested((*c).clone()))
            .chain(picks.iter().map(|c| Entry::Pick((*c).clone())))
            .chain(all.iter().map(|c| Entry::All((*c).clone())))
            .collect();
        if let Some(named) = named.filter(|_| signed_in) {
            let same = |name: &RepositoryName| same_name(name, &named);
            let listed = (suggested.iter().chain(&picks).chain(&all)).any(|c| same(&c.name))
                || self.rows.iter().any(|r| same(&r.repository.name));
            if !listed {
                entries.push(Entry::Typed(named));
            }
        }
        entries
    }

    /// The Repositories to link and the ids to unlink on Save.
    pub fn change(&self) -> (Vec<RepositoryName>, Vec<RepositoryId>) {
        let staged = |staged| (self.rows.iter()).filter(move |r| r.staged == staged);
        (
            staged(Staged::Linking)
                .map(|r| r.repository.name.clone())
                .collect(),
            staged(Staged::Unlinking).map(|r| r.repository.id).collect(),
        )
    }

    /// Whether anything is staged.
    pub fn is_changed(&self) -> bool {
        self.rows.iter().any(|r| r.staged != Staged::Linked)
    }

    /// The note under the rows: how many open PRs the staged unlinks take out of the
    /// Workspace; `None` at 0.
    pub fn note(&self) -> Option<String> {
        let leaving: usize = (self.rows.iter())
            .filter(|r| r.staged == Staged::Unlinking)
            .filter_map(|r| self.offer.open_prs.get(&r.repository.id))
            .sum();
        match leaving {
            0 => None,
            1 => Some("1 open PR will leave this workspace".to_owned()),
            n => Some(format!("{n} open PRs will leave this workspace")),
        }
    }

    /// Stages `choice` for linking, clearing what is typed.
    pub fn pick(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        self.stage(choice, cx);
        self.clear(window, cx);
    }

    /// Asks GitHub for typed Repository `name` and stages it for linking once GitHub shows
    /// it; if GitHub can't see it, says so under the field.
    pub fn pick_typed(
        &mut self,
        name: RepositoryName,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.looking_up.is_some() {
            return;
        }
        self.looking_up = Some(name.clone());
        self.problem = None;
        cx.emit(Staging);
        cx.notify();
        let resolving = self.api.resolve_repository(name.clone(), cx);
        self.lookup = Some(cx.spawn_in(window, async move |this, cx| {
            let resolved = resolving.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.looking_up = None;
                match resolved {
                    Ok(found) => {
                        let choice = Choice {
                            id: found.id,
                            name: found.name,
                            private: found.private,
                            unreachable: false,
                        };
                        this.stage(choice, cx);
                        this.clear(window, cx);
                    }
                    Err(err) => {
                        // The list would cover what shows under the field; typing reopens it.
                        this.open = false;
                        match err.downcast_ref::<ApiError>() {
                            Some(ApiError {
                                message,
                                code: Some(_),
                            }) => this.problem = Some(message.clone().into()),
                            _ => failed(format!("Couldn't look up {name}"), &err, window, cx),
                        }
                    }
                }
                cx.emit(Staging);
                cx.notify();
            });
        }));
    }

    /// The row `×`: a linked row is crossed out for unlinking, a picked one is dropped.
    pub fn unlink(&mut self, id: RepositoryId, cx: &mut Context<Self>) {
        let Some(at) = self.rows.iter().position(|r| r.repository.id == id) else {
            return;
        };
        match self.rows[at].staged {
            Staged::Linked => self.rows[at].staged = Staged::Unlinking,
            Staged::Linking => {
                self.rows.remove(at);
            }
            Staged::Unlinking => return,
        }
        cx.emit(Staging);
        cx.notify();
    }

    /// Undo on a crossed-out row: it stays linked.
    pub fn undo(&mut self, id: RepositoryId, cx: &mut Context<Self>) {
        if let Some(row) =
            (self.rows.iter_mut()).find(|r| r.repository.id == id && r.staged == Staged::Unlinking)
        {
            row.staged = Staged::Linked;
            cx.emit(Staging);
            cx.notify();
        }
    }

    /// Adds `choice` to the rows for linking; one already a row stays as it is, but for a
    /// crossed-out one, which is linked again.
    fn stage(&mut self, choice: Choice, cx: &mut Context<Self>) {
        match self.rows.iter_mut().find(|r| r.repository.id == choice.id) {
            Some(row) if row.staged == Staged::Unlinking => row.staged = Staged::Linked,
            Some(_) => {}
            None => self.rows.push(Row {
                repository: choice,
                staged: Staged::Linking,
            }),
        }
        cx.emit(Staging);
        cx.notify();
    }

    /// Empties the field and closes the list, keeping the keyboard there.
    fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.open = false;
        cx.notify();
    }

    /// Shows the daemon's refusal of a Repository to link under the field, when `err` is one
    /// (`unknown_repository`, `not_signed_in`, `invalid_links`); whether it was.
    pub fn refused(&mut self, err: &anyhow::Error, cx: &mut Context<Self>) -> bool {
        let Some(ApiError {
            message,
            code: Some(code),
        }) = err.downcast_ref::<ApiError>()
        else {
            return false;
        };
        if !matches!(
            code.as_str(),
            "unknown_repository" | "not_signed_in" | "invalid_links"
        ) {
            return false;
        }
        self.problem = Some(message.clone().into());
        cx.notify();
        true
    }

    /// Enter in the field: picks the list's first entry.
    fn pick_first(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.entries(cx).into_iter().next() {
            Some(Entry::Suggested(choice) | Entry::Pick(choice) | Entry::All(choice)) => {
                self.pick(choice, window, cx)
            }
            Some(Entry::Typed(name)) => self.pick_typed(name, window, cx),
            None => {}
        }
    }
}

/// Whether two names are one Repository's, as GitHub compares them: ignoring case.
fn same_name(a: &RepositoryName, b: &RepositoryName) -> bool {
    a.to_string().eq_ignore_ascii_case(&b.to_string())
}

/// `sideOffset={6}`, as core-gpui's Combobox: the gap between the field and the list.
const SIDE_OFFSET: gpui::Pixels = space::S1_5;

impl LinkPicker {
    /// A row of the Workspace's Repositories (Penpot `agentdeck/LinkedRepository`): a lock if
    /// private, its name, an Unreachable badge, then its `×`, or Undo once crossed out.
    fn row(&self, row: &Row, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let name = row.repository.name.to_string();
        let id = row.repository.id;
        let crossed = row.staged == Staged::Unlinking;
        div()
            .debug_selector({
                let name = name.clone();
                move || format!("repository {name}")
            })
            .flex()
            .items_center()
            .gap(space::S2)
            .h(space::S8)
            .text_size(font_size::SM)
            .when(row.repository.private, |d| {
                d.child(lock(&name, s.muted_foreground))
            })
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(if crossed {
                        s.muted_foreground
                    } else {
                        s.foreground
                    })
                    .when(crossed, |d| d.line_through())
                    .child(name.clone()),
            )
            .when(row.repository.unreachable, |d| {
                d.child(
                    div()
                        .debug_selector({
                            let name = name.clone();
                            move || format!("unreachable {name}")
                        })
                        .child(
                            Badge::new()
                                .variant(BadgeVariant::Outline)
                                .label("Unreachable"),
                        ),
                )
            })
            .child(div().flex_1())
            .map(|d| match crossed {
                true => d.child(
                    div()
                        .debug_selector({
                            let name = name.clone();
                            move || format!("undo {name}")
                        })
                        .child(
                            Button::new(SharedString::from(format!("undo-{id}")))
                                .variant(ButtonVariant::Link)
                                .size(ButtonSize::Xs)
                                .label("Undo")
                                .on_click(cx.listener(move |this, _, _, cx| this.undo(id, cx))),
                        ),
                ),
                false => d.child(
                    div()
                        .debug_selector({
                            let name = name.clone();
                            move || format!("unlink {name}")
                        })
                        .child(
                            Button::new(SharedString::from(format!("unlink-{id}")))
                                .variant(ButtonVariant::Ghost)
                                .size(ButtonSize::IconXs)
                                .icon(IconName::X)
                                .on_click(cx.listener(move |this, _, _, cx| this.unlink(id, cx))),
                        ),
                ),
            })
    }

    /// The open list's rows for what is typed now: Suggested, From your pull requests, All
    /// your repositories or its failed line, the typed row, or "No repositories match.".
    fn items(&self, cx: &App) -> Vec<Item> {
        let entries = self.entries(cx);
        let mut items = Vec::new();
        let section = |heading: Option<Item>, rows: Vec<Item>, items: &mut Vec<Item>| {
            if !rows.is_empty() {
                items.extend(heading);
                items.extend(rows);
            }
        };
        let of = |keep: fn(&Entry) -> bool| -> Vec<Item> {
            (entries.iter())
                .filter(|e| keep(e))
                .map(|e| Item::Choice(e.clone()))
                .collect()
        };
        section(
            Some(Item::Heading(SUGGESTED, "suggested")),
            of(|e| matches!(e, Entry::Suggested(_))),
            &mut items,
        );
        section(
            Some(Item::Heading(FROM_PRS, "from your pull requests")),
            of(|e| matches!(e, Entry::Pick(_))),
            &mut items,
        );
        let mut all = of(|e| matches!(e, Entry::All(_)));
        if self.signed_in() && self.github == GithubList::Failed {
            all.push(Item::Failed);
        }
        section(
            Some(Item::Heading(ALL_REPOSITORIES, "all your repositories")),
            all,
            &mut items,
        );
        section(None, of(|e| matches!(e, Entry::Typed(_))), &mut items);
        if items.is_empty() && self.loading().is_none() {
            items.push(Item::NoMatch);
        }
        items
    }

    /// The line under the rows while All your repositories loads: "Loading your
    /// repositories… 400 of about 1,100".
    fn loading(&self) -> Option<String> {
        match &self.github {
            GithubList::Loading { loaded, total, .. } if self.signed_in() => {
                Some(loading_line(*loaded, *total))
            }
            _ => None,
        }
    }

    /// Row `ix` of the open list, as [`Self::list`] last listed them.
    fn item(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let s = cx.theme().semantic();
        let Some(item) = self.shown.get(ix).cloned() else {
            return div().into_any_element();
        };
        // ComboboxItem: flex items-center gap-2 rounded-sm py-1.5 pr-8 pl-2 text-sm,
        // hover:bg-accent
        let row = |id: SharedString| {
            div()
                .id(id)
                .h(ITEM_HEIGHT)
                .flex()
                .items_center()
                .gap(space::S2)
                .rounded(s.radius_sm)
                .py(space::S1_5)
                .pr(space::S8)
                .pl(space::S2)
                .text_size(font_size::SM)
                .whitespace_nowrap()
                .hover(|d| d.bg(s.accent).text_color(s.accent_foreground))
        };
        // ComboboxLabel: px-2 py-1.5 text-xs text-muted-foreground
        let line = || {
            div()
                .h(HEADING_HEIGHT)
                .flex()
                .items_center()
                .px(space::S2)
                .py(space::S1_5)
                .text_size(font_size::XS)
                .text_color(s.muted_foreground)
        };
        match item {
            Item::Heading(text, selector) => line()
                .debug_selector(move || selector.into())
                .child(text)
                .into_any_element(),
            Item::Failed => div()
                .debug_selector(|| "could not load".into())
                .h(HEADING_HEIGHT)
                .flex()
                .items_center()
                .child(line().child(COULD_NOT_LOAD))
                .child(
                    // Drawn as core-gpui's link xs Button (text-primary hover:underline h-6 px-2
                    // text-xs font-medium), without its focus: a button taking the keyboard
                    // would blur the field, closing the list before the click lands.
                    div()
                        .id("retry-github-list")
                        .debug_selector(|| "retry".into())
                        .h(space::S6)
                        .flex()
                        .items_center()
                        .px(space::S2)
                        .rounded(s.radius_md)
                        .text_size(font_size::XS)
                        .font_weight(font_weight::MEDIUM)
                        .text_color(s.primary)
                        .cursor_pointer()
                        .hover(|d| d.underline())
                        .child(RETRY)
                        .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                )
                .into_any_element(),
            // ComboboxEmpty: flex w-full justify-center py-2 text-sm text-muted-foreground
            Item::NoMatch => div()
                .debug_selector(|| "no match".into())
                .h(NO_MATCH_HEIGHT)
                .flex()
                .items_center()
                .w_full()
                .justify_center()
                .py(space::S2)
                .text_size(font_size::SM)
                .text_color(s.muted_foreground)
                .child(NO_MATCH)
                .into_any_element(),
            Item::Choice(Entry::Typed(name)) => {
                let busy = (self.looking_up.as_ref()).is_some_and(|n| same_name(n, &name));
                let label = match busy {
                    true => format!("Looking up {name}…"),
                    false => format!("Link {name}"),
                };
                row("link-typed".into())
                    .debug_selector(|| "link typed".into())
                    .child(Icon::new(IconName::Plus).size(px(14.)))
                    .child(label)
                    .when(busy, |d| d.opacity(0.5))
                    .when(!busy, |d| {
                        d.on_click(cx.listener(move |this, _, window, cx| {
                            this.pick_typed(name.clone(), window, cx)
                        }))
                    })
                    .into_any_element()
            }
            Item::Choice(Entry::Suggested(choice)) => self.choice(row, "suggested", choice, cx),
            Item::Choice(Entry::Pick(choice)) => self.choice(row, "pick", choice, cx),
            Item::Choice(Entry::All(choice)) => self.choice(row, "all", choice, cx),
        }
    }

    /// A Repository's row in section `kind`, as tests find it (`all acme/widgets`): a lock if
    /// private, then its name; a click picks it.
    fn choice(
        &self,
        row: impl Fn(SharedString) -> gpui::Stateful<gpui::Div>,
        kind: &'static str,
        choice: Choice,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().semantic().muted_foreground;
        let name = choice.name.to_string();
        let selector = format!("{kind} {name}");
        row(SharedString::from(format!("{kind}-{}", choice.id)))
            .debug_selector(move || selector.clone())
            .when(choice.private, |d| d.child(lock(&name, muted)))
            .child(name)
            .on_click(cx.listener(move |this, _, window, cx| this.pick(choice.clone(), window, cx)))
            .into_any_element()
    }

    /// The open list (Penpot `agentdeck/LinkPickerList`), drawn as core-gpui's
    /// `ComboboxContent`: its rows ([`Self::items`]), as tall as they are up to
    /// [`MAX_LIST_HEIGHT`] and scrolling past it, the loading line under them, so it shows
    /// however far they scroll, then the last line.
    fn list(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let items = self.items(cx);
        if items != self.shown {
            table::relist(&self.rows_list, items.len());
            self.shown = items;
        }
        let height = (self.shown.iter())
            .map(Item::height)
            .fold(px(0.), |sum, h| sum + h)
            .min(MAX_LIST_HEIGHT);
        let mut shadows = vec![BoxShadow {
            color: s.foreground.opacity(0.1),
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(1.),
            inset: false,
        }];
        shadows.extend_from_slice(shadow::MD);
        // ComboboxLabel: px-2 py-1.5 text-xs text-muted-foreground
        let line = |text: &'static str, selector: &'static str| {
            div()
                .debug_selector(move || selector.into())
                .px(space::S2)
                .py(space::S1_5)
                .text_size(font_size::XS)
                .text_color(s.muted_foreground)
                .child(text)
        };
        div()
            .debug_selector(|| "picker list".into())
            .id("link-picker-list")
            .w_full()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.open = false;
                cx.notify();
            }))
            .flex()
            .flex_col()
            .p(space::S1)
            .rounded(s.radius_md)
            .bg(s.popover)
            .text_color(s.popover_foreground)
            .shadow(shadows)
            .child(
                div().h(height).child(
                    list(
                        self.rows_list.clone(),
                        cx.processor(|this, ix, _, cx| this.item(ix, cx)),
                    )
                    .size_full(),
                ),
            )
            .when_some(self.loading(), |list, text| {
                list.child(
                    div()
                        .debug_selector(|| "loading your repositories".into())
                        .px(space::S2)
                        .py(space::S1_5)
                        .text_size(font_size::XS)
                        .text_color(s.muted_foreground)
                        .child(text),
                )
            })
            .child(
                // ComboboxSeparator: -mx-1 my-1 h-px bg-border
                div().mx(-space::S1).my(space::S1).h(px(1.)).bg(s.border),
            )
            .child(match self.signed_in() {
                true => line(NOT_LISTED, "not listed"),
                false => line(SIGNED_OUT, "signed out"),
            })
    }
}

/// A private Repository's lock: lucide `Lock`, 14px, as the PR state icons. Tests find it as
/// `lock <name>`.
fn lock(name: &str, color: gpui::Hsla) -> impl IntoElement {
    let selector = format!("lock {name}");
    div()
        .debug_selector(move || selector.clone())
        .flex_none()
        .child(Icon::new(IconName::Lock).size(px(14.)).text_color(color))
}

impl Render for LinkPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus) {
            // A dialog focuses itself in an effect it queues while drawing, after this
            // render; queueing from inside an effect lands after that one.
            let input = self.input.clone();
            window.defer(cx, move |window, cx| {
                window.defer(cx, move |window, cx| {
                    input.update(cx, |input, cx| input.focus(window, cx));
                });
            });
        }
        let s = cx.theme().semantic();
        let rows: Vec<_> = (self.rows.clone().iter())
            .map(|row| self.row(row, cx).into_any_element())
            .collect();
        let note = self.note();
        let problem = self.problem.clone();
        let bounds = self.field.clone();
        let field = div()
            .debug_selector(|| "link picker".into())
            .relative()
            .child(Input::new(&self.input).aria_invalid(problem.is_some()))
            .child(
                // Notes the field's bounds for the list.
                canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
                    .absolute()
                    .size_full()
                    .top_0()
                    .left_0(),
            );
        let field = gpui_base::Popup::new("link-picker", field)
            .anchor(Anchor::TopLeft)
            .offset(SIDE_OFFSET)
            .w_full();
        let field = match self.open {
            true => {
                let width = self.field.get().size.width;
                field.content(div().w(width).child(self.list(cx)))
            }
            false => field,
        };
        div()
            .flex()
            .flex_col()
            .gap(space::S2)
            .child(Label::new().child("Repositories"))
            .when(!rows.is_empty(), |d| {
                d.child(
                    div()
                        .debug_selector(|| "repository rows".into())
                        .flex()
                        .flex_col()
                        .children(rows),
                )
            })
            .when_some(note, |d, note| {
                d.child(
                    div()
                        .debug_selector(|| "leaving note".into())
                        .text_size(font_size::SM)
                        .text_color(s.muted_foreground)
                        .child(note),
                )
            })
            .child(
                div()
                    .on_action(cx.listener(|this, _: &gpui_base::input::Escape, _, cx| {
                        if this.open {
                            this.open = false;
                            cx.notify();
                        } else {
                            cx.propagate();
                        }
                    }))
                    .child(field),
            )
            .when_some(problem, |d, why| {
                d.child(
                    div()
                        .debug_selector(|| "picker error".into())
                        .child(FieldError::new(why)),
                )
            })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::rc::Rc;

    use agentdeck_client::types::PullRequest;
    use agentdeck_client::ws::{self, Remote};
    use brrritscold_core_gpui::Root;
    use gpui::{Focusable as _, Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::connection::fake::FakeConnector;
    use crate::overview::fixtures::{id, links, pr_sync, pull_request, repository_id, suggestion};

    const WIDGETS: &str = "acme/widgets";
    const GADGETS: &str = "acme/gadgets";
    const SECRETS: &str = "acme/secrets";
    const LOST: &str = "acme/lost";

    pub(crate) fn choice(repo: &str) -> Choice {
        Choice {
            id: repository_id(repo),
            name: repo.parse().unwrap(),
            private: false,
            unreachable: false,
        }
    }

    pub(crate) fn repository(repo: &str) -> agentdeck_client::types::Repository {
        agentdeck_client::types::Repository {
            archived: false,
            id: repository_id(repo),
            name: repo.parse().unwrap(),
            private: false,
            url: format!("https://github.com/{repo}"),
        }
    }

    mod offer {
        use super::*;

        fn snapshot() -> Snapshot {
            let web = id(1);
            let private = agentdeck_client::types::RepositoryLinks {
                private: true,
                ..links(SECRETS, &[])
            };
            let lost = agentdeck_client::types::RepositoryLinks {
                unreachable: true,
                ..links(LOST, &[web])
            };
            let draft = PullRequest {
                draft: true,
                ..pull_request(2, WIDGETS)
            };
            let finished = PullRequest {
                finished: true,
                ..pull_request(3, WIDGETS)
            };
            let merged = PullRequest {
                state: PrState::Merged,
                ..pull_request(4, GADGETS)
            };
            Snapshot {
                repositories: vec![links(GADGETS, &[]), lost, private, links(WIDGETS, &[web])],
                prs: vec![
                    pull_request(1, WIDGETS),
                    draft,
                    finished,
                    merged,
                    pull_request(5, SECRETS),
                ],
                ..Snapshot::default()
            }
        }

        #[test]
        fn should_hold_what_the_workspace_links_unreachable_ones_included() {
            let offer = Offer::read(&snapshot(), Some(id(1)));

            let lost = Choice {
                unreachable: true,
                ..choice(LOST)
            };
            assert_eq!(offer.linked, [lost, choice(WIDGETS)]);
        }

        #[test]
        fn should_offer_the_reachable_repositories_of_your_kept_prs() {
            let offer = Offer::read(&snapshot(), None);

            let secrets = Choice {
                private: true,
                ..choice(SECRETS)
            };
            assert_eq!(offer.from_prs, [choice(GADGETS), secrets, choice(WIDGETS)]);
            assert_eq!(offer.linked, []);
        }

        #[test]
        fn should_count_open_prs_drafts_in_and_finished_out() {
            let offer = Offer::read(&snapshot(), Some(id(1)));

            assert_eq!(offer.open_prs.get(&repository_id(WIDGETS)), Some(&2));
            assert_eq!(offer.open_prs.get(&repository_id(GADGETS)), None);
        }

        #[test]
        fn should_offer_the_workspaces_suggestions_but_what_it_links_by_now() {
            let mut snapshot = snapshot();
            snapshot
                .workspaces
                .push(crate::overview::fixtures::workspace(1, "web"));
            let private = agentdeck_client::types::Repository {
                private: true,
                ..suggestion("acme/infra")
            };
            snapshot.suggestions.insert(
                id(1),
                vec![private, suggestion("acme/website"), suggestion(WIDGETS)],
            );

            let offer = Offer::read(&snapshot, Some(id(1)));

            let infra = Choice {
                private: true,
                ..choice("acme/infra")
            };
            assert_eq!(offer.suggested, [infra, choice("acme/website")]);
            assert_eq!(Offer::read(&snapshot, None).suggested, []);
        }

        #[test]
        fn should_hold_every_kept_repository_github_cannot_see() {
            let offer = Offer::read(&snapshot(), None);

            assert_eq!(offer.unreachable, [repository_id(LOST)]);
        }

        #[test]
        fn should_be_signed_out_only_while_github_rejects_the_token() {
            let mut snapshot = snapshot();
            assert!(Offer::read(&snapshot, None).signed_in);

            pr_sync(&mut snapshot).login = agentdeck_client::types::GhLogin::LoggedOut;

            assert!(!Offer::read(&snapshot, None).signed_in);
        }
    }

    pub(crate) struct Harness {
        pub api: Arc<FakeApi>,
        pub picker: Entity<LinkPicker>,
        /// Plays the daemon's socket.
        pub remote: Remote,
        _connection: Entity<Connection>,
    }

    /// The daemon's GitHub repository list, loaded whole with `repos`.
    pub(crate) fn loaded(repos: &[Choice]) -> GithubRepositoryList {
        GithubRepositoryList {
            status: GithubListStatus::Loaded,
            loaded: repos.len() as u32,
            total: Some(repos.len() as u32),
            error: None,
            repositories: repos.iter().map(as_repository).collect(),
        }
    }

    fn as_repository(choice: &Choice) -> agentdeck_client::types::Repository {
        agentdeck_client::types::Repository {
            private: choice.private,
            ..repository(&choice.name.to_string())
        }
    }

    /// Widgets and a private gadgets linked, widgets with 2 open PRs and gadgets with 1; your
    /// PRs come from widgets, gadgets, secrets (private), lost (Unreachable) and tools.
    pub(crate) fn offer() -> Offer {
        let private = |repo| Choice {
            private: true,
            ..choice(repo)
        };
        Offer {
            linked: vec![private(GADGETS), choice(WIDGETS)],
            suggested: Vec::new(),
            from_prs: vec![
                private(GADGETS),
                Choice {
                    unreachable: true,
                    ..choice(LOST)
                },
                private(SECRETS),
                choice("acme/tools"),
                choice(WIDGETS),
            ],
            open_prs: HashMap::from([(repository_id(WIDGETS), 2), (repository_id(GADGETS), 1)]),
            signed_in: true,
            unreachable: vec![repository_id(LOST)],
        }
    }

    impl Harness {
        /// A picker opened on `offer`, whose daemon's GitHub repository list is loaded and
        /// holds nothing.
        pub(crate) fn open(
            offer: Offer,
            cx: &mut TestAppContext,
        ) -> (Harness, &mut VisualTestContext) {
            Harness::open_with(offer, loaded(&[]), cx)
        }

        /// A picker opened on `offer`, whose daemon's GitHub repository list reads `github`.
        pub(crate) fn open_with(
            offer: Offer,
            github: GithubRepositoryList,
            cx: &mut TestAppContext,
        ) -> (Harness, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            let api = Arc::new(FakeApi::default());
            api.set_github_list(github);
            let connector = Rc::new(FakeConnector::default());
            let remote = connector.then_connect_to(api.clone());
            let connection = cx.update(|cx| cx.new(|cx| Connection::new(connector, cx)));
            let mut picker = None;
            let window = cx.add_window(|window, cx| {
                let api: Arc<dyn Api> = api.clone();
                let view = cx.new(|cx| LinkPicker::new(api, &connection, window, cx));
                picker = Some(view.clone());
                Root::new(view, window, cx)
            });
            let cx = VisualTestContext::from_window(*window, cx).into_mut();
            cx.update(|window, _| window.activate_window());
            let picker = picker.unwrap();
            cx.update(|window, cx| picker.update(cx, |p, cx| p.reset(offer, window, cx)));
            let harness = Harness {
                api,
                picker,
                remote,
                _connection: connection,
            };
            harness.settle(cx);
            (harness, cx)
        }

        /// The daemon says its GitHub repository list changed.
        pub(crate) fn github_changed(&self, cx: &mut VisualTestContext) {
            self.remote
                .events
                .try_send(ws::Event::Message(ServerMessage::Changed {
                    entity: Changed::GithubRepositories,
                    id: "list".into(),
                }))
                .unwrap();
            self.settle(cx);
        }

        pub(crate) fn settle(&self, cx: &mut VisualTestContext) {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        pub(crate) fn type_in(&self, text: &str, cx: &mut VisualTestContext) {
            cx.update(|window, cx| {
                let input = self.picker.read(cx).input.clone();
                input.update(cx, |input, cx| {
                    input.focus(window, cx);
                    input.set_value("", window, cx);
                });
            });
            self.settle(cx);
            if !text.is_empty() {
                cx.simulate_input(text);
            }
            self.settle(cx);
        }

        pub(crate) fn entries(&self, cx: &mut VisualTestContext) -> Vec<Entry> {
            cx.update(|_, cx| self.picker.read(cx).entries(cx))
        }

        pub(crate) fn rows(&self, cx: &mut VisualTestContext) -> Vec<(String, Staged)> {
            self.picker.read_with(cx, |p, _| {
                (p.rows().iter())
                    .map(|r| (r.repository.name.to_string(), r.staged))
                    .collect()
            })
        }

        pub(crate) fn click(&self, selector: &'static str, cx: &mut VisualTestContext) {
            let at = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} shown"));
            cx.simulate_click(at.center(), Modifiers::default());
            self.settle(cx);
        }

        pub(crate) fn note(&self, cx: &mut VisualTestContext) -> Option<String> {
            self.picker.read_with(cx, |p, _| p.note())
        }

        pub(crate) fn change(
            &self,
            cx: &mut VisualTestContext,
        ) -> (Vec<String>, Vec<RepositoryId>) {
            let (link, unlink) = self.picker.read_with(cx, |p, _| p.change());
            (link.iter().map(|n| n.to_string()).collect(), unlink)
        }
    }

    fn picks(entries: &[Entry]) -> Vec<String> {
        (entries.iter())
            .map(|e| match e {
                Entry::Suggested(choice) => format!("Suggested {}", choice.name),
                Entry::Pick(choice) => choice.name.to_string(),
                Entry::All(choice) => format!("All {}", choice.name),
                Entry::Typed(name) => format!("Link {name}"),
            })
            .collect()
    }

    mod rows {
        use super::*;

        #[gpui::test]
        fn should_list_the_linked_repositories_with_an_unlink_each(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(offer(), cx);

            assert_eq!(
                harness.rows(cx),
                [
                    (GADGETS.to_owned(), Staged::Linked),
                    (WIDGETS.to_owned(), Staged::Linked)
                ]
            );
            assert!(cx.debug_bounds("unlink acme/widgets").is_some());
            assert!(cx.debug_bounds("lock acme/gadgets").is_some());
            assert!(cx.debug_bounds("lock acme/widgets").is_none());
        }

        #[gpui::test]
        fn should_badge_an_unreachable_repository(cx: &mut TestAppContext) {
            let lost = Choice {
                unreachable: true,
                ..choice(LOST)
            };
            let (_harness, cx) = Harness::open(
                Offer {
                    linked: vec![lost, choice(WIDGETS)],
                    ..offer()
                },
                cx,
            );

            assert!(cx.debug_bounds("unreachable acme/lost").is_some());
            assert!(cx.debug_bounds("unreachable acme/widgets").is_none());
        }

        #[gpui::test]
        fn should_cross_out_an_unlinked_row_with_undo_and_note_its_open_prs(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(offer(), cx);
            assert_eq!(harness.note(cx), None);

            harness.click("unlink acme/widgets", cx);

            assert_eq!(harness.rows(cx)[1], (WIDGETS.to_owned(), Staged::Unlinking));
            assert!(cx.debug_bounds("undo acme/widgets").is_some());
            assert!(cx.debug_bounds("unlink acme/widgets").is_none());
            assert_eq!(
                harness.note(cx).as_deref(),
                Some("2 open PRs will leave this workspace")
            );
            assert!(cx.debug_bounds("leaving note").is_some());
            assert_eq!(harness.change(cx), (vec![], vec![repository_id(WIDGETS)]));
        }

        #[gpui::test]
        fn should_say_one_open_pr_in_the_singular(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(offer(), cx);

            harness.click("unlink acme/gadgets", cx);

            assert_eq!(
                harness.note(cx).as_deref(),
                Some("1 open PR will leave this workspace")
            );
        }

        #[gpui::test]
        fn should_keep_the_row_linked_on_undo(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(offer(), cx);
            harness.click("unlink acme/widgets", cx);

            harness.click("undo acme/widgets", cx);

            assert_eq!(harness.rows(cx)[1], (WIDGETS.to_owned(), Staged::Linked));
            assert_eq!(harness.note(cx), None);
            assert!(cx.debug_bounds("leaving note").is_none());
            assert_eq!(harness.change(cx), (vec![], vec![]));
        }

        #[gpui::test]
        fn should_drop_a_picked_row_on_its_unlink(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(offer(), cx);
            harness.type_in("tools", cx);
            harness.click("pick acme/tools", cx);
            assert_eq!(harness.change(cx), (vec!["acme/tools".to_owned()], vec![]));

            harness.click("unlink acme/tools", cx);

            assert_eq!(harness.rows(cx).len(), 2);
            assert_eq!(harness.change(cx), (vec![], vec![]));
        }

        #[gpui::test]
        fn should_note_nothing_for_a_repository_with_no_open_prs(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(
                Offer {
                    open_prs: HashMap::new(),
                    ..offer()
                },
                cx,
            );

            harness.click("unlink acme/widgets", cx);

            assert_eq!(harness.note(cx), None);
        }
    }

    mod picker {
        use super::*;

        #[gpui::test]
        fn should_offer_your_prs_repositories_but_linked_and_unreachable_ones(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(offer(), cx);

            harness.type_in("", cx);

            assert!(harness.picker.read_with(cx, |p, _| p.is_open()));
            assert_eq!(
                picks(&harness.entries(cx)),
                [SECRETS.to_owned(), "acme/tools".to_owned()]
            );
            assert!(cx.debug_bounds("from your pull requests").is_some());
            assert!(cx.debug_bounds("lock acme/secrets").is_some());
            assert!(cx.debug_bounds("not listed").is_some());
        }

        #[gpui::test]
        fn should_filter_as_you_type_ignoring_case(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(offer(), cx);

            harness.type_in("TOO", cx);

            assert_eq!(picks(&harness.entries(cx)), ["acme/tools"]);
            assert!(cx.debug_bounds("pick acme/secrets").is_none());
        }

        #[gpui::test]
        fn should_filter_by_a_pasted_url(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(offer(), cx);

            harness.type_in("https://github.com/acme/tools", cx);

            assert_eq!(picks(&harness.entries(cx)), ["acme/tools"]);
        }

        #[gpui::test]
        fn should_stage_a_picked_repository_and_clear_the_field(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(offer(), cx);
            harness.type_in("tools", cx);

            harness.click("pick acme/tools", cx);

            assert_eq!(
                harness.rows(cx)[2],
                ("acme/tools".to_owned(), Staged::Linking)
            );
            assert_eq!(picks(&harness.entries(cx)), [SECRETS]);
            let typed = harness
                .picker
                .read_with(cx, |p, cx| p.input.read(cx).value());
            assert_eq!(typed, "");
        }

        #[gpui::test]
        fn should_offer_a_typed_repository_it_does_not_list(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(offer(), cx);

            harness.type_in("https://github.com/octo/hello", cx);

            assert_eq!(picks(&harness.entries(cx)), ["Link octo/hello"]);
            assert!(cx.debug_bounds("link typed").is_some());
        }

        #[gpui::test]
        fn should_offer_no_typed_row_for_one_it_lists_or_links(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(offer(), cx);

            harness.type_in("ACME/Tools", cx);
            assert_eq!(picks(&harness.entries(cx)), ["acme/tools"]);
            harness.type_in("acme/widgets", cx);
            assert_eq!(picks(&harness.entries(cx)), Vec::<String>::new());
            assert!(cx.debug_bounds("no match").is_some());
        }

        #[gpui::test]
        fn should_link_a_typed_repository_github_shows(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(offer(), cx);
            harness.api.on_github(agentdeck_client::types::Repository {
                private: true,
                ..repository("octo/hello")
            });
            harness.type_in("octo/hello", cx);

            harness.click("link typed", cx);

            assert_eq!(
                *harness.api.resolved.lock().unwrap(),
                ["octo/hello".parse::<RepositoryName>().unwrap()]
            );
            assert_eq!(
                harness.rows(cx)[2],
                ("octo/hello".to_owned(), Staged::Linking)
            );
            assert!(
                harness
                    .picker
                    .read_with(cx, |p, _| p.rows()[2].repository.private)
            );
            assert_eq!(harness.change(cx), (vec!["octo/hello".to_owned()], vec![]));
            assert_eq!(
                harness.picker.read_with(cx, |p, _| p.problem().cloned()),
                None
            );
        }

        #[gpui::test]
        fn should_say_github_cannot_see_a_typed_repository(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(offer(), cx);
            harness.type_in("octo/nope", cx);

            harness.click("link typed", cx);

            assert_eq!(
                harness.picker.read_with(cx, |p, _| p.problem().cloned()),
                Some("GitHub can't see octo/nope. Check the name or your token's access.".into())
            );
            assert!(cx.debug_bounds("picker error").is_some());
            assert!(!harness.picker.read_with(cx, |p, _| p.is_open()));
            assert_eq!(harness.rows(cx).len(), 2);
            assert_eq!(harness.change(cx), (vec![], vec![]));
            harness.type_in("octo/nop", cx);
            assert_eq!(
                harness.picker.read_with(cx, |p, _| p.problem().cloned()),
                None
            );
        }

        #[gpui::test]
        fn should_pick_the_first_entry_on_enter(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(offer(), cx);
            harness.type_in("secrets", cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(harness.rows(cx)[2], (SECRETS.to_owned(), Staged::Linking));
        }

        #[gpui::test]
        fn should_take_the_keyboard_and_open_when_focused(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(offer(), cx);

            harness.picker.update(cx, |p, cx| p.focus(cx));
            harness.settle(cx);

            let focused = cx.update(|window, cx| {
                let input = harness.picker.read(cx).input.read(cx);
                input.focus_handle(cx).is_focused(window)
            });
            assert!(focused);
            assert!(harness.picker.read_with(cx, |p, _| p.is_open()));
        }
    }

    mod suggested {
        use super::*;

        /// [`offer`] with the Suggestions website, then infra, which you also have PRs in.
        fn suggesting() -> Offer {
            Offer {
                suggested: vec![choice("acme/infra"), choice("acme/website")],
                from_prs: [offer().from_prs, vec![choice("acme/infra")]].concat(),
                ..offer()
            }
        }

        fn shows(selector: &str, cx: &mut VisualTestContext) -> bool {
            cx.debug_bounds(selector.to_owned().leak()).is_some()
        }

        #[gpui::test]
        fn should_list_the_suggestions_first_then_your_prs_repositories(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(suggesting(), cx);

            assert_eq!(
                harness.entries(cx),
                [
                    Entry::Suggested(choice("acme/infra")),
                    Entry::Suggested(choice("acme/website")),
                    Entry::Pick(Choice {
                        private: true,
                        ..choice(SECRETS)
                    }),
                    Entry::Pick(choice("acme/tools")),
                ]
            );
        }

        #[gpui::test]
        fn should_head_them_suggested_above_from_your_pull_requests(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(suggesting(), cx);
            harness.type_in("", cx);

            let suggested = cx.debug_bounds("suggested").expect("the Suggested heading");
            let first = cx.debug_bounds("suggested acme/infra").expect("infra");
            let from_prs = cx
                .debug_bounds("from your pull requests")
                .expect("its heading");
            assert!(suggested.bottom() <= first.top());
            assert!(first.bottom() <= from_prs.top());
            assert!(!shows("pick acme/infra", cx), "listed once, as Suggested");
        }

        #[gpui::test]
        fn should_stage_a_suggestion_picked_from_the_list(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(suggesting(), cx);
            harness.type_in("", cx);

            let at = cx.debug_bounds("suggested acme/website").expect("website");
            cx.simulate_click(at.center(), Modifiers::default());
            harness.settle(cx);

            assert_eq!(
                harness.rows(cx).last(),
                Some(&("acme/website".to_owned(), Staged::Linking))
            );
            assert!(
                !harness
                    .entries(cx)
                    .contains(&Entry::Suggested(choice("acme/website")))
            );
        }

        #[gpui::test]
        fn should_filter_them_as_you_type(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(suggesting(), cx);

            harness.type_in("WEB", cx);

            assert_eq!(
                harness.entries(cx),
                [Entry::Suggested(choice("acme/website"))]
            );
        }

        #[gpui::test]
        fn should_pick_the_first_suggestion_on_enter(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(suggesting(), cx);
            harness.type_in("acme", cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(
                harness.rows(cx).last(),
                Some(&("acme/infra".to_owned(), Staged::Linking))
            );
        }
    }

    mod signed_out {
        use super::*;

        #[gpui::test]
        fn should_turn_typed_names_off_and_say_why(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(
                Offer {
                    signed_in: false,
                    ..offer()
                },
                cx,
            );

            harness.type_in("octo/hello", cx);

            assert_eq!(picks(&harness.entries(cx)), Vec::<String>::new());
            assert!(cx.debug_bounds("signed out").is_some());
            assert!(cx.debug_bounds("not listed").is_none());
            assert!(cx.debug_bounds("link typed").is_none());
            harness.type_in("tools", cx);
            assert_eq!(picks(&harness.entries(cx)), ["acme/tools"]);
        }
    }

    mod all_repositories {
        use super::*;

        fn private(repo: &str) -> Choice {
            Choice {
                private: true,
                ..choice(repo)
            }
        }

        /// What GitHub lists: tools and secrets (also your PRs'), widgets (linked), lost
        /// (Unreachable) and three only GitHub lists, api, billing (private) and docs.
        fn github() -> Vec<Choice> {
            vec![
                choice("acme/api"),
                private("acme/billing"),
                choice("acme/docs"),
                choice(LOST),
                private(SECRETS),
                choice("acme/tools"),
                choice(WIDGETS),
            ]
        }

        fn shows(selector: &str, cx: &mut VisualTestContext) -> bool {
            cx.debug_bounds(selector.to_owned().leak()).is_some()
        }

        fn loading(repos: &[Choice], total: Option<u32>) -> GithubRepositoryList {
            GithubRepositoryList {
                status: GithubListStatus::Loading,
                total,
                ..loaded(repos)
            }
        }

        fn status(status: GithubListStatus) -> GithubRepositoryList {
            GithubRepositoryList {
                status,
                ..loaded(&[])
            }
        }

        #[gpui::test]
        fn should_list_them_after_the_other_sections_but_those_listed_linked_or_unreachable(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open_with(offer(), loaded(&github()), cx);
            harness.type_in("", cx);

            assert_eq!(
                picks(&harness.entries(cx)),
                [
                    SECRETS,
                    "acme/tools",
                    "All acme/api",
                    "All acme/billing",
                    "All acme/docs"
                ]
            );
            let from_prs = cx.debug_bounds("pick acme/tools").expect("tools");
            let heading = cx
                .debug_bounds("all your repositories")
                .expect("its heading");
            let first = cx.debug_bounds("all acme/api").expect("api");
            assert!(from_prs.bottom() <= heading.top());
            assert!(heading.bottom() <= first.top());
            assert!(shows("lock acme/billing", cx));
            assert!(!shows("lock acme/api", cx));
            assert!(shows("not listed", cx));
            assert!(!shows("loading your repositories", cx));
        }

        #[gpui::test]
        fn should_ask_the_daemon_to_load_them_again_each_time_it_opens(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open_with(offer(), loaded(&github()), cx);
            assert_eq!(harness.api.github_list_refreshes(), 1);

            cx.update(|window, cx| {
                harness
                    .picker
                    .update(cx, |p, cx| p.reset(offer(), window, cx))
            });
            harness.settle(cx);

            assert_eq!(harness.api.github_list_refreshes(), 2);
        }

        #[gpui::test]
        fn should_filter_them_as_you_type(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open_with(offer(), loaded(&github()), cx);

            harness.type_in("BILL", cx);

            assert_eq!(picks(&harness.entries(cx)), ["All acme/billing"]);
        }

        #[gpui::test]
        fn should_stage_one_picked_from_the_list(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open_with(offer(), loaded(&github()), cx);
            harness.type_in("docs", cx);

            harness.click("all acme/docs", cx);

            assert_eq!(
                harness.rows(cx).last(),
                Some(&("acme/docs".to_owned(), Staged::Linking))
            );
            assert_eq!(harness.change(cx), (vec!["acme/docs".to_owned()], vec![]));
        }

        #[gpui::test]
        fn should_pick_one_on_enter(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open_with(offer(), loaded(&github()), cx);
            harness.type_in("api", cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(
                harness.rows(cx).last(),
                Some(&("acme/api".to_owned(), Staged::Linking))
            );
        }

        #[gpui::test]
        fn should_offer_no_typed_row_for_a_name_it_lists(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open_with(offer(), loaded(&github()), cx);

            harness.type_in("https://github.com/acme/billing", cx);

            assert_eq!(picks(&harness.entries(cx)), ["All acme/billing"]);
            assert!(!shows("link typed", cx));
        }

        #[gpui::test]
        fn should_show_what_has_loaded_and_how_far_loading_has_got(cx: &mut TestAppContext) {
            let (harness, cx) =
                Harness::open_with(offer(), loading(&github()[..2], Some(1_104)), cx);
            harness.type_in("", cx);

            assert_eq!(
                picks(&harness.entries(cx)),
                [SECRETS, "acme/tools", "All acme/api", "All acme/billing"]
            );
            let line = cx
                .debug_bounds("loading your repositories")
                .expect("the loading line");
            let last = cx.debug_bounds("all acme/billing").expect("billing");
            assert!(last.bottom() <= line.top());
            let read = harness.picker.read_with(cx, |p, _| p.github().clone());
            assert_eq!(
                read,
                GithubList::Loading {
                    repositories: vec![choice("acme/api"), private("acme/billing")],
                    loaded: 2,
                    total: Some(1_104),
                }
            );
        }

        #[gpui::test]
        fn should_say_it_is_loading_before_the_first_page(cx: &mut TestAppContext) {
            let (harness, cx) =
                Harness::open_with(offer(), status(GithubListStatus::NotLoaded), cx);
            harness.type_in("", cx);

            assert!(shows("loading your repositories", cx));
            assert!(!shows("no match", cx));
        }

        #[gpui::test]
        fn should_read_them_again_as_pages_arrive(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open_with(offer(), loading(&github()[..1], Some(300)), cx);
            harness.type_in("", cx);
            assert_eq!(picks(&harness.entries(cx))[2..], ["All acme/api"]);

            harness.api.set_github_list(loaded(&github()));
            harness.github_changed(cx);

            assert_eq!(
                picks(&harness.entries(cx))[2..],
                ["All acme/api", "All acme/billing", "All acme/docs"]
            );
            assert!(!shows("loading your repositories", cx));
        }

        #[gpui::test]
        fn should_say_it_could_not_load_them_and_keep_the_rest_working(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open_with(offer(), status(GithubListStatus::Failed), cx);
            harness.type_in("", cx);

            assert_eq!(picks(&harness.entries(cx)), [SECRETS, "acme/tools"]);
            assert!(shows("all your repositories", cx));
            assert!(shows("could not load", cx));
            assert!(shows("retry", cx));
            assert!(shows("not listed", cx));
            harness.type_in("octo/hello", cx);
            assert_eq!(picks(&harness.entries(cx)), ["Link octo/hello"]);
        }

        #[gpui::test]
        fn should_say_it_could_not_load_them_when_the_daemon_cannot_be_read(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open_with(offer(), loaded(&github()), cx);
            harness.api.fail_github_list(Some("connection refused"));

            harness.github_changed(cx);
            harness.type_in("", cx);

            assert!(shows("could not load", cx));
            assert_eq!(picks(&harness.entries(cx)), [SECRETS, "acme/tools"]);
        }

        #[gpui::test]
        fn should_load_them_again_on_retry(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open_with(offer(), status(GithubListStatus::Failed), cx);
            harness.type_in("", cx);
            harness.api.set_github_list(loaded(&github()));

            harness.click("retry", cx);

            assert_eq!(harness.api.github_list_refreshes(), 2);
            assert!(!shows("could not load", cx));
            assert!(shows("all acme/api", cx));
        }

        #[gpui::test]
        fn should_be_replaced_by_sign_in_while_signed_out(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open_with(
                Offer {
                    signed_in: false,
                    ..offer()
                },
                loaded(&github()),
                cx,
            );
            harness.type_in("", cx);

            assert_eq!(picks(&harness.entries(cx)), [SECRETS, "acme/tools"]);
            assert!(!shows("all your repositories", cx));
            assert!(shows("signed out", cx));
            assert!(!shows("not listed", cx));
        }

        #[gpui::test]
        fn should_be_replaced_by_sign_in_when_the_daemon_has_no_token(cx: &mut TestAppContext) {
            let (harness, cx) =
                Harness::open_with(offer(), status(GithubListStatus::SignedOut), cx);
            harness.type_in("", cx);

            assert!(!shows("all your repositories", cx));
            assert!(shows("signed out", cx));
        }

        #[gpui::test]
        fn should_draw_only_the_rows_in_view_of_a_long_list(cx: &mut TestAppContext) {
            let many: Vec<Choice> = (0..1_100)
                .map(|i| choice(&format!("acme/repo-{i:04}")))
                .collect();
            let (harness, cx) = Harness::open_with(offer(), loaded(&many), cx);
            harness.type_in("", cx);

            assert_eq!(harness.entries(cx).len(), 2 + 1_100);
            assert!(shows("all acme/repo-0000", cx));
            assert!(!shows("all acme/repo-1099", cx));
            let list = cx.debug_bounds("picker list").expect("the list");
            assert!(list.size.height < px(320.), "{:?}", list.size.height);
            assert!(shows("not listed", cx));
        }
    }

    mod long_list {
        use super::*;

        #[gpui::test]
        fn should_keep_the_loading_line_in_view_under_rows_that_scroll(cx: &mut TestAppContext) {
            let many: Vec<Choice> = (0..400)
                .map(|i| choice(&format!("acme/repo-{i:04}")))
                .collect();
            let list = GithubRepositoryList {
                status: GithubListStatus::Loading,
                total: Some(1_104),
                ..loaded(&many)
            };
            let (harness, cx) = Harness::open_with(offer(), list, cx);
            harness.type_in("", cx);

            let line = cx
                .debug_bounds("loading your repositories")
                .expect("the loading line, in view");
            let footer = cx.debug_bounds("not listed").expect("the last line");
            assert!(line.bottom() <= footer.top());
            assert!(cx.debug_bounds("all acme/repo-0399").is_none());
        }
    }

    mod loading_line {
        use super::*;

        #[test]
        fn should_count_what_loaded_of_about_the_total_to_the_hundred() {
            assert_eq!(
                loading_line(400, Some(1_104)),
                "Loading your repositories… 400 of about 1,100"
            );
            assert_eq!(
                loading_line(1_000, Some(1_150)),
                "Loading your repositories… 1,000 of about 1,200"
            );
        }

        #[test]
        fn should_say_no_count_before_page_one() {
            assert_eq!(loading_line(0, None), "Loading your repositories…");
            assert_eq!(loading_line(0, Some(300)), "Loading your repositories…");
        }

        #[test]
        fn should_count_a_list_under_a_hundred_exactly() {
            assert_eq!(
                loading_line(40, Some(64)),
                "Loading your repositories… 40 of about 64"
            );
        }
    }
}
