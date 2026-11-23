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
//! The picker lists From your pull requests (the Repositories of your kept PRs), leaving out
//! what the Workspace links and Unreachable ones; typing filters it. Typed text naming a
//! Repository (`owner/name` or its GitHub URL) that isn't listed offers a "Link owner/name" row,
//! which asks GitHub ([`Api::resolve_repository`]); signed out, typed names are off.

use std::collections::HashMap;
use std::sync::Arc;

use agentdeck_client::ApiError;
use agentdeck_core::{PrState, RepositoryId, RepositoryName, WorkspaceId};
use brrritscold_core_gpui::{
    ActiveTheme as _, Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, FieldError, Icon,
    IconName, Input, InputState, Label,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, shadow, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Anchor, App, AppContext as _, BoxShadow, Context, Entity, EventEmitter,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, point, px,
};
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::overview::Snapshot;
use crate::workspace::failed;

/// The picker's placeholder.
pub const PLACEHOLDER: &str = "Link a repository…";

/// The heading over the Repositories of your kept PRs.
pub const FROM_PRS: &str = "From your pull requests";

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
    /// The Repositories of your kept PRs, by name ignoring case.
    pub from_prs: Vec<Choice>,
    /// Each Repository's open PRs, counted as the sidebar counts them: drafts in, Unreachable
    /// and Finished ones out.
    pub open_prs: HashMap<RepositoryId, usize>,
    /// Whether GitHub accepts the token, so typed names can be looked up.
    pub signed_in: bool,
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
        // Unknown until PR status is read: let GitHub answer for the token.
        let signed_in = (snapshot.pr_sync.as_ref())
            .is_none_or(|s| s.login != agentdeck_client::types::GhLogin::LoggedOut);
        Offer {
            linked,
            from_prs,
            open_prs,
            signed_in,
        }
    }
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
    /// A Repository of your kept PRs.
    Pick(Choice),
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
    _input: Subscription,
}

impl EventEmitter<Staging> for LinkPicker {}

