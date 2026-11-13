//! The selected workspace (spec §11), Penpot "Main window" `Centre`: its [`TopBar`], then a
//! row of tabs over the selected tab's split tree, a [`PaneView`] at each leaf.
//!
//! It reads the workspace's tabs, panes and linked tasks when it opens and again whenever the
//! daemon reports a tab, pane or task `changed`. Each shown pane keeps its view, and so its
//! terminal, across reads; leaving a tab drops its panes' views, which detaches them.
//!
//! It handles the tab and pane shortcuts in [`keymap`]'s workspace context, opens a tab with a
//! shell from its `NewTab` button or shortcut, and carries out each pane menu's
//! [`PaneRequest`], or the same request by shortcut on the focused pane. Closing a pane that is
//! still running asks first. A workspace with no tabs when it opens gets a shell tab.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use agentdeck_client::types::Tab;
use agentdeck_client::ws;
use agentdeck_core::ws::{Entity as Changed, ServerMessage};
use agentdeck_core::{AgentState, Layout, PaneId, SplitDirection, TabId, WorkspaceId};
use brrritscold_core_gpui::{
    ActiveTheme as _, AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent,
    AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle, Button,
    ButtonSize, ButtonVariant, DialogHandle, IconName, Separator, Tabs, TabsList, TabsListVariant,
    TabsTrigger, Toast, ToastDescription, ToastExt as _, ToastTitle, ToastType,
};
use brrritscold_core_gpui_tokens::primitive::space;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Div, Entity, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Task, Window, div, relative,
};

use crate::api::{Api, PaneAt, Split, WorkspacePanes};
use crate::connection::{Connection, DaemonEvent};
use crate::keymap::{
    self, ClosePane, NewTab, NextPane, NextTab, PreviousPane, PreviousTab, SelectTab, SplitDown,
    SplitRight,
};
use crate::pane::{PaneRequest, PaneView};
use crate::rereads::Rereads;
use crate::top_bar::TopBar;

/// Where the read stands.
enum Status {
    /// Reading.
    Loading,
    /// The read failed; why.
    Failed(SharedString),
    /// Showing what was read.
    Ready,
}

/// One workspace's tabs and panes.
pub struct WorkspaceView {
    workspace: WorkspaceId,
    top_bar: TopBar,
    api: Arc<dyn Api>,
    connection: Entity<Connection>,
    sender: ws::Sender,
    status: Status,
    /// The last read.
    read: WorkspacePanes,
    /// The tab shown.
    selected: Option<TabId>,
    /// The selected tab's panes.
    panes: HashMap<PaneId, Entity<PaneView>>,
    /// Each shown pane's menu requests.
    requests: HashMap<PaneId, Subscription>,
    /// A pane to focus once a read shows it: the new side of a split.
    focus_next: Option<PaneId>,
    /// The running pane the close confirmation asks about, while it is open.
    closing: Option<PaneAt>,
    /// Opens and closes the close confirmation.
    close_dialog: DialogHandle,
    /// The panes whose close is under way; asking again meanwhile does nothing.
    closing_now: HashSet<PaneId>,
    /// The last read started.
    reading: Option<Task<()>>,
    /// Whether a read has landed yet: the first one opens a shell tab in an empty workspace.
    read_once: bool,
    rereads: Rereads,
    /// Holds the keyboard while the tab shown has no pane to.
    focus: FocusHandle,
    _events: Subscription,
}

impl WorkspaceView {
    /// `workspace`, headed by `top_bar`, read through `api` and attached over `connection`'s
    /// socket through `sender`. It starts reading at once.
    pub fn new(
        workspace: WorkspaceId,
        top_bar: TopBar,
        api: Arc<dyn Api>,
        connection: Entity<Connection>,
        sender: ws::Sender,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let events = cx.subscribe_in(&connection, window, |this, _, event, window, cx| {
            if let DaemonEvent::Message(ServerMessage::Changed {
                entity: Changed::Tab | Changed::Pane | Changed::Task,
                ..
            }) = event
            {
                this.reread(window, cx);
            }
        });
        let mut view = WorkspaceView {
            workspace,
            top_bar,
            api,
            connection,
            sender,
            status: Status::Loading,
            read: WorkspacePanes::default(),
            selected: None,
            panes: HashMap::new(),
            requests: HashMap::new(),
            focus_next: None,
            closing: None,
            close_dialog: DialogHandle::new(false),
            closing_now: HashSet::new(),
            reading: None,
            read_once: false,
            rereads: Rereads::default(),
            focus: cx.focus_handle(),
            _events: events,
        };
        view.reread(window, cx);
        view
    }

    /// The workspace it shows.
    pub fn workspace(&self) -> WorkspaceId {
        self.workspace
    }

    /// Heads it with `top_bar`, as the workspace now reads.
    pub fn set_top_bar(&mut self, top_bar: TopBar, cx: &mut Context<Self>) {
        if self.top_bar != top_bar {
            self.top_bar = top_bar;
            cx.notify();
        }
    }

    /// The bar heading it.
    #[cfg(test)]
    pub fn top_bar(&self) -> &TopBar {
        &self.top_bar
    }

