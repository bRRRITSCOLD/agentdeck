//! The main window's content: what the daemon connection allows. Until it connects, a
//! centred status; once it does, the sidebar beside the selected view: a workspace's pane
//! the Tasks view or the Inbox, and the new workspace dialog the
//! sidebar's `+` opens. When the compositor leaves decorations to the app, a title bar and
//! resize edges frame it.
//!
//! It archives and unarchives the selected workspace when the top bar asks, toasting the
//! outcome; an archive's toast offers Undo. The top bar's Delete workspace… opens the delete
//! dialog; once a workspace is deleted the selection falls back to the first workspace left,
//! else the Inbox.
//!
//! The Tasks view and the right panel ask for the new task dialog with
//! [`NewTask`], preset to the Inbox or their workspace. They and the Inbox ask for the task
//! sheet with [`OpenTask`] when a task's title is clicked.

use std::sync::Arc;

use agentdeck_client::ApiError;
use agentdeck_core::WorkspaceId;
use brrritscold_core_gpui::{
    ActiveTheme as _, Alert, AlertAction, AlertDescription, AlertTitle, AlertVariant, Button,
    ButtonSize, ButtonVariant, IconName, Separator, SeparatorOrientation,
};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Decorations, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Window, div, px,
};

use crate::api::Api;
use crate::connection::{Connection, Status};
use crate::delete_workspace::{DeleteEvent, DeleteTarget, DeleteWorkspaceDialog};
use crate::frame;
use crate::inbox::{InboxView, Synced};
use crate::keymap::ToggleRightPanel;
use crate::new_agent::{self, Launched, NewAgent, NewAgentDialog, Target};
use crate::new_task::{NewTask, NewTaskDialog};
use crate::new_workspace::{Created, NewWorkspaceDialog};
use crate::notice::{self, Notice, NoticeAction};
use crate::right_panel::RightPanel;
use crate::sidebar::{Selection, SidebarEvent, SidebarView};
use crate::task_sheet::{OpenTask, TaskSheet};
use crate::tasks::TasksView;
use crate::title_bar::{Close, Minimize, TitleBar, ToggleMaximize};
use crate::top_bar::{ArchiveWorkspace, DeleteWorkspace, TopBar, UnarchiveWorkspace};
use crate::workspace::{WorkspaceView, failed};

/// Whether `err` is the daemon refusing with `code`.
fn is_refusal(err: &anyhow::Error, code: &str) -> bool {
    err.downcast_ref::<ApiError>()
        .is_some_and(|err| err.code.as_deref() == Some(code))
}

/// The main window's root view, inside core-gpui's `Root`.
pub struct Shell {
    connection: Entity<Connection>,
    /// The sidebar, while connected; a new connection gets a new one.
    sidebar: Option<Entity<SidebarView>>,
    /// Follows the sidebar's selection.
    _selection: Option<Subscription>,
    /// The new workspace dialog, while connected, and what joins it to the sidebar.
    new_workspace: Option<Entity<NewWorkspaceDialog>>,
    /// The new agent dialog, while connected.
    new_agent: Option<Entity<NewAgentDialog>>,
    /// The Delete workspace dialog, while connected.
    delete_workspace: Option<Entity<DeleteWorkspaceDialog>>,
    /// The new task dialog, while connected.
    new_task: Option<Entity<NewTaskDialog>>,
    /// The task sheet, while connected.
    task_sheet: Option<Entity<TaskSheet>>,
    _new_workspace: Vec<Subscription>,
    /// The selected workspace.
    workspace: Option<Entity<WorkspaceView>>,
    /// The Tasks view, while connected; shown while the sidebar selects Tasks.
    tasks: Option<Entity<TasksView>>,
    /// The Inbox, while connected; shown while the sidebar selects it.
    inbox: Option<Entity<InboxView>>,
    /// The right panel beside a workspace, while connected.
    panel: Option<Entity<RightPanel>>,
    /// Whether the right panel shows; the top bar's panel button flips it.
    panel_open: bool,
    /// What the sidebar selects.
    selection: Option<Selection>,
    /// Held by the shell so the title bar's actions reach its handlers while nothing inside
    /// has focus.
    focus: FocusHandle,
}

impl Shell {
    /// A shell that follows `connection`, focused in `window`.
    pub fn new(
        connection: Entity<Connection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe_in(&connection, window, |shell, _, window, cx| {
            shell.follow_connection(window, cx);
            cx.notify();
        })
        .detach();
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut shell = Shell {
            connection,
            sidebar: None,
            _selection: None,
            new_workspace: None,
            new_agent: None,
            delete_workspace: None,
            new_task: None,
            task_sheet: None,
            _new_workspace: Vec::new(),
            workspace: None,
            tasks: None,
            inbox: None,
            panel: None,
            panel_open: true,
            selection: None,
            focus,
        };
        shell.follow_connection(window, cx);
        shell
    }