impl LinkPicker {
    /// An empty part that asks GitHub through `api`.
    pub fn new(api: Arc<dyn Api>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(PLACEHOLDER));
        let _input = cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
            InputEvent::Change => {
                this.problem = None;
                this.open = true;
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
            _input,
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
        cx.emit(Staging);
        cx.notify();
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
        let picks: Vec<&Choice> = (self.offer.from_prs.iter())
            .filter(|c| !c.unreachable && !held(c.id))
            .filter(|c| c.name.to_string().to_lowercase().contains(&filter))
            .collect();
        let mut entries: Vec<Entry> = picks.iter().map(|c| Entry::Pick((*c).clone())).collect();
        if let Some(named) = named.filter(|_| self.offer.signed_in) {
            let same = |name: &RepositoryName| same_name(name, &named);
            let listed = picks.iter().any(|c| same(&c.name))
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
                    Err(err) => match err.downcast_ref::<ApiError>() {
                        Some(ApiError {
                            message,
                            code: Some(_),
                        }) => this.problem = Some(message.clone().into()),
                        _ => failed(format!("Couldn't look up {name}"), &err, window, cx),
                    },
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
            Some(Entry::Pick(choice)) => self.pick(choice, window, cx),
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

    /// The open list (Penpot `agentdeck/LinkPickerList`), drawn as core-gpui's
    /// `ComboboxContent`: From your pull requests, the typed row, then the last line.
    fn list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let entries = self.entries(cx);
        let picks: Vec<Choice> = (entries.iter())
            .filter_map(|e| match e {
                Entry::Pick(choice) => Some(choice.clone()),
                Entry::Typed(_) => None,
            })
            .collect();
        let typed = entries.iter().find_map(|e| match e {
            Entry::Typed(name) => Some(name.clone()),
            Entry::Pick(_) => None,
        });
        let mut shadows = vec![BoxShadow {
            color: s.foreground.opacity(0.1),
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(1.),
            inset: false,
        }];
        shadows.extend_from_slice(shadow::MD);
        // ComboboxItem: flex items-center gap-2 rounded-sm py-1.5 pr-8 pl-2 text-sm,
        // hover:bg-accent
        let item = |id: SharedString| {
            div()
                .id(id)
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
        let line = |text: &'static str, selector: &'static str| {
            div()
                .debug_selector(move || selector.into())
                .px(space::S2)
                .py(space::S1_5)
                .text_size(font_size::XS)
                .text_color(s.muted_foreground)
                .child(text)
        };
        let looking_up = self.looking_up.clone();
        div()
            .debug_selector(|| "picker list".into())
            .id("link-picker-list")
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
            .when(!picks.is_empty(), |list| {
                list.child(line(FROM_PRS, "from your pull requests"))
                    .children(picks.into_iter().map(|choice| {
                        let name = choice.name.to_string();
                        let selector = format!("pick {name}");
                        item(SharedString::from(format!("pick-{}", choice.id)))
                            .debug_selector(move || selector.clone())
                            .when(choice.private, |d| d.child(lock(&name, s.muted_foreground)))
                            .child(name)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.pick(choice.clone(), window, cx)
                            }))
                    }))
            })
            .when_some(typed, |list, name| {
                let busy = looking_up.as_ref().is_some_and(|n| same_name(n, &name));
                let label = match busy {
                    true => format!("Looking up {name}…"),
                    false => format!("Link {name}"),
                };
                list.child(
                    item("link-typed".into())
                        .debug_selector(|| "link typed".into())
                        .child(Icon::new(IconName::Plus).size(px(14.)))
                        .child(label)
                        .when(busy, |d| d.opacity(0.5))
                        .when(!busy, |d| {
                            d.on_click(cx.listener(move |this, _, window, cx| {
                                this.pick_typed(name.clone(), window, cx)
                            }))
                        }),
                )
            })
            .when(entries.is_empty(), |list| {
                // ComboboxEmpty: flex w-full justify-center py-2 text-sm text-muted-foreground
                list.child(
                    div()
                        .debug_selector(|| "no match".into())
                        .flex()
                        .w_full()
                        .justify_center()
                        .py(space::S2)
                        .text_size(font_size::SM)
                        .text_color(s.muted_foreground)
                        .child(NO_MATCH),
                )
            })
            .child(
                // ComboboxSeparator: -mx-1 my-1 h-px bg-border
                div().mx(-space::S1).my(space::S1).h(px(1.)).bg(s.border),
            )
            .child(match self.offer.signed_in {
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
        let field = div()
            .debug_selector(|| "link picker".into())
            .child(Input::new(&self.input).aria_invalid(problem.is_some()));
        let field = gpui_base::Popup::new("link-picker", field)
            .anchor(Anchor::TopLeft)
            .offset(SIDE_OFFSET)
            .w_full();
        let field = match self.open {
            true => field.content(div().w_full().child(self.list(cx))),
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
    use agentdeck_client::types::PullRequest;
    use brrritscold_core_gpui::Root;
    use gpui::{Focusable as _, Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::overview::fixtures::{id, links, pr_sync, pull_request, repository_id};

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
        }
    }

    impl Harness {
        pub(crate) fn open(
            offer: Offer,
            cx: &mut TestAppContext,
        ) -> (Harness, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            let api = Arc::new(FakeApi::default());
            let mut picker = None;
            let window = cx.add_window(|window, cx| {
                let api: Arc<dyn Api> = api.clone();
                let view = cx.new(|cx| LinkPicker::new(api, window, cx));
                picker = Some(view.clone());
                Root::new(view, window, cx)
            });
            let cx = VisualTestContext::from_window(*window, cx).into_mut();
            cx.update(|window, _| window.activate_window());
            let picker = picker.unwrap();
            cx.update(|window, cx| picker.update(cx, |p, cx| p.reset(offer, window, cx)));
            let harness = Harness { api, picker };
            harness.settle(cx);
            (harness, cx)
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
                Entry::Pick(choice) => choice.name.to_string(),
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
}
