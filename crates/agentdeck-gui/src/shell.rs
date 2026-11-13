//! The main window's content: what the daemon connection allows. Until it connects, a
//! centred status; once it does, the sidebar beside the selected view: a workspace's pane
//! or the Tasks view (the Inbox arrives with a later PR), and the new workspace dialog the
//! sidebar's `+` opens. When the compositor leaves decorations to the app, a title bar and
//! resize edges frame it.

use brrritscold_core_gpui::{
    ActiveTheme as _, Alert, AlertAction, AlertDescription, AlertTitle, AlertVariant, Button,
    ButtonSize, ButtonVariant, IconName, Separator, SeparatorOrientation,
};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Decorations, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _, Subscription,
    Window, div, px,
};

use crate::connection::{Connection, Status};
use crate::frame;
use crate::new_workspace::{Created, NewWorkspaceDialog};
use crate::sidebar::{Selection, SidebarEvent, SidebarView};
use crate::tasks::TasksView;
use crate::title_bar::{Close, Minimize, TitleBar, ToggleMaximize};
use crate::top_bar::TopBar;
use crate::workspace::WorkspaceView;

/// The main window's root view, inside core-gpui's `Root`.
pub struct Shell {
    connection: Entity<Connection>,
    /// The sidebar, while connected; a new connection gets a new one.
    sidebar: Option<Entity<SidebarView>>,
    /// Follows the sidebar's selection.
    _selection: Option<Subscription>,
    /// The new workspace dialog, while connected, and what joins it to the sidebar.
    new_workspace: Option<Entity<NewWorkspaceDialog>>,
    _new_workspace: Vec<Subscription>,
    /// The selected workspace.
    workspace: Option<Entity<WorkspaceView>>,
    /// The Tasks view, while connected; shown while the sidebar selects Tasks.
    tasks: Option<Entity<TasksView>>,
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
            _new_workspace: Vec::new(),
            workspace: None,
            tasks: None,
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
                    self.tasks = Some(cx.new(|cx| TasksView::new(api.clone(), window, cx)));
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
                        cx.subscribe(&dialog, |shell, _, Created(id), cx| {
                            if let Some(sidebar) = &shell.sidebar {
                                let selection = Selection::Workspace(*id);
                                sidebar.update(cx, |sidebar, cx| sidebar.select(selection, cx));
                            }
                        }),
                    ];
                    self.new_workspace = Some(dialog);
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
        if let Some(tasks) = &self.tasks {
            tasks.update(cx, |tasks, cx| tasks.set_snapshot(snapshot, cx));
        }
        let Status::Connected(session) = self.connection.read(cx).status() else {
            return;
        };
        let sidebar = sidebar.read(cx);
        let row = match selection {
            Selection::Workspace(selected) => {
                (sidebar.overview().workspaces.iter()).find(|row| row.id == selected)
            }
            Selection::Inbox | Selection::Tasks => None,
        };
        let Some(row) = row else {
            self.workspace = None;
            cx.notify();
            return;
        };
        let (selected, top_bar) = (row.id, TopBar::new(row.name.clone(), row.dir.clone()));
        if let Some(view) = &self.workspace
            && view.read(cx).workspace() == selected
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
                .map(
                    |body| match (self.selection, &self.workspace, &self.tasks) {
                        (Some(Selection::Workspace(_)), Some(workspace), _) => {
                            body.child(workspace.clone())
                        }
                        (Some(Selection::Tasks), _, Some(tasks)) => body.child(tasks.clone()),
                        _ => body.child(div().flex_1()),
                    },
                )
                .children(self.new_workspace.clone())
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