    /// Opens a sidebar on a new session, and drops it and the selected view once the
    /// connection is lost.
    fn follow_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.connection.read(cx).status() {
            Status::Connected(session) => {
                if self.sidebar.is_none() {
                    let api = session.api.clone();
                    let connection = self.connection.clone();
                    let dialog = cx.new(|cx| NewWorkspaceDialog::new(api.clone(), window, cx));
                    let inbox = cx.new(|_| InboxView::new(api.clone()));
                    let new_agent = cx.new(|_| NewAgentDialog::new(api.clone()));
                    let delete = cx.new(|cx| DeleteWorkspaceDialog::new(api.clone(), window, cx));
                    let new_task = cx.new(|cx| NewTaskDialog::new(api.clone(), window, cx));
                    let panel = cx.new(|cx| RightPanel::new(api.clone(), window, cx));
                    let tasks = cx.new(|cx| TasksView::new(api.clone(), window, cx));
                    let task_sheet =
                        cx.new(|cx| TaskSheet::new(api.clone(), &connection, window, cx));
                    let sidebar = cx.new(|cx| SidebarView::new(api, &connection, cx));
                    self._selection =
                        Some(cx.observe_in(&sidebar, window, |shell, _, window, cx| {
                            shell.follow_selection(window, cx);
                        }));
                    self._new_workspace = vec![
                        cx.subscribe_in(&sidebar, window, |shell, _, event, window, cx| {
                            let SidebarEvent::NewWorkspace = event;
                            if let Some(dialog) = &shell.new_workspace {
                                dialog.update(cx, |dialog, cx| dialog.open(window, cx));
                            }
                        }),
                        // A sync that changes no task broadcasts nothing, yet moves when PRs
                        // last synced.
                        cx.subscribe(&inbox, |shell, _, _: &Synced, cx| {
                            if let Some(sidebar) = &shell.sidebar {
                                sidebar.update(cx, |sidebar, cx| sidebar.refresh(cx));
                            }
                        }),
                        // Show the agent's tab and focus it: it may be asking about trust.
                        cx.subscribe_in(
                            &new_agent,
                            window,
                            |shell, _, launched: &Launched, window, cx| {
                                if let Some(workspace) = &shell.workspace {
                                    let Launched { tab, pane } = *launched;
                                    workspace.update(cx, |view, cx| {
                                        view.show_pane(tab, pane, window, cx)
                                    });
                                }
                            },
                        ),
                        cx.subscribe_in(&delete, window, |shell, _, event, window, cx| {
                            match event {
                                DeleteEvent::ArchiveInstead(_) => shell.archive(window, cx),
                                // The sidebar's selection falls back once it is gone.
                                DeleteEvent::Deleted(_) => shell.refresh(cx),
                            }
                        }),
                        cx.subscribe_in(&tasks, window, |shell, _, ask, window, cx| {
                            shell.new_task(*ask, window, cx);
                        }),
                        cx.subscribe_in(&panel, window, |shell, _, ask, window, cx| {
                            shell.new_task(*ask, window, cx);
                        }),
                        cx.subscribe_in(&tasks, window, |shell, _, ask, window, cx| {
                            shell.open_task(*ask, window, cx);
                        }),
                        cx.subscribe_in(&inbox, window, |shell, _, ask, window, cx| {
                            shell.open_task(*ask, window, cx);
                        }),
                        cx.subscribe_in(&panel, window, |shell, _, ask, window, cx| {
                            shell.open_task(*ask, window, cx);
                        }),
                        cx.subscribe(&dialog, |shell, _, Created(id), cx| {
                            if let Some(sidebar) = &shell.sidebar {
                                let selection = Selection::Workspace(*id);
                                sidebar.update(cx, |sidebar, cx| sidebar.select(selection, cx));
                            }
                        }),
                    ];
                    self.new_workspace = Some(dialog);
                    self.inbox = Some(inbox);
                    self.new_agent = Some(new_agent);
                    self.delete_workspace = Some(delete);
                    self.new_task = Some(new_task);
                    self.task_sheet = Some(task_sheet);
                    self.tasks = Some(tasks);
                    self.panel = Some(panel);
                    self.sidebar = Some(sidebar);
                    self.follow_selection(window, cx);
                }
            }
            Status::Connecting | Status::Failed(_) => {
                self.sidebar = None;
                self._selection = None;
                self.new_workspace = None;
                self._new_workspace.clear();
                self.workspace = None;
                self.tasks = None;
                self.inbox = None;
                self.panel = None;
                self.new_agent = None;
                self.delete_workspace = None;
                self.new_task = None;
                self.task_sheet = None;
                self.selection = None;
            }
        }
    }

    /// Opens the view the sidebar selects, keeping it while the selection stays.
    fn follow_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sidebar) = self.sidebar.clone() else {
            return;
        };
        let (selection, snapshot) = {
            let sidebar = sidebar.read(cx);
            (sidebar.selection(), sidebar.snapshot().clone())
        };
        self.selection = Some(selection);
        if let (Some(panel), Selection::Workspace(workspace)) = (&self.panel, selection) {
            let snapshot = snapshot.clone();
            panel.update(cx, |panel, cx| panel.show(workspace, snapshot, cx));
        }
        if let Some(inbox) = &self.inbox {
            let snapshot = snapshot.clone();
            inbox.update(cx, |inbox, cx| inbox.set_snapshot(snapshot, cx));
        }
        if let Some(sheet) = &self.task_sheet {
            let snapshot = snapshot.clone();
            sheet.update(cx, |sheet, cx| sheet.set_snapshot(snapshot, cx));
        }
        if let Some(tasks) = &self.tasks {
            tasks.update(cx, |tasks, cx| tasks.set_snapshot(snapshot, cx));
        }
        let Status::Connected(session) = self.connection.read(cx).status() else {
            return;
        };
        let sidebar = sidebar.read(cx);
        let row = match selection {
            Selection::Workspace(selected) => sidebar.overview().workspace(selected),
            Selection::Inbox | Selection::Tasks => None,
        };
        let Some(row) = row else {
            self.workspace = None;
            cx.notify();
            return;
        };
        let top_bar = TopBar::new(row.name.clone(), row.dir.clone()).archived(row.archived);
        let selected = row.id;
        // Archiving or unarchiving swaps the view: an archived one holds no terminals.
        if let Some(view) = &self.workspace
            && view.read(cx).workspace() == selected
            && view.read(cx).top_bar().is_archived() == row.archived
        {
            view.update(cx, |view, cx| view.set_top_bar(top_bar, cx));
            return;
        }
        let (api, sender) = (session.api.clone(), session.sender.clone());
        let connection = self.connection.clone();
        self.workspace =
            Some(cx.new(|cx| {
                WorkspaceView::new(selected, top_bar, api, connection, sender, window, cx)
            }));
        cx.notify();
    }

    /// Opens the new agent dialog on the workspace shown, offering its root and the repos in
    /// it.
    fn new_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(sidebar), Some(dialog), Some(Selection::Workspace(selected))) =
            (&self.sidebar, &self.new_agent, self.selection)
        else {
            return;
        };
        let sidebar = sidebar.read(cx);
        let Some(row) = (sidebar.overview().workspaces.iter()).find(|w| w.id == selected) else {
            return;
        };
        let target = Target {
            id: row.id,
            name: row.name.clone(),
            dir: row.dir.clone(),
        };
        let folders = new_agent::folders(&row.dir, new_agent::repos_in(&row.dir));
        let snapshot = sidebar.snapshot().clone();
        dialog.update(cx, |dialog, cx| {
            dialog.open(target, folders, snapshot, window, cx)
        });
    }

    /// Opens the new task dialog as a view asks, offering the workspaces the sidebar holds.
    fn new_task(&mut self, ask: NewTask, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(sidebar), Some(dialog)) = (&self.sidebar, &self.new_task) else {
            return;
        };
        let snapshot = sidebar.read(cx).snapshot().clone();
        let NewTask(preset) = ask;
        dialog.update(cx, |dialog, cx| dialog.open(preset, &snapshot, window, cx));
    }

    /// Opens the task sheet on the task a view asks for.
    fn open_task(&mut self, ask: OpenTask, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(sidebar), Some(sheet)) = (&self.sidebar, &self.task_sheet) else {
            return;
        };
        let snapshot = sidebar.read(cx).snapshot().clone();
        let OpenTask(task) = ask;
        sheet.update(cx, |sheet, cx| sheet.open(task, snapshot, window, cx));
    }

    /// The selected workspace's id and name, if a workspace is selected.
    fn selected_workspace(&self, cx: &App) -> Option<(WorkspaceId, SharedString, bool)> {
        let (Some(sidebar), Some(Selection::Workspace(selected))) = (&self.sidebar, self.selection)
        else {
            return None;
        };
        let row = sidebar.read(cx).overview().workspace(selected)?;
        Some((row.id, row.name.clone(), row.archived))
    }

    /// The connected session's API.
    fn api(&self, cx: &App) -> Option<Arc<dyn Api>> {
        match self.connection.read(cx).status() {
            Status::Connected(session) => Some(session.api.clone()),
            Status::Connecting | Status::Failed(_) => None,
        }
    }

    /// Reads the sidebar again, so the window follows a change it made.
    fn refresh(&self, cx: &mut Context<Self>) {
        if let Some(sidebar) = &self.sidebar {
            sidebar.update(cx, |sidebar, cx| sidebar.refresh(cx));
        }
    }

    /// Archives the selected workspace, keeping it selected, and toasts the outcome: a refusal
    /// in the daemon's words, or the archive with an Undo.
    fn archive(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some((workspace, name, false)), Some(api)) =
            (self.selected_workspace(cx), self.api(cx))
        else {
            return;
        };
        let archiving = api.archive_workspace(workspace, cx);
        cx.spawn_in(window, async move |this, cx| {
            let archived = archiving.await;
            let _ = this.update_in(cx, |this, window, cx| match archived {
                Ok(()) => {
                    // Picked, so it stays selected once it leaves the workspace rows.
                    if let Some(sidebar) = &this.sidebar {
                        let selection = Selection::Workspace(workspace);
                        sidebar.update(cx, |sidebar, cx| sidebar.select(selection, cx));
                    }
                    this.refresh(cx);
                    let shell = cx.entity().downgrade();
                    let undo = NoticeAction::new("Undo", {
                        let name = name.clone();
                        move |window, cx| {
                            let name = name.clone();
                            let _ = shell.update(cx, |shell, cx| {
                                shell.unarchive(workspace, name, window, cx);
                            });
                        }
                    });
                    Notice::new(format!("Archived {name}"))
                        .description("Its folder, tasks and instructions are kept.")
                        .action(undo)
                        .show(window, cx);
                }
                Err(err) if is_refusal(&err, "panes_running") => {
                    Notice::error(format!("Can't archive {name}"), notice::message(&err))
                        .show(window, cx);
                }
                Err(err) => failed(format!("Couldn't archive {name}"), &err, window, cx),
            });
        })
        .detach();
    }

    /// Opens the Delete workspace dialog on the selected workspace.
    fn delete_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(sidebar), Some(dialog), Some(Selection::Workspace(selected))) =
            (&self.sidebar, &self.delete_workspace, self.selection)
        else {
            return;
        };
        let sidebar = sidebar.read(cx);
        let Some(row) = sidebar.overview().workspace(selected) else {
            return;
        };
        let target = DeleteTarget {
            id: row.id,
            name: row.name.clone(),
            dir: row.dir.clone(),
            archived: row.archived,
        };
        let snapshot = sidebar.snapshot().clone();
        dialog.update(cx, |dialog, cx| dialog.open(target, &snapshot, window, cx));
    }

    /// Brings `workspace`, named `name`, back from the archive.
    fn unarchive(
        &mut self,
        workspace: WorkspaceId,
        name: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(api) = self.api(cx) else {
            return;
        };
        let unarchiving = api.unarchive_workspace(workspace, cx);
        cx.spawn_in(window, async move |this, cx| {
            let unarchived = unarchiving.await;
            let _ = this.update_in(cx, |this, window, cx| match unarchived {
                Ok(()) => this.refresh(cx),
                Err(err) => failed(format!("Couldn't unarchive {name}"), &err, window, cx),
            });
        })
        .detach();
    }

    /// What the connection allows, filling the space under the title bar.
    fn content(&self, cx: &mut Context<Self>) -> AnyElement {
        let s = cx.theme().semantic();
        let centred = div().flex_1().flex().items_center().justify_center();
        match self.connection.read(cx).status() {
            Status::Connecting => centred
                .text_sm()
                .text_color(s.muted_foreground)
                .child("Connecting to agentdeckd…")
                .into_any_element(),
            Status::Failed(reason) => {
                let connection = self.connection.clone();
                let retry = Button::new("retry")
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Sm)
                    .label("Retry")
                    .on_click(move |_, _, cx| {
                        connection.update(cx, |connection, cx| connection.connect(cx));
                    });
                centred
                    .child(
                        Alert::new()
                            .variant(AlertVariant::Destructive)
                            .icon(IconName::Unplug)
                            .title(AlertTitle::new("agentdeck can't reach its daemon"))
                            .description(AlertDescription::new(reason.clone()))
                            .action(AlertAction::new().child(retry))
                            .w(px(480.)),
                    )
                    .into_any_element()
            }
            // Penpot "Main window" `Body`: Sidebar | Separator | the selected view.
            Status::Connected(_) => div()
                .flex_1()
                .min_h_0()
                .flex()
                .children(self.sidebar.clone())
                .child(Separator::new().orientation(SeparatorOrientation::Vertical))
                .map(|body| match self.selection {
                    Some(Selection::Workspace(_)) if self.workspace.is_some() => {
                        body.children(self.workspace.clone()).when_some(
                            self.panel.clone().filter(|_| self.panel_open),
                            |body, panel| {
                                body.child(
                                    Separator::new().orientation(SeparatorOrientation::Vertical),
                                )
                                .child(panel)
                            },
                        )
                    }
                    Some(Selection::Tasks) if self.tasks.is_some() => {
                        body.children(self.tasks.clone())
                    }
                    Some(Selection::Inbox) if self.inbox.is_some() => {
                        body.children(self.inbox.clone())
                    }
                    _ => body.child(div().flex_1()),
                })
                .children(self.new_workspace.clone())
                .children(self.new_agent.clone())
                .children(self.delete_workspace.clone())
                .children(self.new_task.clone())
                .children(self.task_sheet.clone())
                .into_any_element(),
        }
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let client_tiling = match window.window_decorations() {
            Decorations::Client { tiling } => Some(tiling),
            Decorations::Server => None,
        };
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .track_focus(&self.focus)
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &ToggleMaximize, window, _| window.zoom_window())
            .on_action(|_: &Close, window, _| window.remove_window())
            .on_action(cx.listener(|this, _: &NewAgent, window, cx| this.new_agent(window, cx)))
            .on_action(cx.listener(|this, _: &ArchiveWorkspace, window, cx| {
                this.archive(window, cx);
            }))
            .on_action(cx.listener(|this, _: &DeleteWorkspace, window, cx| {
                this.delete_workspace(window, cx);
            }))
            .on_action(cx.listener(|this, _: &UnarchiveWorkspace, window, cx| {
                if let Some((workspace, name, true)) = this.selected_workspace(cx) {
                    this.unarchive(workspace, name, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleRightPanel, _, cx| {
                this.panel_open = !this.panel_open;
                cx.notify();
            }))
            // Penpot "Main window": TitleBar / Separator / Body.
            .when(client_tiling.is_some(), |shell| {
                shell
                    .child(TitleBar::new("agentdeck"))
                    .child(Separator::new())
            })
            .child(self.content(cx))
            .when_some(client_tiling, |shell, tiling| {
                shell.children(frame::resize_edges(tiling))
            })
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::Arc;

    use agentdeck_client::ws::Socket;
    use brrritscold_core_gpui::{Root, Theme, ThemePreference};
    use gpui::{App, Task, TestAppContext, VisualTestContext};

    use agentdeck_core::WorkspaceId;

    use super::*;
    use crate::api::Api;
    use crate::api::fake::FakeApi;
    use crate::connection::Connector;
    use crate::connection::fake::FakeConnector;
    use crate::overview::Snapshot;
    use crate::overview::fixtures::{id, tab, workspace};

    /// A connect that never resolves, so the shell stays connecting.
    struct Pending;

    impl Connector for Pending {
        fn connect(&self, cx: &App) -> Task<anyhow::Result<(Arc<dyn Api>, Socket)>> {
            cx.background_spawn(std::future::pending())
        }
    }

    /// Opens a `Root` window on a [`Shell`] over a connection through `connector`, and draws
    /// it in light, then dark.
    fn draw_in_both_themes(connector: Rc<dyn Connector>, cx: &mut TestAppContext) {
        cx.update(brrritscold_core_gpui::init);
        cx.update(crate::terminal::init);
        let window = cx.add_window(|window, cx| {
            let connection = cx.new(|cx| Connection::new(connector, cx));
            let shell = cx.new(|cx| Shell::new(connection, window, cx));
            Root::new(shell, window, cx)
        });
        let cx = &mut VisualTestContext::from_window(*window, cx);
        for preference in [ThemePreference::Light, ThemePreference::Dark] {
            cx.update(|_, cx| Theme::set_preference(preference, cx));
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }

    mod render {
        use super::*;

        #[gpui::test]
        fn should_draw_while_connecting_in_both_themes(cx: &mut TestAppContext) {
            draw_in_both_themes(Rc::new(Pending), cx);
        }

        #[gpui::test]
        fn should_draw_a_failure_in_both_themes(cx: &mut TestAppContext) {
            draw_in_both_themes(Rc::new(FakeConnector::default().then_fail("refused")), cx);
        }

        #[gpui::test]
        fn should_draw_once_connected_in_both_themes(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let _remote = connector.then_connect();

            draw_in_both_themes(connector, cx);
        }

        #[gpui::test]
        fn should_draw_a_workspaces_pane_in_both_themes(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let _remote = connector.then_connect_to(workspace_with_a_pane());

            draw_in_both_themes(connector, cx);
        }
    }

    /// A daemon with workspace 1, whose one tab holds pane 7.
    fn workspace_with_a_pane() -> Arc<FakeApi> {
        let api = Arc::new(FakeApi::with(Snapshot {
            workspaces: vec![workspace(1, "agentdeck")],
            ..Snapshot::default()
        }));
        api.set_tabs(id(1), vec![tab(1, id(1), 0, Some(id(7)))]);
        api
    }

    mod selection {
        use super::*;

        fn open(connector: Rc<FakeConnector>, cx: &mut TestAppContext) -> Entity<Shell> {
            cx.update(brrritscold_core_gpui::init);
            cx.update(crate::terminal::init);
            let mut shell = None;
            cx.add_window(|window, cx| {
                let connection = cx.new(|cx| Connection::new(connector, cx));
                let view = cx.new(|cx| Shell::new(connection, window, cx));
                shell = Some(view.clone());
                Root::new(view, window, cx)
            });
            cx.run_until_parked();
            shell.unwrap()
        }

        fn shown_workspace(shell: &Entity<Shell>, cx: &TestAppContext) -> Option<WorkspaceId> {
            shell.read_with(cx, |shell, cx| {
                shell
                    .workspace
                    .as_ref()
                    .map(|view| view.read(cx).workspace())
            })
        }

        #[gpui::test]
        fn should_open_the_selected_workspace(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let _remote = connector.then_connect_to(workspace_with_a_pane());

            let shell = open(connector, cx);

            assert_eq!(shown_workspace(&shell, cx), Some(id(1)));
        }

        #[gpui::test]
        fn should_drop_the_workspace_once_the_connection_is_lost(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let remote = connector.then_connect_to(workspace_with_a_pane());
            let shell = open(connector, cx);

            drop(remote);
            cx.run_until_parked();

            assert_eq!(shown_workspace(&shell, cx), None);
        }

        mod tasks {
            use super::*;

            #[gpui::test]
            fn should_show_the_tasks_view_when_the_sidebar_selects_it(cx: &mut TestAppContext) {
                let connector = Rc::new(FakeConnector::default());
                let _remote = connector.then_connect_to(workspace_with_a_pane());
                let shell = open(connector, cx);
                let sidebar = shell.read_with(cx, |shell, _| shell.sidebar.clone().unwrap());

                sidebar.update(cx, |sidebar, cx| sidebar.select(Selection::Tasks, cx));
                cx.run_until_parked();

                shell.read_with(cx, |shell, _| {
                    assert_eq!(shell.selection, Some(Selection::Tasks));
                    assert!(shell.tasks.is_some());
                });
                assert_eq!(shown_workspace(&shell, cx), None);
            }

            #[gpui::test]
            fn should_toggle_the_right_panel_from_its_action(cx: &mut TestAppContext) {
                let connector = Rc::new(FakeConnector::default());
                let _remote = connector.then_connect_to(workspace_with_a_pane());
                let shell = open(connector, cx);
                let window = cx.windows()[0];

                let mut flip = || {
                    window
                        .update(cx, |_, window, cx| {
                            window.dispatch_action(Box::new(ToggleRightPanel), cx);
                        })
                        .unwrap();
                    cx.run_until_parked();
                    shell.read_with(cx, |shell, _| shell.panel_open)
                };

                assert!(!flip());
                assert!(flip());
            }

            #[gpui::test]
            fn should_open_the_new_agent_dialog_on_the_workspace_shown(cx: &mut TestAppContext) {
                let connector = Rc::new(FakeConnector::default());
                let _remote = connector.then_connect_to(workspace_with_a_pane());
                let shell = open(connector, cx);
                let window = cx.windows()[0];

                window
                    .update(cx, |_, window, cx| {
                        window.dispatch_action(Box::new(NewAgent), cx)
                    })
                    .unwrap();
                cx.run_until_parked();

                let dialog = shell.read_with(cx, |shell, _| shell.new_agent.clone().unwrap());
                assert!(dialog.read_with(cx, |d, _| d.is_open()));
            }

            #[gpui::test]
            fn should_show_the_inbox_when_there_is_no_workspace(cx: &mut TestAppContext) {
                let connector = Rc::new(FakeConnector::default());
                let _remote = connector.then_connect_to(Arc::new(FakeApi::default()));

                let shell = open(connector, cx);

                shell.read_with(cx, |shell, _| {
                    assert_eq!(shell.selection, Some(Selection::Inbox));
                    assert!(shell.inbox.is_some());
                });
            }
        }

        mod new_workspace {
            use agentdeck_client::ws::Event;
            use agentdeck_core::ws::{Entity as Changed, ServerMessage};

            use super::*;

            fn dialog(shell: &Entity<Shell>, cx: &TestAppContext) -> Entity<NewWorkspaceDialog> {
                shell.read_with(cx, |shell, _| shell.new_workspace.clone().unwrap())
            }

            fn ask(shell: &Entity<Shell>, cx: &mut TestAppContext) {
                let sidebar = shell.read_with(cx, |shell, _| shell.sidebar.clone().unwrap());
                sidebar.update(cx, |_, cx| cx.emit(SidebarEvent::NewWorkspace));
                cx.run_until_parked();
            }

            #[gpui::test]
            fn should_open_the_dialog_when_the_sidebar_asks(cx: &mut TestAppContext) {
                let connector = Rc::new(FakeConnector::default());
                let _remote = connector.then_connect_to(workspace_with_a_pane());
                let shell = open(connector, cx);

                ask(&shell, cx);

                assert!(dialog(&shell, cx).read_with(cx, |d, _| d.is_open()));
            }

            #[gpui::test]
            fn should_show_the_workspace_it_makes(cx: &mut TestAppContext) {
                let connector = Rc::new(FakeConnector::default());
                let remote = connector.then_connect_to(workspace_with_a_pane());
                let shell = open(connector, cx);
                let dialog = dialog(&shell, cx);
                ask(&shell, cx);
                let window = cx.windows()[0];
                cx.simulate_input(window, "tabs-a11y");
                window
                    .update(cx, |_, window, cx| {
                        dialog.update(cx, |d, cx| d.create(window, cx));
                    })
                    .unwrap();
                // The daemon's broadcast for the new workspace.
                remote
                    .events
                    .try_send(Event::Message(ServerMessage::Changed {
                        entity: Changed::Workspace,
                        id: "x".into(),
                    }))
                    .unwrap();
                cx.run_until_parked();

                assert_eq!(shown_workspace(&shell, cx), Some(id(101)));
            }
        }

        mod new_task {
            use super::*;
            use crate::new_task::{NewTask, NewTaskDialog};

            fn dialog(shell: &Entity<Shell>, cx: &TestAppContext) -> Entity<NewTaskDialog> {
                shell.read_with(cx, |shell, _| shell.new_task.clone().unwrap())
            }

            #[gpui::test]
            fn should_open_the_dialog_on_the_inbox_when_the_tasks_view_asks(
                cx: &mut TestAppContext,
            ) {
                let connector = Rc::new(FakeConnector::default());
                let _remote = connector.then_connect_to(workspace_with_a_pane());
                let shell = open(connector, cx);
                let tasks = shell.read_with(cx, |shell, _| shell.tasks.clone().unwrap());

                tasks.update(cx, |_, cx| cx.emit(NewTask(None)));
                cx.run_until_parked();

                let dialog = dialog(&shell, cx);
                assert!(dialog.read_with(cx, |d, _| d.is_open()));
                assert_eq!(dialog.read_with(cx, |d, _| d.workspace()), None);
            }

            #[gpui::test]
            fn should_open_the_dialog_on_the_workspace_when_the_right_panel_asks(
                cx: &mut TestAppContext,
            ) {
                let connector = Rc::new(FakeConnector::default());
                let _remote = connector.then_connect_to(workspace_with_a_pane());
                let shell = open(connector, cx);
                let panel = shell.read_with(cx, |shell, _| shell.panel.clone().unwrap());

                panel.update(cx, |_, cx| cx.emit(NewTask(Some(id(1)))));
                cx.run_until_parked();

                let dialog = dialog(&shell, cx);
                assert!(dialog.read_with(cx, |d, _| d.is_open()));
                assert_eq!(dialog.read_with(cx, |d, _| d.workspace()), Some(id(1)));
            }
        }

        mod task_sheet {
            use super::*;

            fn sheet(shell: &Entity<Shell>, cx: &TestAppContext) -> Entity<TaskSheet> {
                shell.read_with(cx, |shell, _| shell.task_sheet.clone().expect("a sheet"))
            }

            /// A daemon with workspace 1 and its task 2.
            fn daemon() -> Arc<FakeApi> {
                let api = workspace_with_a_pane();
                let mut snapshot = api.snapshot_now();
                snapshot.tasks = vec![crate::overview::fixtures::task(
                    2,
                    agentdeck_core::TaskKind::Plain,
                    Some(id(1)),
                )];
                api.set(snapshot);
                api
            }

            #[gpui::test]
            fn should_open_the_sheet_on_the_task_the_tasks_view_asks_for(cx: &mut TestAppContext) {
                let connector = Rc::new(FakeConnector::default());
                let _remote = connector.then_connect_to(daemon());
                let shell = open(connector, cx);
                let tasks = shell.read_with(cx, |shell, _| shell.tasks.clone().unwrap());

                tasks.update(cx, |_, cx| cx.emit(OpenTask(id(2))));
                cx.run_until_parked();

                let sheet = sheet(&shell, cx);
                assert!(sheet.read_with(cx, |s, _| s.is_open()));
                assert_eq!(sheet.read_with(cx, |s, _| s.task()), Some(id(2)));
            }

            #[gpui::test]
            fn should_open_the_sheet_when_the_inbox_or_the_right_panel_asks(
                cx: &mut TestAppContext,
            ) {
                let connector = Rc::new(FakeConnector::default());
                let api = daemon();
                let _remote = connector.then_connect_to(api.clone());
                let shell = open(connector, cx);
                let (inbox, panel) = shell.read_with(cx, |shell, _| {
                    (shell.inbox.clone().unwrap(), shell.panel.clone().unwrap())
                });

                inbox.update(cx, |_, cx| cx.emit(OpenTask(id(2))));
                cx.run_until_parked();
                panel.update(cx, |_, cx| cx.emit(OpenTask(id(2))));
                cx.run_until_parked();

                assert_eq!(
                    *api.seen.lock().unwrap(),
                    [id::<agentdeck_core::TaskId>(2); 2]
                );
            }
        }

        mod top_bar {
            use agentdeck_client::ws::Event;
            use agentdeck_core::ws::{Entity as Changed, ServerMessage};

            use super::*;
            use crate::top_bar::TopBar;

            fn shown_top_bar(shell: &Entity<Shell>, cx: &TestAppContext) -> Option<TopBar> {
                shell.read_with(cx, |shell, cx| {
                    (shell.workspace.as_ref()).map(|view| view.read(cx).top_bar().clone())
                })
            }

            #[gpui::test]
            fn should_head_the_workspace_with_its_name_and_dir(cx: &mut TestAppContext) {
                let connector = Rc::new(FakeConnector::default());
                let _remote = connector.then_connect_to(workspace_with_a_pane());

                let shell = open(connector, cx);

                assert_eq!(
                    shown_top_bar(&shell, cx),
                    Some(TopBar::new("agentdeck", "/w/agentdeck"))
                );
            }

            #[gpui::test]
            fn should_follow_a_rename(cx: &mut TestAppContext) {
                let connector = Rc::new(FakeConnector::default());
                let api = workspace_with_a_pane();
                let remote = connector.then_connect_to(api.clone());
                let shell = open(connector, cx);
                api.set(Snapshot {
                    workspaces: vec![workspace(1, "renamed")],
                    ..Snapshot::default()
                });

                remote
                    .events
                    .try_send(Event::Message(ServerMessage::Changed {
                        entity: Changed::Workspace,
                        id: id::<WorkspaceId>(1).to_string(),
                    }))
                    .unwrap();
                cx.run_until_parked();

                assert_eq!(
                    shown_top_bar(&shell, cx),
                    Some(TopBar::new("renamed", "/w/renamed"))
                );
            }
        }
    }

    mod archive {
        use agentdeck_client::types::Workspace;
        use gpui::Modifiers;

        use super::*;
        use crate::notice::{self, Notice};
        use crate::top_bar::ArchiveWorkspace;

        struct Harness {
            shell: Entity<Shell>,
            api: Arc<FakeApi>,
            _remote: agentdeck_client::ws::Remote,
        }

        impl Harness {
            /// A shell showing `api`'s workspaces, the first selected.
            fn open(
                api: Arc<FakeApi>,
                cx: &mut TestAppContext,
            ) -> (Harness, &mut VisualTestContext) {
                cx.update(brrritscold_core_gpui::init);
                cx.update(crate::terminal::init);
                let connector = Rc::new(FakeConnector::default());
                let remote = connector.then_connect_to(api.clone());
                let mut shell = None;
                let window = cx.add_window(|window, cx| {
                    let connection = cx.new(|cx| Connection::new(connector, cx));
                    let view = cx.new(|cx| Shell::new(connection, window, cx));
                    shell = Some(view.clone());
                    Root::new(view, window, cx)
                });
                let cx = VisualTestContext::from_window(*window, cx).into_mut();
                let harness = Harness {
                    shell: shell.unwrap(),
                    api,
                    _remote: remote,
                };
                harness.settle(cx);
                (harness, cx)
            }

            fn settle(&self, cx: &mut VisualTestContext) {
                cx.run_until_parked();
                cx.update(|window, cx| window.draw(cx).clear(cx));
                cx.run_until_parked();
                cx.update(|window, cx| window.draw(cx).clear(cx));
            }

            fn click(&self, at: gpui::Point<gpui::Pixels>, cx: &mut VisualTestContext) {
                cx.simulate_click(at, Modifiers::default());
                self.settle(cx);
            }

            fn select(&self, workspace: u32, cx: &mut VisualTestContext) {
                let sidebar = self.shell.read_with(cx, |shell, _| shell.sidebar.clone());
                sidebar.unwrap().update(cx, |sidebar, cx| {
                    sidebar.select(Selection::Workspace(id(workspace)), cx);
                });
                self.settle(cx);
            }

            fn archive(&self, cx: &mut VisualTestContext) {
                cx.dispatch_action(ArchiveWorkspace);
                self.settle(cx);
            }

            /// Whether the sidebar has workspace `n` as archived.
            fn is_archived(&self, n: u32, cx: &mut VisualTestContext) -> bool {
                self.shell.read_with(cx, |shell, cx| {
                    let sidebar = shell.sidebar.as_ref().unwrap().read(cx);
                    sidebar.overview().workspace(id(n)).unwrap().archived
                })
            }

            /// The top bar of the workspace shown says it's archived.
            fn shows_archived(&self, cx: &mut VisualTestContext) -> bool {
                self.shell.read_with(cx, |shell, cx| {
                    let view = shell
                        .workspace
                        .as_ref()
                        .expect("a workspace shows")
                        .read(cx);
                    view.top_bar().is_archived()
                })
            }

            fn last_notice(&self, cx: &mut VisualTestContext) -> Notice {
                cx.update(|_, cx| notice::shown(cx).last().cloned().expect("a toast"))
            }
        }

        /// Workspace 1, `agentdeck`, live; workspace 2, `old`, archived.
        fn daemon() -> Arc<FakeApi> {
            let old = Workspace {
                archived_at: Some("2026-11-13T00:00:00Z".into()),
                ..workspace(2, "old")
            };
            Arc::new(FakeApi::with(Snapshot {
                workspaces: vec![workspace(1, "agentdeck"), old],
                ..Snapshot::default()
            }))
        }

        #[gpui::test]
        fn should_archive_the_selected_workspace_from_the_top_bar_menu(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(), cx);

            let trigger = cx.debug_bounds("workspace menu").expect("the menu shows");
            harness.click(trigger.center(), cx);
            let menu = cx.debug_bounds("dropdown-menu-content").expect("it opens");
            // The first item: the menu's 4px padding, then half a 32px row.
            harness.click(gpui::point(menu.center().x, menu.top() + gpui::px(20.)), cx);

            assert_eq!(*harness.api.archived.lock().unwrap(), [id(1)]);
        }

        #[gpui::test]
        fn should_toast_the_daemons_refusal_and_leave_it_unarchived(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(), cx);
            harness
                .api
                .refuse_next("panes_running", "Close its 2 running panes first");

            harness.archive(cx);

            let notice = harness.last_notice(cx);
            assert_eq!(notice.title, "Can't archive agentdeck");
            assert_eq!(
                notice.description.as_deref(),
                Some("Close its 2 running panes first")
            );
            assert!(!harness.is_archived(1, cx));
            assert!(!harness.shows_archived(cx));
        }

        #[gpui::test]
        fn should_keep_the_workspace_selected_and_show_it_archived(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(), cx);

            harness.archive(cx);

            assert!(harness.is_archived(1, cx));
            assert_eq!(shown(&harness, cx), Some(id(1)));
            assert!(harness.shows_archived(cx));
        }

        #[gpui::test]
        fn should_toast_the_archive_with_an_undo_that_unarchives(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(), cx);

            harness.archive(cx);
            let notice = harness.last_notice(cx);
            assert_eq!(notice.title, "Archived agentdeck");
            assert_eq!(
                notice.description.as_deref(),
                Some("Its folder, tasks and instructions are kept.")
            );
            let undo = notice.action.expect("an Undo");
            assert_eq!(undo.label, "Undo");
            cx.update(|window, cx| undo.run(window, cx));
            harness.settle(cx);

            assert_eq!(*harness.api.unarchived.lock().unwrap(), [id(1)]);
            assert!(!harness.is_archived(1, cx));
            assert!(!harness.shows_archived(cx));
        }

        #[gpui::test]
        fn should_unarchive_from_the_top_bars_button(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(), cx);
            harness.select(2, cx);
            assert!(harness.shows_archived(cx));

            let button = cx.debug_bounds("unarchive").expect("Unarchive shows");
            harness.click(button.center(), cx);

            assert_eq!(*harness.api.unarchived.lock().unwrap(), [id(2)]);
            assert!(!harness.is_archived(2, cx));
            assert!(!harness.shows_archived(cx));
        }

        #[gpui::test]
        fn should_toast_a_failed_unarchive(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(), cx);
            harness.select(2, cx);
            harness.api.fail_next("connection refused");

            let button = cx.debug_bounds("unarchive").expect("Unarchive shows");
            harness.click(button.center(), cx);

            let notice = harness.last_notice(cx);
            assert_eq!(notice.title, "Couldn't unarchive old");
            assert!(harness.is_archived(2, cx));
        }

        fn shown(harness: &Harness, cx: &mut VisualTestContext) -> Option<WorkspaceId> {
            harness.shell.read_with(cx, |shell, cx| {
                (shell.workspace.as_ref()).map(|view| view.read(cx).workspace())
            })
        }

        mod delete {
            use super::*;
            use crate::delete_workspace::DeleteWorkspaceDialog;
            use crate::top_bar::DeleteWorkspace;

            fn dialog(
                harness: &Harness,
                cx: &mut VisualTestContext,
            ) -> Entity<DeleteWorkspaceDialog> {
                harness
                    .shell
                    .read_with(cx, |shell, _| shell.delete_workspace.clone().unwrap())
            }

            fn is_open(harness: &Harness, cx: &mut VisualTestContext) -> bool {
                dialog(harness, cx).read_with(cx, |d, _| d.is_open())
            }

            /// Opens the top bar's menu and picks its `n`th item by keyboard.
            fn pick_from_menu(harness: &Harness, n: usize, cx: &mut VisualTestContext) {
                let trigger = cx.debug_bounds("workspace menu").expect("the menu shows");
                harness.click(trigger.center(), cx);
                cx.simulate_keystrokes(&format!("{}enter", "down ".repeat(n)));
                harness.settle(cx);
            }

            fn open_and_delete(harness: &Harness, name: &str, cx: &mut VisualTestContext) {
                cx.dispatch_action(DeleteWorkspace);
                harness.settle(cx);
                cx.simulate_input(name);
                cx.simulate_keystrokes("enter");
                harness.settle(cx);
            }

            fn selection(harness: &Harness, cx: &mut VisualTestContext) -> Option<Selection> {
                harness.shell.read_with(cx, |shell, _| shell.selection)
            }

            #[gpui::test]
            fn should_open_from_the_top_bar_menu_after_archive(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(daemon(), cx);

                pick_from_menu(&harness, 2, cx);

                assert!(is_open(&harness, cx));
                assert_eq!(*harness.api.archived.lock().unwrap(), []);
            }

            #[gpui::test]
            fn should_open_from_an_archived_workspaces_menu(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(daemon(), cx);
                harness.select(2, cx);

                pick_from_menu(&harness, 1, cx);

                assert!(is_open(&harness, cx));
                assert_eq!(*harness.api.unarchived.lock().unwrap(), []);
            }

            #[gpui::test]
            fn should_archive_instead_when_asked(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(daemon(), cx);
                cx.dispatch_action(DeleteWorkspace);
                harness.settle(cx);

                let dialog = dialog(&harness, cx);
                cx.update(|window, cx| dialog.update(cx, |d, cx| d.archive_instead(window, cx)));
                harness.settle(cx);

                assert_eq!(*harness.api.archived.lock().unwrap(), [id(1)]);
                assert!(!is_open(&harness, cx));
                assert_eq!(harness.last_notice(cx).title, "Archived agentdeck");
            }

            #[gpui::test]
            fn should_toast_a_refusal_to_archive_instead(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(daemon(), cx);
                cx.dispatch_action(DeleteWorkspace);
                harness.settle(cx);
                harness
                    .api
                    .refuse_next("panes_running", "Close its 1 running pane first");

                let dialog = dialog(&harness, cx);
                cx.update(|window, cx| dialog.update(cx, |d, cx| d.archive_instead(window, cx)));
                harness.settle(cx);

                let notice = harness.last_notice(cx);
                assert_eq!(notice.title, "Can't archive agentdeck");
                assert!(!harness.is_archived(1, cx));
            }

            #[gpui::test]
            fn should_select_the_first_workspace_left_once_deleted(cx: &mut TestAppContext) {
                let api = daemon();
                let mut snapshot = api.snapshot_now();
                snapshot.workspaces.push(workspace(3, "third"));
                api.set(snapshot);
                let (harness, cx) = Harness::open(api, cx);
                harness.select(3, cx);

                open_and_delete(&harness, "third", cx);

                assert_eq!(*harness.api.deleted.lock().unwrap(), [id(3)]);
                assert!(!is_open(&harness, cx));
                assert_eq!(selection(&harness, cx), Some(Selection::Workspace(id(1))));
            }

            #[gpui::test]
            fn should_select_the_inbox_once_the_last_workspace_is_deleted(cx: &mut TestAppContext) {
                let api = Arc::new(FakeApi::with(Snapshot {
                    workspaces: vec![workspace(1, "agentdeck")],
                    ..Snapshot::default()
                }));
                let (harness, cx) = Harness::open(api, cx);

                open_and_delete(&harness, "agentdeck", cx);

                assert_eq!(*harness.api.deleted.lock().unwrap(), [id(1)]);
                assert_eq!(selection(&harness, cx), Some(Selection::Inbox));
            }
        }
    }

    mod window_actions {
        use super::*;
        use crate::title_bar::Close;

        #[gpui::test]
        fn should_remove_the_window_on_close(cx: &mut TestAppContext) {
            cx.update(brrritscold_core_gpui::init);
            let window = cx.add_window(|window, cx| {
                let connection = cx.new(|cx| Connection::new(Rc::new(Pending), cx));
                let shell = cx.new(|cx| Shell::new(connection, window, cx));
                Root::new(shell, window, cx)
            });
            let handle = window.into();
            let cx = &mut VisualTestContext::from_window(handle, cx);
            cx.update(|window, cx| window.draw(cx).clear(cx));

            cx.dispatch_action(Close);
            cx.run_until_parked();

            assert!(!cx.windows().contains(&handle));
        }
    }
}