    /// Opens a tab with a shell in the workspace's dir, then shows it.
    pub fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let opening = (self.api).new_shell_tab(self.workspace, self.top_bar.dir().to_string(), cx);
        cx.spawn_in(window, async move |this, cx| {
            let opened = opening.await;
            let _ = this.update_in(cx, |this, window, cx| match opened {
                Ok(tab) => {
                    this.selected = Some(tab);
                    this.reread(window, cx);
                }
                Err(err) => failed("Couldn't open a tab", &err, window, cx),
            });
        })
        .detach();
    }

    /// Carries out `request` on the pane that has the keyboard, if one has: a shortcut.
    fn on_focused(&mut self, request: PaneRequest, window: &mut Window, cx: &mut Context<Self>) {
        let focused = (self.shown().into_iter())
            .find(|pane| pane.focus_handle(cx).contains_focused(window, cx));
        if let Some(pane) = focused {
            self.on_request(&pane, request, window, cx);
        }
    }

    fn on_request(
        &mut self,
        pane: &Entity<PaneView>,
        request: PaneRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane = pane.read(cx).pane();
        let at = PaneAt {
            workspace: self.workspace,
            tab: pane.tab_id,
            pane: pane.id,
        };
        match request {
            PaneRequest::Split(direction) => {
                let cwd = pane.cwd.clone();
                self.split_pane(Split { at, direction, cwd }, window, cx);
            }
            PaneRequest::Close if pane.state == AgentState::Exited => {
                self.close_pane(at, window, cx);
            }
            PaneRequest::Close => {
                self.closing = Some(at);
                self.close_dialog.open(window, cx);
                cx.notify();
            }
        }
    }

    /// Splits a pane as `split` says, then focuses the new side.
    fn split_pane(&mut self, split: Split, window: &mut Window, cx: &mut Context<Self>) {
        let splitting = self.api.split_pane(split, cx);
        cx.spawn_in(window, async move |this, cx| {
            let split = splitting.await;
            let _ = this.update_in(cx, |this, window, cx| match split {
                Ok(pane) => {
                    this.focus_next = Some(pane);
                    this.reread(window, cx);
                }
                Err(err) => failed("Couldn't split the pane", &err, window, cx),
            });
        })
        .detach();
    }

    /// Closes the pane the confirmation asks about: what its `Close` button does.
    pub fn confirm_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.closing.take();
        self.close_dialog.close(window, cx);
        if let Some(pane) = pane {
            self.close_pane(pane, window, cx);
        }
        cx.notify();
    }

    /// The running pane the close confirmation asks about, while it is open.
    #[cfg(test)]
    pub fn closing(&self) -> Option<PaneId> {
        let open = self.close_dialog.is_open();
        self.closing.filter(|_| open).map(|at| at.pane)
    }

    /// Closes `at`, dimming it until it goes. A close already under way isn't repeated.
    fn close_pane(&mut self, at: PaneAt, window: &mut Window, cx: &mut Context<Self>) {
        if !self.closing_now.insert(at.pane) {
            return;
        }
        self.set_closing(at.pane, true, cx);
        let closing = self.api.close_pane(at, cx);
        cx.spawn_in(window, async move |this, cx| {
            let closed = closing.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.closing_now.remove(&at.pane);
                match closed {
                    Ok(()) => this.reread(window, cx),
                    Err(err) => {
                        this.set_closing(at.pane, false, cx);
                        failed("Couldn't close the pane", &err, window, cx);
                    }
                }
            });
        })
        .detach();
    }

    fn set_closing(&self, pane: PaneId, closing: bool, cx: &mut Context<Self>) {
        if let Some(view) = self.panes.get(&pane) {
            view.update(cx, |view, cx| view.set_closing(closing, cx));
        }
    }

    /// Shows the tab `tab`, if the workspace has it.
    pub fn select(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected != Some(tab) && self.read.tabs.iter().any(|t| t.id == tab) {
            self.selected = Some(tab);
            self.sync(window, cx);
            cx.notify();
        }
    }

    /// The tab it shows.
    #[cfg(test)]
    pub fn selected(&self) -> Option<TabId> {
        self.selected
    }

    /// Shows the tab `step` places along from the one shown, wrapping around.
    fn step_tab(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let tabs = &self.read.tabs;
        let Some(at) =
            (self.selected).and_then(|selected| tabs.iter().position(|tab| tab.id == selected))
        else {
            return;
        };
        let to = (at as isize + step).rem_euclid(tabs.len() as isize) as usize;
        self.select(tabs[to].id, window, cx);
    }

    /// Shows the tab at `index`, if there is one.
    fn select_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.read.tabs.get(index) {
            self.select(tab.id, window, cx);
        }
    }

    /// Focuses the pane `step` places along from the focused one in layout order, wrapping
    /// around; the first or last pane when none has focus.
    fn step_pane(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.shown();
        let focused = shown
            .iter()
            .position(|pane| pane.focus_handle(cx).contains_focused(window, cx));
        let to = match focused {
            Some(at) => (at as isize + step).rem_euclid(shown.len() as isize) as usize,
            None if step > 0 => 0,
            None => shown.len().wrapping_sub(1),
        };
        if let Some(pane) = shown.get(to) {
            window.focus(&pane.focus_handle(cx), cx);
        }
    }

    /// The panes it shows, left to right and top to bottom.
    pub fn shown(&self) -> Vec<Entity<PaneView>> {
        self.layout()
            .map(|layout| layout.panes())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|pane| self.panes.get(&pane).cloned())
            .collect()
    }

    /// Reads the tabs and panes, or once more after the read in flight.
    fn reread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.rereads.ask() {
            return;
        }
        let reading = self.api.panes(self.workspace, cx);
        self.reading = Some(cx.spawn_in(window, async move |this, cx| {
            let result = reading.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.show(result, window, cx);
                cx.notify();
                if this.rereads.landed() {
                    this.reread(window, cx);
                }
            });
        }));
    }

    fn show(
        &mut self,
        read: anyhow::Result<WorkspacePanes>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut read = match read {
            Ok(read) => read,
            Err(err) => {
                self.status = Status::Failed(format!("Couldn't read the tabs: {err:#}").into());
                self.panes.clear();
                return;
            }
        };
        read.tabs.sort_by_key(|tab| tab.position);
        self.read = read;
        self.status = Status::Ready;
        let kept = self
            .selected
            .filter(|selected| self.read.tabs.iter().any(|tab| tab.id == *selected));
        self.selected = kept.or_else(|| self.read.tabs.first().map(|tab| tab.id));
        self.sync(window, cx);
        // A workspace opened with no tabs gets a shell; one emptied while shown stays empty.
        if !std::mem::replace(&mut self.read_once, true) && self.read.tabs.is_empty() {
            self.new_tab(window, cx);
        }
    }

    /// The selected tab's layout, if it has a pane.
    fn layout(&self) -> Option<&Layout> {
        let selected = self.selected?;
        let tab = self.read.tabs.iter().find(|tab| tab.id == selected)?;
        tab.layout.as_ref()
    }

    /// Makes `panes` the selected tab's: keeps and updates the views of panes still in it,
    /// makes the new ones, and drops the rest. A pane waiting for focus gets it once shown;
    /// else a tab shown afresh focuses its first pane, or the workspace itself when it has
    /// none, so the shortcuts still reach it.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.layout().map(Layout::panes).unwrap_or_default();
        let fresh = !wanted.iter().any(|pane| self.panes.contains_key(pane));
        let mut panes = HashMap::new();
        for id in &wanted {
            let Some(pane) = self.read.panes.iter().find(|pane| pane.id == *id).cloned() else {
                continue;
            };
            let task = (pane.task_id)
                .and_then(|task| self.read.tasks.iter().find(|t| t.id == task))
                .map(|task| SharedString::from(task.title.clone()));
            let view = match self.panes.remove(id) {
                Some(view) => {
                    view.update(cx, |view, cx| view.update(pane, task, cx));
                    view
                }
                None => {
                    let (connection, sender) = (&self.connection, self.sender.clone());
                    let view =
                        cx.new(|cx| PaneView::new(pane, task, connection, sender, window, cx));
                    let requests = cx.subscribe_in(&view, window, |this, pane, request, w, cx| {
                        this.on_request(pane, *request, w, cx);
                    });
                    self.requests.insert(*id, requests);
                    view
                }
            };
            panes.insert(*id, view);
        }
        self.panes = panes;
        self.requests.retain(|id, _| self.panes.contains_key(id));
        let waiting = self
            .focus_next
            .and_then(|pane| self.panes.get(&pane))
            .cloned();
        if let Some(pane) = waiting {
            self.focus_next = None;
            window.focus(&pane.focus_handle(cx), cx);
        } else if fresh {
            match wanted.first().and_then(|pane| self.panes.get(pane)) {
                Some(first) => window.focus(&first.focus_handle(cx), cx),
                None => window.focus(&self.focus, cx),
            }
        }
    }

    /// `text` centred in the space left, wrapping within it.
    fn message(&self, text: impl Into<SharedString>, cx: &mut Context<Self>) -> AnyElement {
        div()
            .size_full()
            .min_w_0()
            .flex()
            .items_center()
            .justify_center()
            .p(space::S8)
            .text_sm()
            .text_color(cx.theme().semantic().muted_foreground)
            .child(div().max_w_full().text_center().child(text.into()))
            .into_any_element()
    }

    /// Penpot `TabsRow`: a `line` tab list, then `NewTab`, `space.2` apart and `space.4` in
    /// from each side.
    fn tabs_row(&self, cx: &mut Context<Self>) -> Div {
        let list = self.read.tabs.iter().fold(
            TabsList::new().variant(TabsListVariant::Line),
            |list, tab: &Tab| list.trigger(TabsTrigger::new(tab.id.to_string(), tab.title.clone())),
        );
        let on_select = cx.listener(|this, value: &SharedString, window, cx| {
            if let Ok(tab) = value.parse() {
                this.select(tab, window, cx);
            }
        });
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(space::S2)
            .px(space::S4)
            .child(
                Tabs::new("workspace-tabs")
                    .when_some(self.selected, |tabs, tab| tabs.value(tab.to_string()))
                    .list(list)
                    .on_value_change(on_select),
            )
            // Penpot `NewTab`: ghost, icon-xs, `Plus`.
            .child(
                Button::new("new-tab")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::IconXs)
                    .icon(IconName::Plus)
                    .on_click(cx.listener(|this, _, window, cx| this.new_tab(window, cx))),
            )
    }

    /// Asks before closing a running pane: core-react's `AlertDialog`, opened by
    /// [`PaneRequest::Close`].
    fn close_confirmation(&self, cx: &mut Context<Self>) -> AlertDialog {
        let title = self
            .closing
            .and_then(|at| self.panes.get(&at.pane))
            .map(|pane| pane.read(cx).title());
        let what = title.map_or("What runs there".into(), |title| format!("“{title}”"));
        AlertDialog::new("close-pane")
            .handle(self.close_dialog.clone())
            .content(
                AlertDialogContent::new()
                    .header(
                        AlertDialogHeader::new()
                            .title(AlertDialogTitle::new("Close this pane?"))
                            .description(AlertDialogDescription::new(format!(
                                "{what} is still running. Closing the pane ends it."
                            ))),
                    )
                    .footer(
                        AlertDialogFooter::new()
                            .child(AlertDialogCancel::new("close-pane-cancel", "Cancel"))
                            .child(
                                AlertDialogAction::new("close-pane-confirm", "Close pane")
                                    .variant(ButtonVariant::Destructive)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.confirm_close(window, cx);
                                    })),
                            ),
                    ),
            )
    }

    /// The subtree `layout`, taking `grow` shares of its split's free space.
    fn split(&self, layout: &Layout, grow: f32) -> Div {
        let mut side = div().flex().min_w_0().min_h_0().flex_basis(relative(0.));
        side.style().flex_grow = Some(grow);
        match layout {
            Layout::Pane { pane } => side.children(self.panes.get(pane).cloned()),
            // Penpot `Panes` and `Stack`: the sides `space.2` apart.
            Layout::Split { split, ratio, a, b } => side
                .gap(space::S2)
                .when(*split == SplitDirection::Vertical, |side| side.flex_col())
                .child(self.split(a, ratio.get()))
                .child(self.split(b, 1. - ratio.get())),
        }
    }
}

/// Reports a failed action in a toast.
pub fn failed(title: &'static str, err: &anyhow::Error, window: &mut Window, cx: &mut gpui::App) {
    window.push_toast(
        Toast::new()
            .toast_type(ToastType::Error)
            .title(ToastTitle::new(title))
            .description(ToastDescription::new(format!("{err:#}"))),
        cx,
    );
}

impl Render for WorkspaceView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = div()
            .key_context(keymap::WORKSPACE)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.step_tab(1, window, cx)))
            .on_action(cx.listener(|this, _: &PreviousTab, window, cx| {
                this.step_tab(-1, window, cx);
            }))
            .on_action(
                cx.listener(|this, SelectTab(index): &SelectTab, window, cx| {
                    this.select_index(*index, window, cx);
                }),
            )
            .on_action(cx.listener(|this, _: &NextPane, window, cx| this.step_pane(1, window, cx)))
            .on_action(cx.listener(|this, _: &PreviousPane, window, cx| {
                this.step_pane(-1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NewTab, window, cx| this.new_tab(window, cx)))
            .on_action(cx.listener(|this, _: &SplitRight, window, cx| {
                this.on_focused(PaneRequest::Split(SplitDirection::Horizontal), window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitDown, window, cx| {
                this.on_focused(PaneRequest::Split(SplitDirection::Vertical), window, cx);
            }))
            .on_action(cx.listener(|this, _: &ClosePane, window, cx| {
                this.on_focused(PaneRequest::Close, window, cx);
            }))
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            // Penpot `Centre`: TopBar / TabsRow / Separator / Panes.
            .child(self.top_bar.clone())
            .child(self.close_confirmation(cx));
        match &self.status {
            Status::Loading => view,
            Status::Failed(reason) => view.child(self.message(reason.clone(), cx)),
            // The panes `space.2` in.
            Status::Ready => {
                view.child(self.tabs_row(cx))
                    .child(Separator::new())
                    .child(match self.layout() {
                        Some(layout) => self.split(layout, 1.).p(space::S2).into_any_element(),
                        None if self.read.tabs.is_empty() => {
                            self.message("No tabs in this workspace yet", cx)
                        }
                        None => self.message("No panes in this tab yet", cx),
                    })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use agentdeck_client::types::Pane;
    use agentdeck_client::ws::{Event, Remote};
    use agentdeck_core::{AgentState, Ratio, TaskKind};
    use brrritscold_core_gpui::{Root, Theme, ThemePreference};
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::connection::fake::FakeConnector;
    use crate::connection::{Connector, Status as Connected};
    use crate::overview::fixtures::{id, tab, task};
    use crate::pane::fixtures::shell_pane;

    struct Harness {
        api: Arc<FakeApi>,
        remote: Remote,
        _connection: Entity<Connection>,
        view: Entity<WorkspaceView>,
    }

    impl Harness {
        /// A window on workspace 1, whose tabs are `tabs` and whose panes are `panes`.
        fn open(
            tabs: Vec<Tab>,
            panes: Vec<Pane>,
            cx: &mut TestAppContext,
        ) -> (Harness, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            cx.update(crate::terminal::init);
            cx.update(crate::keymap::init);
            let api = Arc::new(FakeApi::default());
            api.set_tabs(id(1), tabs);
            api.set_panes(panes);
            let connector = Rc::new(FakeConnector::default());
            let remote = connector.then_connect_to(api.clone());
            let connector: Rc<dyn Connector> = connector;
            let connection = cx.update(|cx| cx.new(|cx| Connection::new(connector, cx)));
            cx.run_until_parked();
            let Connected::Connected(session) = connection.read_with(cx, |c, _| c.status().clone())
            else {
                panic!("the fake connects");
            };
            let mut view = None;
            let window = cx.add_window(|window, cx| {
                let workspace = cx.new(|cx| {
                    WorkspaceView::new(
                        id(1),
                        TopBar::new("agentdeck", "/w/agentdeck"),
                        session.api,
                        connection.clone(),
                        session.sender,
                        window,
                        cx,
                    )
                });
                view = Some(workspace.clone());
                Root::new(workspace, window, cx)
            });
            let cx = VisualTestContext::from_window(*window, cx).into_mut();
            cx.run_until_parked();
            let harness = Harness {
                api,
                remote,
                _connection: connection,
                view: view.unwrap(),
            };
            (harness, cx)
        }

        /// The panes shown, in layout order.
        fn shown(&self, cx: &mut VisualTestContext) -> Vec<PaneId> {
            self.view.read_with(cx, |view, cx| {
                view.shown().iter().map(|pane| pane.read(cx).id()).collect()
            })
        }

        fn pane(&self, n: usize, cx: &mut VisualTestContext) -> Entity<PaneView> {
            self.view.read_with(cx, |view, _| view.shown()[n].clone())
        }

        fn selected(&self, cx: &mut VisualTestContext) -> Option<TabId> {
            self.view.read_with(cx, |view, _| view.selected())
        }

        /// The pane whose terminal has the keyboard.
        fn focused(&self, cx: &mut VisualTestContext) -> Option<PaneId> {
            let shown = self.view.read_with(cx, |view, _| view.shown());
            cx.update(|window, cx| {
                shown
                    .iter()
                    .find(|pane| pane.focus_handle(cx).is_focused(window))
                    .map(|pane| pane.read(cx).id())
            })
        }

        fn select(&self, tab: TabId, cx: &mut VisualTestContext) {
            cx.update(|window, cx| {
                self.view
                    .update(cx, |view, cx| view.select(tab, window, cx));
            });
            cx.run_until_parked();
        }

        /// Runs `f` on the view in its window, then lets what it started finish.
        fn act(
            &self,
            f: impl FnOnce(&mut WorkspaceView, &mut Window, &mut Context<WorkspaceView>),
            cx: &mut VisualTestContext,
        ) {
            cx.update(|window, cx| self.view.update(cx, |view, cx| f(view, window, cx)));
            cx.run_until_parked();
        }

        /// Asks the `n`th pane shown for `request`, as its header menu does.
        fn request(&self, n: usize, request: PaneRequest, cx: &mut VisualTestContext) {
            let pane = self.pane(n, cx);
            pane.update(cx, |pane, cx| pane.request(request, cx));
            cx.run_until_parked();
        }

        /// The pane waiting on the close confirmation.
        fn closing(&self, cx: &mut VisualTestContext) -> Option<PaneId> {
            self.view.read_with(cx, |view, _| view.closing())
        }

        fn closed(&self) -> Vec<PaneId> {
            self.api.closed.lock().unwrap().clone()
        }

        fn deliver(&self, message: ServerMessage, cx: &mut VisualTestContext) {
            self.remote
                .events
                .try_send(Event::Message(message))
                .unwrap();
            cx.run_until_parked();
        }

        fn changed(&self, entity: Changed, cx: &mut VisualTestContext) {
            self.deliver(
                ServerMessage::Changed {
                    entity,
                    id: "x".into(),
                },
                cx,
            );
        }

        fn draw(&self, cx: &mut VisualTestContext) {
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }

    /// Tab `n` at `position`, split side by side between panes `a` and `b`.
    fn split_tab(n: u32, position: i64, a: u32, b: u32) -> Tab {
        Tab {
            layout: Some(Layout::Split {
                split: SplitDirection::Horizontal,
                ratio: Ratio::HALF,
                a: Box::new(Layout::single(id(a))),
                b: Box::new(Layout::single(id(b))),
            }),
            ..tab(n, id(1), position, None)
        }
    }

    mod tabs {
        use super::*;

        #[gpui::test]
        fn should_show_the_lowest_positioned_tab_first(cx: &mut TestAppContext) {
            let tabs = vec![
                tab(1, id(1), 1, Some(id(11))),
                tab(2, id(1), 0, Some(id(21))),
            ];
            let panes = vec![shell_pane(11, id(1)), shell_pane(21, id(2))];

            let (harness, cx) = Harness::open(tabs, panes, cx);

            assert_eq!(harness.selected(cx), Some(id(2)));
            assert_eq!(harness.shown(cx), [id(21)]);
        }

        #[gpui::test]
        fn should_show_the_panes_of_the_tab_selected(cx: &mut TestAppContext) {
            let tabs = vec![tab(1, id(1), 0, Some(id(11))), split_tab(2, 1, 21, 22)];
            let panes = vec![
                shell_pane(11, id(1)),
                shell_pane(21, id(2)),
                shell_pane(22, id(2)),
            ];
            let (harness, cx) = Harness::open(tabs, panes, cx);

            harness.select(id(2), cx);

            assert_eq!(harness.shown(cx), [id(21), id(22)]);
        }

        #[gpui::test]
        fn should_read_at_most_twice_more_for_a_burst_of_changes(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(vec![tab(1, id(1), 0, None)], vec![], cx);
            let before = harness.api.pane_reads();
            harness
                .api
                .set_tabs(id(1), vec![tab(1, id(1), 0, None), tab(2, id(1), 1, None)]);

            for _ in 0..10 {
                harness
                    .remote
                    .events
                    .try_send(Event::Message(ServerMessage::Changed {
                        entity: Changed::Task,
                        id: "x".into(),
                    }))
                    .unwrap();
            }
            cx.run_until_parked();

            let reads = harness.api.pane_reads() - before;
            assert!(reads <= 2, "{reads} reads");
            let tabs = harness.view.read_with(cx, |view, _| view.read.tabs.len());
            assert_eq!(tabs, 2);
        }

        #[gpui::test]
        fn should_keep_the_selected_tab_across_reads(cx: &mut TestAppContext) {
            let tabs = vec![tab(1, id(1), 0, None), tab(2, id(1), 1, None)];
            let (harness, cx) = Harness::open(tabs, vec![], cx);
            harness.select(id(2), cx);

            harness.changed(Changed::Tab, cx);

            assert_eq!(harness.selected(cx), Some(id(2)));
        }

        #[gpui::test]
        fn should_show_the_first_tab_once_the_selected_one_is_gone(cx: &mut TestAppContext) {
            let tabs = vec![tab(1, id(1), 0, None), tab(2, id(1), 1, None)];
            let (harness, cx) = Harness::open(tabs, vec![], cx);
            harness.select(id(2), cx);
            harness.api.set_tabs(id(1), vec![tab(1, id(1), 0, None)]);

            harness.changed(Changed::Tab, cx);

            assert_eq!(harness.selected(cx), Some(id(1)));
        }

        #[gpui::test]
        fn should_ignore_a_tab_the_workspace_lacks(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(vec![tab(1, id(1), 0, None)], vec![], cx);

            harness.select(id(9), cx);

            assert_eq!(harness.selected(cx), Some(id(1)));
        }
    }

    mod panes {
        use super::*;

        #[gpui::test]
        fn should_show_every_pane_of_a_split(cx: &mut TestAppContext) {
            let panes = vec![shell_pane(7, id(1)), shell_pane(8, id(1))];

            let (harness, cx) = Harness::open(vec![split_tab(1, 0, 7, 8)], panes, cx);

            assert_eq!(harness.shown(cx), [id(7), id(8)]);
            harness.draw(cx);
        }

        #[gpui::test]
        fn should_show_no_pane_when_the_tab_has_none(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(vec![tab(1, id(1), 0, None)], vec![], cx);

            assert!(harness.shown(cx).is_empty());
            harness.draw(cx);
        }

        #[gpui::test]
        fn should_keep_each_panes_view_across_reads(cx: &mut TestAppContext) {
            let panes = vec![shell_pane(7, id(1)), shell_pane(8, id(1))];
            let (harness, cx) = Harness::open(vec![split_tab(1, 0, 7, 8)], panes, cx);
            let before = harness.pane(1, cx);

            harness.changed(Changed::Pane, cx);

            assert_eq!(harness.pane(1, cx), before);
        }

        #[gpui::test]
        fn should_keep_a_panes_view_when_its_tab_is_split(cx: &mut TestAppContext) {
            let panes = vec![shell_pane(7, id(1)), shell_pane(8, id(1))];
            let (harness, cx) = Harness::open(vec![tab(1, id(1), 0, Some(id(7)))], panes, cx);
            let before = harness.pane(0, cx);
            harness.api.set_tabs(id(1), vec![split_tab(1, 0, 7, 8)]);

            harness.changed(Changed::Tab, cx);

            assert_eq!(harness.shown(cx), [id(7), id(8)]);
            assert_eq!(harness.pane(0, cx), before);
        }

        #[gpui::test]
        fn should_not_read_again_for_other_changes(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(
                vec![tab(1, id(1), 0, Some(id(7)))],
                vec![shell_pane(7, id(1))],
                cx,
            );
            harness
                .api
                .set_tabs(id(1), vec![tab(1, id(1), 0, Some(id(8)))]);
            harness.api.set_panes(vec![shell_pane(8, id(1))]);

            harness.changed(Changed::Note, cx);

            assert_eq!(harness.shown(cx), [id(7)]);
        }

        #[gpui::test]
        fn should_focus_the_first_pane_of_a_tab_it_shows(cx: &mut TestAppContext) {
            let tabs = vec![tab(1, id(1), 0, Some(id(11))), split_tab(2, 1, 21, 22)];
            let panes = vec![
                shell_pane(11, id(1)),
                shell_pane(21, id(2)),
                shell_pane(22, id(2)),
            ];
            let (harness, cx) = Harness::open(tabs, panes, cx);

            harness.select(id(2), cx);

            let first = harness.pane(0, cx);
            let focused = cx.update(|window, cx| first.focus_handle(cx).is_focused(window));
            assert!(focused);
        }

        #[gpui::test]
        fn should_draw_a_split_in_both_themes(cx: &mut TestAppContext) {
            let tab = Tab {
                layout: Some(Layout::Split {
                    split: SplitDirection::Horizontal,
                    ratio: Ratio::HALF,
                    a: Box::new(Layout::single(id(7))),
                    b: Box::new(Layout::Split {
                        split: SplitDirection::Vertical,
                        ratio: Ratio::HALF,
                        a: Box::new(Layout::single(id(8))),
                        b: Box::new(Layout::single(id(9))),
                    }),
                }),
                ..tab(1, id(1), 0, None)
            };
            let panes = (7..=9).map(|n| shell_pane(n, id(1))).collect();
            let (harness, cx) = Harness::open(vec![tab], panes, cx);

            for preference in [ThemePreference::Light, ThemePreference::Dark] {
                cx.update(|_, cx| Theme::set_preference(preference, cx));
                harness.draw(cx);
            }

            assert_eq!(harness.shown(cx), [id(7), id(8), id(9)]);
        }
    }

    mod shortcuts {
        use super::*;

        /// Tabs 1, 2 and 3, at positions 0, 1 and 2, with panes 11, 21 + 22 and 31.
        fn three_tabs(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            let tabs = vec![
                tab(1, id(1), 0, Some(id(11))),
                split_tab(2, 1, 21, 22),
                tab(3, id(1), 2, Some(id(31))),
            ];
            let panes = vec![
                shell_pane(11, id(1)),
                shell_pane(21, id(2)),
                shell_pane(22, id(2)),
                shell_pane(31, id(3)),
            ];
            Harness::open(tabs, panes, cx)
        }

        mod tabs {
            use super::*;

            #[gpui::test]
            fn should_show_the_next_tab_and_wrap_to_the_first(cx: &mut TestAppContext) {
                let (harness, cx) = three_tabs(cx);

                cx.simulate_keystrokes("ctrl-pagedown ctrl-tab");
                assert_eq!(harness.selected(cx), Some(id(3)));

                cx.simulate_keystrokes("ctrl-pagedown");
                assert_eq!(harness.selected(cx), Some(id(1)));
            }

            #[gpui::test]
            fn should_show_the_previous_tab_and_wrap_to_the_last(cx: &mut TestAppContext) {
                let (harness, cx) = three_tabs(cx);

                cx.simulate_keystrokes("ctrl-pageup");
                assert_eq!(harness.selected(cx), Some(id(3)));

                cx.simulate_keystrokes("ctrl-shift-tab");
                assert_eq!(harness.selected(cx), Some(id(2)));
            }

            #[gpui::test]
            fn should_show_the_tab_by_its_number(cx: &mut TestAppContext) {
                let (harness, cx) = three_tabs(cx);

                cx.simulate_keystrokes("alt-3");

                assert_eq!(harness.selected(cx), Some(id(3)));
            }

            #[gpui::test]
            fn should_ignore_a_number_past_the_last_tab(cx: &mut TestAppContext) {
                let (harness, cx) = three_tabs(cx);

                cx.simulate_keystrokes("alt-2 alt-9");

                assert_eq!(harness.selected(cx), Some(id(2)));
            }

            #[gpui::test]
            fn should_keep_the_keyboard_on_a_tab_with_no_panes(cx: &mut TestAppContext) {
                let tabs = vec![tab(1, id(1), 0, None), tab(2, id(1), 1, Some(id(21)))];
                let (harness, cx) = Harness::open(tabs, vec![shell_pane(21, id(2))], cx);

                cx.simulate_keystrokes("ctrl-tab");

                assert_eq!(harness.selected(cx), Some(id(2)));
            }
        }

        mod panes {
            use super::*;

            #[gpui::test]
            fn should_focus_the_next_pane_and_wrap_to_the_first(cx: &mut TestAppContext) {
                let (harness, cx) = three_tabs(cx);
                cx.simulate_keystrokes("alt-2");

                cx.simulate_keystrokes("secondary-shift-down");
                assert_eq!(harness.focused(cx), Some(id(22)));

                cx.simulate_keystrokes("secondary-shift-down");
                assert_eq!(harness.focused(cx), Some(id(21)));
            }

            #[gpui::test]
            fn should_focus_the_previous_pane_and_wrap_to_the_last(cx: &mut TestAppContext) {
                let (harness, cx) = three_tabs(cx);
                cx.simulate_keystrokes("alt-2");

                cx.simulate_keystrokes("secondary-shift-up");
                assert_eq!(harness.focused(cx), Some(id(22)));

                cx.simulate_keystrokes("secondary-shift-up");
                assert_eq!(harness.focused(cx), Some(id(21)));
            }
        }
        mod pane_actions {
            use super::*;

            /// A split of `a` and `b` in `direction`, half each.
            fn halves(direction: SplitDirection, a: Layout, b: Layout) -> Layout {
                Layout::Split {
                    split: direction,
                    ratio: Ratio::HALF,
                    a: Box::new(a),
                    b: Box::new(b),
                }
            }

            #[gpui::test]
            fn should_open_a_tab_with_a_shell(cx: &mut TestAppContext) {
                let (harness, cx) = three_tabs(cx);

                cx.simulate_keystrokes("secondary-shift-t");

                assert_eq!(harness.selected(cx), Some(id(101)));
                assert_eq!(harness.focused(cx), Some(id(102)));
            }

            #[gpui::test]
            fn should_split_the_focused_pane_right(cx: &mut TestAppContext) {
                let (harness, cx) = three_tabs(cx);

                cx.simulate_keystrokes("secondary-shift-e");

                assert_eq!(
                    harness.api.layout(id(1), id(1)),
                    Some(halves(
                        SplitDirection::Horizontal,
                        Layout::single(id(11)),
                        Layout::single(id(101))
                    ))
                );
                assert_eq!(harness.focused(cx), Some(id(101)));
            }

            #[gpui::test]
            fn should_split_the_focused_pane_down(cx: &mut TestAppContext) {
                let (harness, cx) = three_tabs(cx);
                cx.simulate_keystrokes("alt-2 secondary-shift-down");

                cx.simulate_keystrokes("secondary-shift-o");

                assert_eq!(
                    harness.api.layout(id(1), id(2)),
                    Some(halves(
                        SplitDirection::Horizontal,
                        Layout::single(id(21)),
                        halves(
                            SplitDirection::Vertical,
                            Layout::single(id(22)),
                            Layout::single(id(101))
                        )
                    ))
                );
            }

            #[gpui::test]
            fn should_ask_before_closing_the_focused_running_pane(cx: &mut TestAppContext) {
                let (harness, cx) = three_tabs(cx);
                cx.simulate_keystrokes("alt-2 secondary-shift-down");

                cx.simulate_keystrokes("secondary-shift-w");

                assert_eq!(harness.closing(cx), Some(id(22)));
                assert_eq!(harness.closed(), []);
            }

            #[gpui::test]
            fn should_close_the_focused_exited_pane_at_once(cx: &mut TestAppContext) {
                let tabs = vec![tab(1, id(1), 0, Some(id(11)))];
                let pane = Pane {
                    state: AgentState::Exited,
                    ..shell_pane(11, id(1))
                };
                let (harness, cx) = Harness::open(tabs, vec![pane], cx);

                cx.simulate_keystrokes("secondary-shift-w");

                assert_eq!(harness.closed(), [id(11)]);
            }

            #[gpui::test]
            fn should_do_nothing_to_panes_on_a_tab_with_none(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(vec![tab(1, id(1), 0, None)], vec![], cx);

                cx.simulate_keystrokes("secondary-shift-e secondary-shift-w");

                assert_eq!(harness.api.layout(id(1), id(1)), None);
                assert_eq!(harness.closed(), []);
            }
        }
    }

    mod new_tab {
        use super::*;

        fn one_tab(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            let tabs = vec![tab(1, id(1), 0, Some(id(11)))];
            Harness::open(tabs, vec![shell_pane(11, id(1))], cx)
        }

        #[gpui::test]
        fn should_open_a_tab_with_a_shell_and_show_it(cx: &mut TestAppContext) {
            let (harness, cx) = one_tab(cx);

            harness.act(WorkspaceView::new_tab, cx);

            // The fake makes the tab first, then its pane.
            assert_eq!(harness.selected(cx), Some(id(101)));
            assert_eq!(harness.shown(cx), [id(102)]);
            assert_eq!(harness.focused(cx), Some(id(102)));
        }

        #[gpui::test]
        fn should_start_the_shell_in_the_workspaces_dir(cx: &mut TestAppContext) {
            let (harness, cx) = one_tab(cx);

            harness.act(WorkspaceView::new_tab, cx);

            let cwd = harness
                .pane(0, cx)
                .read_with(cx, |pane, _| pane.pane().cwd.clone());
            assert_eq!(cwd, "/w/agentdeck");
        }

        #[gpui::test]
        fn should_open_one_when_the_workspace_has_none(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(vec![], vec![], cx);

            assert_eq!(harness.selected(cx), Some(id(101)));
            assert_eq!(harness.shown(cx), [id(102)]);
        }

        #[gpui::test]
        fn should_leave_the_workspace_empty_once_its_last_tab_closes(cx: &mut TestAppContext) {
            let pane = Pane {
                state: AgentState::Exited,
                ..shell_pane(11, id(1))
            };
            let (harness, cx) = Harness::open(vec![tab(1, id(1), 0, Some(id(11)))], vec![pane], cx);

            cx.simulate_keystrokes("secondary-shift-w");

            assert_eq!(harness.selected(cx), None);
        }

        #[gpui::test]
        fn should_keep_the_tab_shown_when_one_cant_open(cx: &mut TestAppContext) {
            let (harness, cx) = one_tab(cx);
            harness.api.fail_next("no such dir");

            harness.act(WorkspaceView::new_tab, cx);

            assert_eq!(harness.selected(cx), Some(id(1)));
            harness.draw(cx);
        }
    }

    mod pane_menu {
        use agentdeck_core::AgentState;

        use super::*;

        fn one_pane(
            state: AgentState,
            cx: &mut TestAppContext,
        ) -> (Harness, &mut VisualTestContext) {
            let pane = Pane {
                state,
                ..shell_pane(7, id(1))
            };
            Harness::open(vec![tab(1, id(1), 0, Some(id(7)))], vec![pane], cx)
        }

        fn split(direction: SplitDirection, a: u32, b: u32) -> Layout {
            Layout::Split {
                split: direction,
                ratio: Ratio::HALF,
                a: Box::new(Layout::single(id(a))),
                b: Box::new(Layout::single(id(b))),
            }
        }

        mod split {
            use super::*;

            #[gpui::test]
            fn should_split_right_with_a_shell_in_the_panes_cwd(cx: &mut TestAppContext) {
                let (harness, cx) = one_pane(AgentState::Idle, cx);

                harness.request(0, PaneRequest::Split(SplitDirection::Horizontal), cx);

                assert_eq!(
                    harness.api.layout(id(1), id(1)),
                    Some(split(SplitDirection::Horizontal, 7, 101))
                );
                assert_eq!(harness.shown(cx), [id(7), id(101)]);
                let cwd = harness
                    .pane(1, cx)
                    .read_with(cx, |pane, _| pane.pane().cwd.clone());
                assert_eq!(cwd, "/w");
            }

            #[gpui::test]
            fn should_split_down(cx: &mut TestAppContext) {
                let (harness, cx) = one_pane(AgentState::Idle, cx);

                harness.request(0, PaneRequest::Split(SplitDirection::Vertical), cx);

                assert_eq!(
                    harness.api.layout(id(1), id(1)),
                    Some(split(SplitDirection::Vertical, 7, 101))
                );
            }

            #[gpui::test]
            fn should_focus_the_new_pane(cx: &mut TestAppContext) {
                let (harness, cx) = one_pane(AgentState::Idle, cx);

                harness.request(0, PaneRequest::Split(SplitDirection::Horizontal), cx);

                assert_eq!(harness.focused(cx), Some(id(101)));
            }

            #[gpui::test]
            fn should_keep_the_layout_when_the_split_fails(cx: &mut TestAppContext) {
                let (harness, cx) = one_pane(AgentState::Idle, cx);
                harness.api.fail_next("refused");

                harness.request(0, PaneRequest::Split(SplitDirection::Horizontal), cx);

                assert_eq!(harness.shown(cx), [id(7)]);
                harness.draw(cx);
            }
        }

        mod close {
            use super::*;

            #[gpui::test]
            fn should_close_an_exited_pane_at_once(cx: &mut TestAppContext) {
                let (harness, cx) = one_pane(AgentState::Exited, cx);

                harness.request(0, PaneRequest::Close, cx);

                assert_eq!(harness.closed(), [id(7)]);
                assert_eq!(harness.closing(cx), None);
                assert!(harness.shown(cx).is_empty());
            }

            #[gpui::test]
            fn should_close_a_pane_once_however_often_asked(cx: &mut TestAppContext) {
                let (harness, cx) = one_pane(AgentState::Exited, cx);
                let pane = harness.pane(0, cx);

                // Asked again before the first close finishes, as a slow daemon invites.
                pane.update(cx, |pane, cx| {
                    pane.request(PaneRequest::Close, cx);
                    pane.request(PaneRequest::Close, cx);
                });
                cx.run_until_parked();

                assert_eq!(harness.closed(), [id(7)]);
            }

            #[gpui::test]
            fn should_dim_a_pane_while_it_closes(cx: &mut TestAppContext) {
                let (harness, cx) = one_pane(AgentState::Exited, cx);
                let pane = harness.pane(0, cx);

                pane.update(cx, |pane, cx| pane.request(PaneRequest::Close, cx));

                assert!(pane.read_with(cx, |pane, _| pane.is_closing()));
                harness.draw(cx);
            }

            #[gpui::test]
            fn should_give_its_sibling_the_space(cx: &mut TestAppContext) {
                let panes = vec![
                    Pane {
                        state: AgentState::Exited,
                        ..shell_pane(7, id(1))
                    },
                    shell_pane(8, id(1)),
                ];
                let (harness, cx) = Harness::open(vec![split_tab(1, 0, 7, 8)], panes, cx);

                harness.request(0, PaneRequest::Close, cx);

                assert_eq!(
                    harness.api.layout(id(1), id(1)),
                    Some(Layout::single(id(8)))
                );
                assert_eq!(harness.shown(cx), [id(8)]);
            }

            #[gpui::test]
            fn should_take_the_tab_with_its_last_pane(cx: &mut TestAppContext) {
                let tabs = vec![
                    tab(1, id(1), 0, Some(id(11))),
                    tab(2, id(1), 1, Some(id(21))),
                ];
                let panes = vec![
                    shell_pane(11, id(1)),
                    Pane {
                        state: AgentState::Exited,
                        ..shell_pane(21, id(2))
                    },
                ];
                let (harness, cx) = Harness::open(tabs, panes, cx);
                harness.select(id(2), cx);

                harness.request(0, PaneRequest::Close, cx);

                assert_eq!(harness.selected(cx), Some(id(1)));
                assert_eq!(harness.shown(cx), [id(11)]);
            }

            #[gpui::test]
            fn should_ask_before_closing_a_running_pane(cx: &mut TestAppContext) {
                let (harness, cx) = one_pane(AgentState::Working, cx);

                harness.request(0, PaneRequest::Close, cx);

                assert_eq!(harness.closed(), []);
                assert_eq!(harness.closing(cx), Some(id(7)));
                harness.draw(cx);
            }

            #[gpui::test]
            fn should_close_a_running_pane_once_confirmed(cx: &mut TestAppContext) {
                let (harness, cx) = one_pane(AgentState::Working, cx);
                harness.request(0, PaneRequest::Close, cx);

                harness.act(WorkspaceView::confirm_close, cx);

                assert_eq!(harness.closed(), [id(7)]);
                assert_eq!(harness.closing(cx), None);
            }

            #[gpui::test]
            fn should_keep_a_running_pane_when_cancelled(cx: &mut TestAppContext) {
                let (harness, cx) = one_pane(AgentState::Working, cx);
                harness.request(0, PaneRequest::Close, cx);

                // What the dialog's Cancel and Escape do.
                harness.act(|view, window, cx| view.close_dialog.close(window, cx), cx);

                assert_eq!(harness.closed(), []);
                assert_eq!(harness.closing(cx), None);
                assert_eq!(harness.shown(cx), [id(7)]);
            }
        }
    }

    mod header {
        use super::*;

        fn one_pane(pane: Pane, cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            let shown = pane.id;
            Harness::open(vec![tab(1, id(1), 0, Some(shown))], vec![pane], cx)
        }

        #[gpui::test]
        fn should_title_a_pane_from_its_terminal(cx: &mut TestAppContext) {
            let (harness, cx) = one_pane(shell_pane(7, id(1)), cx);

            // The pane's terminal attaches on the connection's first channel.
            harness.deliver(
                ServerMessage::Attached {
                    channel: 1,
                    pane: id(7),
                    cols: 80,
                    rows: 24,
                    title: "✳ Claude Code".into(),
                    state: AgentState::Idle,
                },
                cx,
            );

            let title = harness.pane(0, cx).read_with(cx, |pane, _| pane.title());
            assert_eq!(title, "✳ Claude Code");
        }

        #[gpui::test]
        fn should_follow_state_broadcasts_between_reads(cx: &mut TestAppContext) {
            let (harness, cx) = one_pane(shell_pane(7, id(1)), cx);

            harness.deliver(
                ServerMessage::State {
                    pane: id(7),
                    state: AgentState::Blocked,
                },
                cx,
            );

            let state = harness
                .pane(0, cx)
                .read_with(cx, |pane, _| pane.pane().state);
            assert_eq!(state, AgentState::Blocked);
        }

        #[gpui::test]
        fn should_record_the_exit_code(cx: &mut TestAppContext) {
            let (harness, cx) = one_pane(shell_pane(7, id(1)), cx);

            harness.deliver(
                ServerMessage::Exit {
                    pane: id(7),
                    code: Some(1),
                },
                cx,
            );

            let pane = harness
                .pane(0, cx)
                .read_with(cx, |pane, _| pane.pane().clone());
            assert_eq!((pane.state, pane.exit_code), (AgentState::Exited, Some(1)));
        }

        #[gpui::test]
        fn should_ignore_another_panes_state(cx: &mut TestAppContext) {
            let (harness, cx) = one_pane(shell_pane(7, id(1)), cx);

            harness.deliver(
                ServerMessage::State {
                    pane: id(8),
                    state: AgentState::Blocked,
                },
                cx,
            );

            let state = harness
                .pane(0, cx)
                .read_with(cx, |pane, _| pane.pane().state);
            assert_eq!(state, AgentState::Idle);
        }

        #[gpui::test]
        fn should_show_the_linked_tasks_title(cx: &mut TestAppContext) {
            let linked = task(3, TaskKind::Plain, Some(id(1)));
            let pane = Pane {
                task_id: Some(linked.id),
                ..shell_pane(7, id(1))
            };
            let (harness, cx) = one_pane(pane, cx);
            harness.api.set_tasks(vec![linked]);

            harness.changed(Changed::Task, cx);

            let task = harness
                .pane(0, cx)
                .read_with(cx, |pane, _| pane.task().cloned());
            assert_eq!(task.as_deref(), Some("task 3"));
            harness.draw(cx);
        }
    }
}
