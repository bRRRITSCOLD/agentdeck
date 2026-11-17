//! The main window's content: what the daemon connection allows. Until it connects, a
//! centred status; once it does, the sidebar beside the selected view, under a banner with a
//! Restart while the daemon predates the app: a workspace's pane,
//! the Tasks view, the Inbox or Settings, and the new workspace dialog the
//! sidebar's `+` opens. Settings is made as the window connects, so the saved theme applies
//! from the start. When the compositor leaves decorations to the app, a title bar and
//! resize edges frame it.
//!
//! It mutes and unmutes the selected workspace's notifications when the top bar asks, toasting
//! a failure. It archives and unarchives the selected workspace when the top bar asks, toasting
//! the outcome; an archive's toast offers Undo. The top bar's Edit workspace opens the edit
//! dialog on the selected workspace, and the sidebar reads again once it renames. Its Delete
//! workspace… opens the delete dialog; once a workspace is deleted the selection falls back to the first workspace left,
//! else the Inbox.
//!
//! The Tasks view and the right panel ask for the new task dialog with
//! [`NewTask`], preset to the Inbox or their workspace. They and the Inbox ask for the task
//! sheet with [`OpenTask`] when a task's title is clicked, and for it editing the task with
//! [`EditTask`] from a row's **Edit task**; the sheet's Start agent asks for
//! the new agent dialog on the task's workspace with [`StartAgentOn`]. Once an agent starts,
//! the window shows its workspace and tab. A pane header's Link to task reaches it as the
//! workspace view's [`LinkPane`] and opens the Link to task dialog on that pane.
//!
//! It shows the sidebar's [`Alerts`] as OS notifications (spec §11), leaving out one while the
//! user can see what it is about in the focused window: the pane's tab, the task sheet on the
//! PR task, the Tasks view. Clicking one focuses the window and shows what it is about. One
//! still open is withdrawn once the user can see what it is about: the window focused, the
//! workspace, tab or task sheet shown.
//!
//! Closing the window (its own close button or the compositor's) and Quit ask first while an
//! agent runs, unless `gui.confirm_quit` is off (spec §11): "Quit agentdeck?", Penpot "Screens"
//! → "Quit · agents running". Quit, or Enter, quits, after saving `gui.confirm_quit = false`
//! when Don't ask again is ticked; Cancel, or Escape, keeps the app.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use agentdeck_client::ApiError;
use agentdeck_core::{TaskId, WorkspaceId};
use agentdeck_notify::Notifier;
use brrritscold_core_gpui::{
    ActiveTheme as _, Alert, AlertAction, AlertDescription, AlertDialog, AlertDialogAction,
    AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter,
    AlertDialogHeader, AlertDialogTitle, AlertTitle, AlertVariant, Button, ButtonSize,
    ButtonVariant, Checkbox, DialogHandle, Field, FieldContent, FieldOrientation, IconName, Label,
    Separator, SeparatorOrientation,
};
use brrritscold_core_gpui_tokens::primitive::space;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Decorations, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, Subscription, Window, div, px,
};
use gpui_base::actions::Confirm;

use crate::activity;
use crate::api::Api;
use crate::connection::{Connection, Status};
use crate::delete_workspace::{DeleteEvent, DeleteTarget, DeleteWorkspaceDialog};
use crate::edit_workspace::{EditTarget, EditWorkspaceDialog, Renamed};
use crate::frame;
use crate::inbox::{InboxView, OpenSettings, Synced};
use crate::keymap::ToggleRightPanel;
use crate::link_task::{self, LinkTaskDialog};
use crate::new_agent::{self, Launched, NewAgent, NewAgentDialog, Target};
use crate::new_task::{NewTask, NewTaskDialog};
use crate::new_workspace::{Created, NewWorkspaceDialog};
use crate::notice::{self, Notice, NoticeAction};
use crate::notifications::{self, Alert as Notification};
use crate::quit;
use crate::right_panel::{self, RightPanel};
use crate::settings::SettingsView;
use crate::sidebar::{Alerts, Selection, SidebarEvent, SidebarView};
use crate::task_sheet::{EditTask, OpenTask, StartAgentOn, TaskSheet};
use crate::tasks::TasksView;
use crate::title_bar::{Close, Minimize, TitleBar, ToggleMaximize};
use crate::top_bar::{
    ArchiveWorkspace, DeleteWorkspace, EditWorkspace, MuteWorkspace, TopBar, UnarchiveWorkspace,
    UnmuteWorkspace,
};
use crate::workspace::{LinkPane, WorkspaceView, failed};

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
    /// Follows the sidebar's selection and snapshot, and opens the new workspace dialog
    /// from its `+`.
    _selection: Option<Subscription>,
    /// The new workspace dialog, while connected, and what joins it to the sidebar.
    new_workspace: Option<Entity<NewWorkspaceDialog>>,
    /// The new agent dialog, while connected.
    new_agent: Option<Entity<NewAgentDialog>>,
    /// The Edit workspace dialog, while connected.
    edit_workspace: Option<Entity<EditWorkspaceDialog>>,
    /// The Delete workspace dialog, while connected.
    delete_workspace: Option<Entity<DeleteWorkspaceDialog>>,
    /// The new task dialog, while connected.
    new_task: Option<Entity<NewTaskDialog>>,
    /// The task sheet, while connected.
    task_sheet: Option<Entity<TaskSheet>>,
    /// The Link to task dialog, while connected.
    link_task: Option<Entity<LinkTaskDialog>>,
    _new_workspace: Vec<Subscription>,
    /// The selected workspace.
    workspace: Option<Entity<WorkspaceView>>,
    /// Follows the selected workspace's asks to link a pane, and the tab it shows.
    _workspace: Vec<Subscription>,
    /// The Tasks view, while connected; shown while the sidebar selects Tasks.
    tasks: Option<Entity<TasksView>>,
    /// The Inbox, while connected; shown while the sidebar selects it.
    inbox: Option<Entity<InboxView>>,
    /// Settings, while connected; shown while the sidebar's gear selects it. It also applies
    /// the saved theme.
    settings: Option<Entity<SettingsView>>,
    /// The right panel beside a workspace, while connected.
    panel: Option<Entity<RightPanel>>,
    /// Whether the right panel shows; the top bar's panel button flips it.
    panel_open: bool,
    /// What the sidebar selects.
    selection: Option<Selection>,
    /// Held by the shell so the title bar's actions reach its handlers while nothing inside
    /// has focus.
    focus: FocusHandle,
    /// Shows the alerts as OS notifications.
    notifier: Rc<dyn Notifier>,
    /// What each notification still open is about, by its tag: what a click opens, and what
    /// withdraws it once the user can see it.
    alerts: HashMap<SharedString, notifications::Target>,
    /// Withdraws the notifications about what the window shows once it is focused.
    _activation: Subscription,
    /// Times the window's input, so its turning marks hold still while the user is away.
    _activity: [Subscription; 2],
    /// Opens and closes "Quit agentdeck?".
    quit_dialog: DialogHandle,
    /// How many agents ran when the quit dialog opened.
    quit_running: usize,
    /// Whether the quit dialog's Don't ask again is ticked.
    dont_ask_again: bool,
}

impl Shell {
    /// A shell that follows `connection`, focused in `window`, showing its alerts through
    /// `notifier`.
    pub fn new(
        connection: Entity<Connection>,
        notifier: Rc<dyn Notifier>,
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
        // A click on a notification's body focuses the window and shows what it is about.
        let this = cx.entity().downgrade();
        let handle = window.window_handle();
        notifier.on_click(Box::new(move |tag, cx| {
            let _ = handle.update(cx, |_, window, cx| {
                window.activate_window();
                let _ = this.update(cx, |shell, cx| shell.open_alert(&tag, window, cx));
            });
        }));
        let activation = cx.observe_window_activation(window, |shell, window, cx| {
            shell.withdraw_seen(window, cx);
        });
        let mut shell = Shell {
            connection,
            sidebar: None,
            _selection: None,
            new_workspace: None,
            new_agent: None,
            edit_workspace: None,
            delete_workspace: None,
            new_task: None,
            task_sheet: None,
            link_task: None,
            _new_workspace: Vec::new(),
            workspace: None,
            _workspace: Vec::new(),
            tasks: None,
            inbox: None,
            settings: None,
            panel: None,
            panel_open: true,
            selection: None,
            focus,
            notifier,
            alerts: HashMap::new(),
            _activation: activation,
            _activity: activity::watch(window, cx),
            quit_dialog: DialogHandle::new(false),
            quit_running: 0,
            dont_ask_again: false,
        };
        // The compositor's close button, and Quit, ask first while agents run.
        let this = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |shell, cx| !shell.ask_to_quit(window, cx))
                .unwrap_or(true)
        });
        let this = cx.entity().downgrade();
        let handle = window.window_handle();
        quit::ask_first_with(
            move |cx| {
                handle
                    .update(cx, |_, window, cx| {
                        this.update(cx, |shell, cx| shell.ask_to_quit(window, cx))
                            .unwrap_or(false)
                    })
                    .unwrap_or(false)
            },
            cx,
        );
        shell.follow_connection(window, cx);
        shell
    }

    /// Opens "Quit agentdeck?" while an agent runs and `gui.confirm_quit` is on; whether it
    /// did, so the caller quits only when it didn't. Agent panes whose process runs count, and
    /// shell panes hosting an agent; plain shells don't.
    pub fn ask_to_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let running = (self.sidebar.as_ref()).map_or(0, |sidebar| {
            quit::running_agents(sidebar.read(cx).snapshot())
        });
        let confirm = (self.settings.as_ref()).is_none_or(|view| view.read(cx).confirm_quit());
        if running == 0 || !confirm {
            return false;
        }
        if !self.quit_dialog.is_open() {
            self.quit_running = running;
            self.dont_ask_again = false;
            self.quit_dialog.open(window, cx);
        }
        cx.notify();
        true
    }

    /// The quit dialog's Quit, and Enter: saves `gui.confirm_quit = false` first when Don't ask
    /// again is ticked, then quits, whether or not the save went through.
    fn quit_confirmed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quit_dialog.close(window, cx);
        let saving = (self.settings.clone())
            .filter(|_| self.dont_ask_again)
            .map(|view| view.update(cx, |view, cx| view.set_confirm_quit(false, window, cx)));
        cx.spawn(async move |_, cx| {
            if let Some(saving) = saving {
                saving.await;
            }
            cx.update(quit::now);
        })
        .detach();
    }

    /// "Quit agentdeck?", Penpot "Screens" → "Quit · agents running": the running count over
    /// Don't ask again, then Cancel and Quit. Enter and the Quit button dispatch the dialog's
    /// `Confirm`, caught here on its way down to the dialog, which then closes.
    fn quit_confirmation(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let tick = move |on: bool, cx: &mut App| {
            let _ = this.update(cx, |shell, cx| {
                shell.dont_ask_again = on;
                cx.notify();
            });
        };
        let tick_label = tick.clone();
        let ticked = self.dont_ask_again;
        // core-gpui's content has no body part, so Don't ask again leads the footer's column,
        // `space.6` above the buttons, as Penpot places it between the header and the footer.
        let dont_ask = Field::new()
            .orientation(FieldOrientation::Horizontal)
            .child(
                div().debug_selector(|| "quit dont ask again".into()).child(
                    Checkbox::new("quit-dont-ask-again")
                        .checked(ticked)
                        .accessibility_label("Don't ask again")
                        .on_change(move |on, _, cx| tick(on, cx)),
                ),
            )
            .content(
                FieldContent::new().child(
                    div()
                        .id("quit-dont-ask-again-label")
                        .cursor_pointer()
                        .on_click(move |_, _, cx| tick_label(!ticked, cx))
                        .child(Label::new().child("Don't ask again")),
                ),
            );
        div()
            .capture_action(cx.listener(|this, _: &Confirm, window, cx| {
                this.quit_confirmed(window, cx);
            }))
            .child(
                AlertDialog::new("quit")
                    .handle(self.quit_dialog.clone())
                    .content(
                        AlertDialogContent::new()
                            .header(
                                AlertDialogHeader::new()
                                    .title(AlertDialogTitle::new("Quit agentdeck?"))
                                    .description(AlertDialogDescription::new(quit::still_running(
                                        self.quit_running,
                                    ))),
                            )
                            .footer(
                                AlertDialogFooter::new()
                                    .flex_col()
                                    .gap(space::S6)
                                    .child(dont_ask)
                                    .child(
                                        div()
                                            .flex()
                                            .justify_end()
                                            .gap(space::S2)
                                            .child(AlertDialogCancel::new("quit-cancel", "Cancel"))
                                            .child(
                                                div()
                                                    .debug_selector(|| "quit confirm".into())
                                                    .child(AlertDialogAction::new(
                                                        "quit-confirm",
                                                        "Quit",
                                                    )),
                                            ),
                                    ),
                            ),
                    ),
            )
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
                    let inbox = cx.new(|cx| InboxView::new(api.clone(), cx));
                    let new_agent = cx.new(|_| NewAgentDialog::new(api.clone()));
                    let link_task = cx.new(|_| LinkTaskDialog::new(api.clone()));
                    let edit = cx.new(|cx| EditWorkspaceDialog::new(api.clone(), window, cx));
                    let delete = cx.new(|cx| DeleteWorkspaceDialog::new(api.clone(), window, cx));
                    let new_task = cx.new(|cx| NewTaskDialog::new(api.clone(), window, cx));
                    let panel = cx.new(|cx| RightPanel::new(api.clone(), window, cx));
                    let tasks = cx.new(|cx| TasksView::new(api.clone(), window, cx));
                    let settings =
                        cx.new(|cx| SettingsView::new(api.clone(), &connection, window, cx));
                    let task_sheet =
                        cx.new(|cx| TaskSheet::new(api.clone(), &connection, window, cx));
                    let sidebar = cx.new(|cx| SidebarView::new(api, &connection, cx));
                    self._selection = Some(cx.subscribe_in(
                        &sidebar,
                        window,
                        |shell, _, event: &SidebarEvent, window, cx| match event {
                            SidebarEvent::Changed => {
                                shell.follow_selection(window, cx);
                                shell.withdraw_seen(window, cx);
                            }
                            SidebarEvent::NewWorkspace => {
                                if let Some(dialog) = &shell.new_workspace {
                                    dialog.update(cx, |dialog, cx| dialog.open(window, cx));
                                }
                            }
                        },
                    ));
                    self._new_workspace = vec![
                        cx.subscribe_in(
                            &sidebar,
                            window,
                            |shell, _, alerts: &Alerts, window, cx| {
                                shell.notify(&alerts.0, window, cx);
                            },
                        ),
                        // A sync that changes no task broadcasts nothing, yet moves when PRs
                        // last synced.
                        cx.subscribe(&inbox, |shell, _, _: &Synced, cx| {
                            if let Some(sidebar) = &shell.sidebar {
                                sidebar.update(cx, |sidebar, cx| sidebar.refresh(cx));
                            }
                        }),
                        // Show the agent's workspace and tab, and focus it: it may be asking
                        // about trust.
                        cx.subscribe_in(
                            &new_agent,
                            window,
                            |shell, _, launched: &Launched, window, cx| {
                                shell.show_launched(*launched, window, cx);
                            },
                        ),
                        cx.observe_in(&task_sheet, window, |shell, _, window, cx| {
                            shell.withdraw_seen(window, cx);
                        }),
                        cx.subscribe_in(
                            &task_sheet,
                            window,
                            |shell, _, ask: &StartAgentOn, window, cx| {
                                shell.open_new_agent(ask.workspace, Some(ask.task), window, cx);
                            },
                        ),
                        // New task's Send to: New agent.
                        cx.subscribe_in(
                            &new_task,
                            window,
                            |shell, _, ask: &StartAgentOn, window, cx| {
                                shell.open_new_agent(ask.workspace, Some(ask.task), window, cx);
                            },
                        ),
                        // The rename also comes back through `changed`; reading now shows it
                        // at once.
                        cx.subscribe(&edit, |shell, _, _: &Renamed, cx| shell.refresh(cx)),
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
                        cx.subscribe(&inbox, |shell, _, _: &OpenSettings, cx| {
                            if let Some(sidebar) = &shell.sidebar {
                                let settings = Selection::Settings;
                                sidebar.update(cx, |sidebar, cx| sidebar.select(settings, cx));
                            }
                            // At its GitHub section, the top.
                            if let Some(settings) = &shell.settings {
                                settings.update(cx, |settings, cx| settings.scroll_to_github(cx));
                            }
                        }),
                        cx.subscribe_in(&panel, window, |shell, _, ask, window, cx| {
                            shell.open_task(*ask, window, cx);
                        }),
                        cx.subscribe_in(&tasks, window, |shell, _, ask, window, cx| {
                            shell.edit_task(*ask, window, cx);
                        }),
                        cx.subscribe_in(&inbox, window, |shell, _, ask, window, cx| {
                            shell.edit_task(*ask, window, cx);
                        }),
                        cx.subscribe_in(&panel, window, |shell, _, ask, window, cx| {
                            shell.edit_task(*ask, window, cx);
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
                    self.edit_workspace = Some(edit);
                    self.delete_workspace = Some(delete);
                    self.new_task = Some(new_task);
                    self.task_sheet = Some(task_sheet);
                    self.link_task = Some(link_task);
                    self.tasks = Some(tasks);
                    self.settings = Some(settings);
                    self.panel = Some(panel);
                    self.sidebar = Some(sidebar);
                    self.follow_selection(window, cx);
                }
            }
            Status::Connecting | Status::Restarting | Status::Failed(_) => {
                self.sidebar = None;
                self._selection = None;
                self.new_workspace = None;
                self._new_workspace.clear();
                self.workspace = None;
                self._workspace.clear();
                self.tasks = None;
                self.inbox = None;
                self.settings = None;
                self.panel = None;
                self.new_agent = None;
                self.edit_workspace = None;
                self.delete_workspace = None;
                self.new_task = None;
                self.task_sheet = None;
                self.link_task = None;
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
        if let Some(link_task) = &self.link_task {
            let snapshot = snapshot.clone();
            link_task.update(cx, |dialog, cx| dialog.set_snapshot(snapshot, cx));
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
            Selection::Inbox | Selection::Tasks | Selection::Settings => None,
        };
        let Some(row) = row else {
            self.workspace = None;
            cx.notify();
            return;
        };
        let top_bar = TopBar::new(row.name.clone(), row.dir.clone())
            .archived(row.archived)
            .muted(row.muted);
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
        let view =
            cx.new(|cx| WorkspaceView::new(selected, top_bar, api, connection, sender, window, cx));
        self._workspace = vec![
            cx.subscribe_in(&view, window, |shell, _, ask: &LinkPane, window, cx| {
                shell.open_link_task(ask, window, cx);
            }),
            // Its tabs read, or another tab shown.
            cx.observe_in(&view, window, |shell, _, window, cx| {
                shell.withdraw_seen(window, cx);
            }),
        ];
        self.workspace = Some(view);
        cx.notify();
    }

    /// Opens the Link to task dialog on the pane `ask` names, in the workspace shown,
    /// offering that workspace's tasks.
    fn open_link_task(&mut self, ask: &LinkPane, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(sidebar), Some(dialog), Some(view)) =
            (&self.sidebar, &self.link_task, &self.workspace)
        else {
            return;
        };
        let workspace = view.read(cx).workspace();
        let sidebar = sidebar.read(cx);
        let Some(row) = sidebar.overview().workspace(workspace) else {
            return;
        };
        let target = link_task::Target {
            pane: ask.pane,
            agent: ask.agent,
            tab: ask.tab.clone(),
            workspace,
            workspace_name: row.name.clone(),
        };
        let snapshot = sidebar.snapshot().clone();
        let task = ask.task;
        dialog.update(cx, |dialog, cx| {
            dialog.open(target, task, snapshot, window, cx);
        });
    }

    /// Opens the new agent dialog on the workspace shown.
    fn new_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Selection::Workspace(selected)) = self.selection {
            self.open_new_agent(selected, None, window, cx);
        }
    }

    /// Opens the new agent dialog on `workspace`, offering its root and the repos in it, with
    /// `task` picked.
    fn open_new_agent(
        &mut self,
        workspace: WorkspaceId,
        task: Option<TaskId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(sidebar), Some(dialog)) = (&self.sidebar, &self.new_agent) else {
            return;
        };
        let sidebar = sidebar.read(cx);
        let Some(row) = (sidebar.overview().workspaces.iter()).find(|w| w.id == workspace) else {
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
            dialog.open(target, folders, snapshot, window, cx);
            dialog.set_task(task, cx);
        });
    }

    /// Shows the workspace an agent was launched in, selecting it first if another view
    /// shows, then the agent's tab, focusing its pane.
    fn show_launched(&mut self, launched: Launched, window: &mut Window, cx: &mut Context<Self>) {
        let Launched {
            workspace,
            tab,
            pane,
        } = launched;
        if let Some(view) = self.show_workspace(workspace, window, cx) {
            view.update(cx, |view, cx| view.show_pane(tab, pane, window, cx));
        }
    }

    /// Selects `workspace` if another view shows, and answers its view.
    fn show_workspace(
        &mut self,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<WorkspaceView>> {
        if self.selection != Some(Selection::Workspace(workspace))
            && let Some(sidebar) = &self.sidebar
        {
            let selection = Selection::Workspace(workspace);
            sidebar.update(cx, |sidebar, cx| sidebar.select(selection, cx));
            self.follow_selection(window, cx);
        }
        (self.workspace.clone()).filter(|view| view.read(cx).workspace() == workspace)
    }

    /// Whether the user can see what `target` is about in the focused window: the pane in the
    /// tab shown, the task sheet open on the task, the Tasks view.
    fn in_view(&self, target: notifications::Target, window: &Window, cx: &App) -> bool {
        if !window.is_window_active() {
            return false;
        }
        match target {
            notifications::Target::Pane { pane, .. } => {
                (self.workspace.as_ref()).is_some_and(|view| {
                    (view.read(cx).shown().iter()).any(|shown| shown.read(cx).id() == pane)
                })
            }
            notifications::Target::Task(task) => (self.task_sheet.as_ref()).is_some_and(|sheet| {
                let sheet = sheet.read(cx);
                sheet.is_open() && sheet.task() == Some(task)
            }),
            notifications::Target::Tasks => self.selection == Some(Selection::Tasks),
        }
    }

    /// Shows `alerts` as OS notifications, but for what the user can see. Each is logged by
    /// its tag, so a missed one can be told from one the daemon never reported (spec §11).
    fn notify(&mut self, alerts: &[Notification], window: &mut Window, cx: &mut Context<Self>) {
        for alert in alerts {
            if self.in_view(alert.target, window, cx) {
                tracing::debug!(tag = %alert.tag, "leaving out a desktop alert: what it is about is in view");
                continue;
            }
            tracing::info!(tag = %alert.tag, title = %alert.title, "showing a desktop alert");
            self.alerts.insert(alert.tag.clone(), alert.target);
            self.notifier.show(alert.notification());
        }
    }

    /// Withdraws each notification still open whose target the user can now see.
    fn withdraw_seen(&mut self, window: &Window, cx: &App) {
        let seen: Vec<SharedString> = (self.alerts.iter())
            .filter(|(_, target)| self.in_view(**target, window, cx))
            .map(|(tag, _)| tag.clone())
            .collect();
        for tag in seen {
            tracing::debug!(%tag, "withdrawing a desktop alert: what it is about is in view");
            self.alerts.remove(&tag);
            self.notifier.withdraw(&tag);
        }
    }

    /// Shows what the notification tagged `tag` is about.
    fn open_alert(&mut self, tag: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.alerts.get(tag).copied() else {
            return;
        };
        match target {
            notifications::Target::Pane { workspace, pane } => {
                if let Some(view) = self.show_workspace(workspace, window, cx) {
                    view.update(cx, |view, cx| view.reveal_pane(pane, window, cx));
                }
            }
            notifications::Target::Task(task) => self.open_task(OpenTask(task), window, cx),
            notifications::Target::Tasks => {
                if let Some(sidebar) = &self.sidebar {
                    sidebar.update(cx, |sidebar, cx| sidebar.select(Selection::Tasks, cx));
                }
            }
        }
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

    /// Opens the task sheet editing the task a row's Edit task asks for.
    fn edit_task(&mut self, ask: EditTask, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(sidebar), Some(sheet)) = (&self.sidebar, &self.task_sheet) else {
            return;
        };
        let snapshot = sidebar.read(cx).snapshot().clone();
        let EditTask(task) = ask;
        sheet.update(cx, |sheet, cx| {
            sheet.open_editing(task, snapshot, window, cx)
        });
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
            Status::Connecting | Status::Restarting | Status::Failed(_) => None,
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

    /// Mutes the selected workspace's notifications, or unmutes them; a failure toasts why.
    fn set_muted(&mut self, muted: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (Some((workspace, name, _)), Some(api)) = (self.selected_workspace(cx), self.api(cx))
        else {
            return;
        };
        let setting = api.set_muted(workspace, muted, cx);
        cx.spawn_in(window, async move |this, cx| {
            let set = setting.await;
            let _ = this.update_in(cx, |this, window, cx| match set {
                Ok(()) => this.refresh(cx),
                Err(err) => {
                    let verb = if muted { "mute" } else { "unmute" };
                    failed(format!("Couldn't {verb} {name}"), &err, window, cx);
                }
            });
        })
        .detach();
    }

    /// Opens the Edit workspace dialog on the selected workspace, archived or not.
    fn edit_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some((id, name, _)), Some(dialog)) =
            (self.selected_workspace(cx), self.edit_workspace.clone())
        else {
            return;
        };
        let target = EditTarget { id, name };
        dialog.update(cx, |dialog, cx| dialog.open(target, window, cx));
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

    /// The banner over the window while the daemon predates the app (spec §11): Penpot has
    /// no design for it, so it is the Inbox's gh banner (a default `Alert` with the `Info`
    /// icon) with the failure alert's outline sm action. Restart is the user's choice, since
    /// every running agent is re-spawned.
    fn older_daemon_banner(&self) -> impl IntoElement {
        let connection = self.connection.clone();
        let restart = Button::new("restart-daemon")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Sm)
            .label("Restart")
            .on_click(move |_, _, cx| {
                connection.update(cx, |connection, cx| connection.restart(cx));
            });
        div()
            .flex_none()
            .debug_selector(|| "older daemon banner".into())
            .child(
                div().px(space::S6).py(space::S4).child(
                    Alert::new()
                        .icon(IconName::Info)
                        .title(AlertTitle::new(
                            "agentdeck's background service is older than this app",
                        ))
                        .description(AlertDescription::new(
                            "Restart it to use new features. Your agents resume after the restart.",
                        ))
                        .action(
                            AlertAction::new().child(
                                div()
                                    .debug_selector(|| "restart daemon".into())
                                    .child(restart),
                            ),
                        ),
                ),
            )
            .child(Separator::new())
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
            Status::Restarting => centred
                .text_sm()
                .text_color(s.muted_foreground)
                .child("Restarting agentdeck's background service…")
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
            // The older-daemon banner, if any, over Penpot "Main window" `Body`.
            Status::Connected(_) => div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .when(self.connection.read(cx).daemon_predates_app(), |content| {
                    content.child(self.older_daemon_banner())
                })
                .child(self.body())
                .into_any_element(),
        }
    }

    /// Penpot "Main window" `Body`: Sidebar | Separator | the selected view, and the dialogs.
    fn body(&self) -> impl IntoElement {
        div()
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
                            // Cached: a frame drawn for a mark turning elsewhere reuses it.
                            let style = StyleRefinement::default()
                                .flex_none()
                                .w(px(right_panel::WIDTH))
                                .h_full();
                            body.child(Separator::new().orientation(SeparatorOrientation::Vertical))
                                .child(panel.cached(style))
                        },
                    )
                }
                Some(Selection::Tasks) if self.tasks.is_some() => body.children(self.tasks.clone()),
                Some(Selection::Inbox) if self.inbox.is_some() => body.children(self.inbox.clone()),
                Some(Selection::Settings) if self.settings.is_some() => {
                    body.children(self.settings.clone())
                }
                _ => body.child(div().flex_1()),
            })
            .children(self.new_workspace.clone())
            .children(self.new_agent.clone())
            .children(self.edit_workspace.clone())
            .children(self.delete_workspace.clone())
            .children(self.new_task.clone())
            .children(self.task_sheet.clone())
            .children(self.link_task.clone())
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
            .on_action(cx.listener(|this, _: &Close, window, cx| {
                if !this.ask_to_quit(window, cx) {
                    window.remove_window();
                }
            }))
            .on_action(cx.listener(|this, _: &NewAgent, window, cx| this.new_agent(window, cx)))
            .on_action(cx.listener(|this, _: &ArchiveWorkspace, window, cx| {
                this.archive(window, cx);
            }))
            .on_action(cx.listener(|this, _: &MuteWorkspace, window, cx| {
                this.set_muted(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &UnmuteWorkspace, window, cx| {
                this.set_muted(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &EditWorkspace, window, cx| {
                this.edit_workspace(window, cx);
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
            .child(self.quit_confirmation(cx))
            .child(activity::listener())
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::Arc;

    use agentdeck_core::DaemonInfo;
    use brrritscold_core_gpui::{Root, Theme, ThemePreference};
    use gpui::{App, Task, TestAppContext, VisualTestContext};

    use agentdeck_core::WorkspaceId;

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::connection::fake::FakeConnector;
    use crate::connection::{Connector, Link};
    use crate::notifications::fake::FakeNotifier;
    use crate::overview::Snapshot;
    use crate::overview::fixtures::{id, tab, workspace};

    /// A connect that never resolves, so the shell stays connecting.
    struct Pending;

    impl Connector for Pending {
        fn connect(&self, cx: &App) -> Task<anyhow::Result<Link>> {
            cx.background_spawn(std::future::pending())
        }

        fn restart(&self, _: &DaemonInfo, cx: &App) -> Task<anyhow::Result<Link>> {
            cx.background_spawn(std::future::pending())
        }
    }

    /// Opens a `Root` window on a [`Shell`] over a connection through `connector`, and draws
    /// it in light, then dark.
    fn draw_in_both_themes(connector: Rc<dyn Connector>, cx: &mut TestAppContext) {
        let connection = cx.update(|cx| cx.new(|cx| Connection::new(connector, cx)));
        draw_connection_in_both_themes(connection, cx);
    }

    /// Opens a `Root` window on a [`Shell`] over `connection`, and draws it in light, then
    /// dark.
    fn draw_connection_in_both_themes(connection: Entity<Connection>, cx: &mut TestAppContext) {
        cx.update(brrritscold_core_gpui::init);
        cx.update(crate::terminal::init);
        let window = cx.add_window(|window, cx| {
            let shell =
                cx.new(|cx| Shell::new(connection, Rc::new(FakeNotifier::default()), window, cx));
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
        fn should_draw_the_older_daemon_banner_in_both_themes(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let _remote = connector.then_connect_older();

            draw_in_both_themes(connector, cx);
        }

        #[gpui::test]
        fn should_draw_while_restarting_in_both_themes(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let _remote = connector.then_connect_older();
            let connection = cx.update(|cx| {
                let connector: Rc<dyn Connector> = connector.clone();
                cx.new(|cx| Connection::new(connector, cx))
            });
            cx.run_until_parked();
            let _new = connector.then_connect();
            connection.update(cx, |connection, cx| connection.restart(cx));

            draw_connection_in_both_themes(connection, cx);
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
                let view = cx
                    .new(|cx| Shell::new(connection, Rc::new(FakeNotifier::default()), window, cx));
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

        mod settings {
            use super::*;
            use crate::api::fake::default_settings;
            use agentdeck_client::types::{AppearanceSettings, Settings, ThemePreference};

            #[gpui::test]
            fn should_show_settings_when_the_sidebar_selects_it(cx: &mut TestAppContext) {
                let connector = Rc::new(FakeConnector::default());
                let _remote = connector.then_connect_to(workspace_with_a_pane());
                let shell = open(connector, cx);
                let sidebar = shell.read_with(cx, |shell, _| shell.sidebar.clone().unwrap());

                sidebar.update(cx, |sidebar, cx| sidebar.select(Selection::Settings, cx));
                cx.run_until_parked();

                shell.read_with(cx, |shell, _| {
                    assert_eq!(shell.selection, Some(Selection::Settings));
                    assert!(shell.settings.is_some());
                });
                assert_eq!(shown_workspace(&shell, cx), None);
            }

            #[gpui::test]
            fn should_show_settings_when_the_inbox_asks(cx: &mut TestAppContext) {
                let connector = Rc::new(FakeConnector::default());
                let _remote = connector.then_connect_to(workspace_with_a_pane());
                let shell = open(connector, cx);
                let inbox = shell.read_with(cx, |shell, _| shell.inbox.clone().unwrap());

                inbox.update(cx, |_, cx| cx.emit(OpenSettings));
                cx.run_until_parked();

                shell.read_with(cx, |shell, _| {
                    assert_eq!(shell.selection, Some(Selection::Settings));
                });
            }

            #[gpui::test]
            fn should_apply_the_saved_theme_once_connected(cx: &mut TestAppContext) {
                let connector = Rc::new(FakeConnector::default());
                let api = workspace_with_a_pane();
                api.set_settings(Settings {
                    appearance: AppearanceSettings {
                        theme: ThemePreference::Dark,
                    },
                    ..default_settings()
                });
                let _remote = connector.then_connect_to(api);

                open(connector, cx);

                assert_eq!(
                    cx.read(|cx| cx.theme().preference()),
                    brrritscold_core_gpui::ThemePreference::Dark
                );
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

            #[gpui::test]
            fn should_open_new_agent_on_the_task_when_the_dialog_asks(cx: &mut TestAppContext) {
                let connector = Rc::new(FakeConnector::default());
                let _remote = connector.then_connect_to(workspace_with_a_pane());
                let shell = open(connector, cx);

                dialog(&shell, cx).update(cx, |_, cx| {
                    cx.emit(crate::task_sheet::StartAgentOn {
                        workspace: id(1),
                        task: id(3),
                    })
                });
                cx.run_until_parked();

                let new_agent = shell.read_with(cx, |shell, _| shell.new_agent.clone().unwrap());
                assert!(new_agent.read_with(cx, |d, _| d.is_open()));
                assert_eq!(new_agent.read_with(cx, |d, _| d.workspace()), Some(id(1)));
                assert_eq!(new_agent.read_with(cx, |d, _| d.task()), Some(id(3)));
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

            /// Whether the sheet is open on task 2, editing its title, after `ask`.
            fn editing_after(
                shell: &Entity<Shell>,
                ask: impl FnOnce(&mut TestAppContext),
                cx: &mut TestAppContext,
            ) -> bool {
                let sheet = sheet(shell, cx);
                sheet.update(cx, |sheet, cx| sheet.cancel_edit(cx));
                ask(cx);
                cx.run_until_parked();
                sheet.read_with(cx, |s, _| {
                    s.is_open()
                        && s.task() == Some(id(2))
                        && s.editing() == Some(crate::task_sheet::Editing::Title)
                })
            }

            #[gpui::test]
            fn should_open_the_sheet_editing_the_task_a_rows_edit_task_asks_for(
                cx: &mut TestAppContext,
            ) {
                let connector = Rc::new(FakeConnector::default());
                let _remote = connector.then_connect_to(daemon());
                let shell = open(connector, cx);
                let (tasks, inbox, panel) = shell.read_with(cx, |shell, _| {
                    (
                        shell.tasks.clone().unwrap(),
                        shell.inbox.clone().unwrap(),
                        shell.panel.clone().unwrap(),
                    )
                });

                assert!(editing_after(
                    &shell,
                    |cx| tasks.update(cx, |_, cx| cx.emit(EditTask(id(2)))),
                    cx
                ));
                assert!(editing_after(
                    &shell,
                    |cx| inbox.update(cx, |_, cx| cx.emit(EditTask(id(2)))),
                    cx
                ));
                assert!(editing_after(
                    &shell,
                    |cx| panel.update(cx, |_, cx| cx.emit(EditTask(id(2)))),
                    cx
                ));
            }

            mod start_agent {
                use agentdeck_client::ws::Remote;

                use super::*;
                use crate::task_sheet::StartAgentOn;

                /// Workspace 1 shown, and workspace 2 `other`, holding task 3.
                fn daemon() -> Arc<FakeApi> {
                    let api = workspace_with_a_pane();
                    let mut snapshot = api.snapshot_now();
                    snapshot.workspaces.push(workspace(2, "other"));
                    snapshot.tasks = vec![crate::overview::fixtures::task(
                        3,
                        agentdeck_core::TaskKind::Plain,
                        Some(id(2)),
                    )];
                    api.set(snapshot);
                    api
                }

                /// The shell, once the sheet's Start agent asked for task 3 in workspace 2,
                /// and the daemon's end, which keeps it connected.
                fn asked(
                    cx: &mut TestAppContext,
                ) -> (Entity<Shell>, Entity<NewAgentDialog>, Remote) {
                    let connector = Rc::new(FakeConnector::default());
                    let remote = connector.then_connect_to(daemon());
                    let shell = open(connector, cx);
                    let sheet = sheet(&shell, cx);
                    sheet.update(cx, |_, cx| {
                        cx.emit(StartAgentOn {
                            workspace: id(2),
                            task: id(3),
                        })
                    });
                    cx.run_until_parked();
                    let dialog = shell.read_with(cx, |shell, _| shell.new_agent.clone().unwrap());
                    (shell, dialog, remote)
                }

                #[gpui::test]
                fn should_open_new_agent_on_the_tasks_workspace_with_the_task_picked(
                    cx: &mut TestAppContext,
                ) {
                    let (shell, dialog, _remote) = asked(cx);

                    assert!(dialog.read_with(cx, |d, _| d.is_open()));
                    assert_eq!(dialog.read_with(cx, |d, _| d.workspace()), Some(id(2)));
                    assert_eq!(dialog.read_with(cx, |d, _| d.task()), Some(id(3)));
                    // The window stays where it was until the agent starts.
                    assert_eq!(shown_workspace(&shell, cx), Some(id(1)));
                }

                #[gpui::test]
                fn should_show_the_tasks_workspace_once_the_agent_starts(cx: &mut TestAppContext) {
                    let (shell, dialog, _remote) = asked(cx);
                    let window = cx.windows()[0];

                    window
                        .update(cx, |_, window, cx| {
                            dialog.update(cx, |d, cx| d.start(window, cx));
                        })
                        .unwrap();
                    cx.run_until_parked();

                    assert_eq!(shown_workspace(&shell, cx), Some(id(2)));
                    assert_eq!(
                        shell.read_with(cx, |shell, _| shell.selection),
                        Some(Selection::Workspace(id(2)))
                    );
                }
            }
        }

        mod link_task {
            use agentdeck_core::Agent;

            use super::*;

            #[gpui::test]
            fn should_open_the_link_dialog_on_the_pane_the_workspace_asks_for(
                cx: &mut TestAppContext,
            ) {
                let connector = Rc::new(FakeConnector::default());
                let _remote = connector.then_connect_to(workspace_with_a_pane());
                let shell = open(connector, cx);
                let view = shell.read_with(cx, |shell, _| shell.workspace.clone().unwrap());

                view.update(cx, |_, cx| {
                    cx.emit(LinkPane {
                        pane: id(7),
                        agent: Agent::Claude,
                        task: Some(id(30)),
                        tab: "tab 1".into(),
                    })
                });
                cx.run_until_parked();

                let dialog = shell.read_with(cx, |shell, _| shell.link_task.clone().unwrap());
                assert!(dialog.read_with(cx, |d, _| d.is_open()));
                assert_eq!(dialog.read_with(cx, |d, _| d.pane()), Some(id(7)));
                assert_eq!(dialog.read_with(cx, |d, _| d.task()), Some(id(30)));
                assert_eq!(
                    dialog.read_with(cx, |d, _| d.workspace_name()),
                    Some("agentdeck".into())
                );
            }

            #[gpui::test]
            fn should_follow_the_tasks_the_sidebar_reads_while_open(cx: &mut TestAppContext) {
                use agentdeck_core::{TaskKind, TaskStatus};

                use crate::overview::fixtures::task;

                let connector = Rc::new(FakeConnector::default());
                let api = workspace_with_a_pane();
                let mut snapshot = api.snapshot_now();
                snapshot.tasks = vec![task(10, TaskKind::Plain, Some(id(1)))];
                api.set(snapshot.clone());
                let _remote = connector.then_connect_to(api.clone());
                let shell = open(connector, cx);
                let view = shell.read_with(cx, |shell, _| shell.workspace.clone().unwrap());
                view.update(cx, |_, cx| {
                    cx.emit(LinkPane {
                        pane: id(7),
                        agent: Agent::Claude,
                        task: None,
                        tab: "tab 1".into(),
                    })
                });
                cx.run_until_parked();

                snapshot.tasks[0].status = TaskStatus::Done;
                snapshot.tasks.push(task(13, TaskKind::Plain, Some(id(1))));
                api.set(snapshot);
                let sidebar = shell.read_with(cx, |shell, _| shell.sidebar.clone().unwrap());
                sidebar.update(cx, |sidebar, cx| sidebar.refresh(cx));
                cx.run_until_parked();

                let dialog = shell.read_with(cx, |shell, _| shell.link_task.clone().unwrap());
                assert_eq!(dialog.read_with(cx, |d, _| d.offered()), [id(13)]);
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

    mod older_daemon {
        use gpui::Modifiers;

        use super::*;

        /// A `Root` window on a [`Shell`] over `connector`, drawn.
        fn open(connector: Rc<FakeConnector>, cx: &mut TestAppContext) -> &mut VisualTestContext {
            cx.update(brrritscold_core_gpui::init);
            cx.update(crate::terminal::init);
            let window = cx.add_window(|window, cx| {
                let connection = cx.new(|cx| Connection::new(connector, cx));
                let shell = cx
                    .new(|cx| Shell::new(connection, Rc::new(FakeNotifier::default()), window, cx));
                Root::new(shell, window, cx)
            });
            let cx = VisualTestContext::from_window(*window, cx).into_mut();
            settle(cx);
            cx
        }

        fn settle(cx: &mut VisualTestContext) {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        #[gpui::test]
        fn should_show_the_banner_while_the_daemon_predates_the_app(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let _remote = connector.then_connect_older();

            let cx = open(connector, cx);

            assert!(cx.debug_bounds("older daemon banner").is_some());
        }

        #[gpui::test]
        fn should_not_show_it_for_a_daemon_as_new_as_the_app(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let _remote = connector.then_connect();

            let cx = open(connector, cx);

            assert!(cx.debug_bounds("older daemon banner").is_none());
        }

        #[gpui::test]
        fn should_restart_the_daemon_when_restart_is_clicked(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let _old = connector.then_connect_older();
            let _new = connector.then_connect();
            let cx = open(connector.clone(), cx);

            let restart = cx
                .debug_bounds("restart daemon")
                .expect("the Restart button");
            cx.simulate_click(restart.center(), Modifiers::default());
            settle(cx);

            assert_eq!(connector.restarts.borrow().len(), 1);
            assert!(cx.debug_bounds("older daemon banner").is_none());
        }
    }

    /// A shell over a fake daemon, driven through its window, for the workspace menu's
    /// tests.
    mod support {
        use agentdeck_client::types::Workspace;
        use gpui::Modifiers;

        use super::*;
        use crate::notice::{self, Notice};
        use crate::top_bar::ArchiveWorkspace;

        pub struct Harness {
            pub shell: Entity<Shell>,
            pub api: Arc<FakeApi>,
            pub _remote: agentdeck_client::ws::Remote,
        }

        impl Harness {
            /// A shell showing `api`'s workspaces, the first selected.
            pub fn open(
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
                    let view = cx.new(|cx| {
                        Shell::new(connection, Rc::new(FakeNotifier::default()), window, cx)
                    });
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

            pub fn settle(&self, cx: &mut VisualTestContext) {
                cx.run_until_parked();
                cx.update(|window, cx| window.draw(cx).clear(cx));
                cx.run_until_parked();
                cx.update(|window, cx| window.draw(cx).clear(cx));
            }

            pub fn click(&self, at: gpui::Point<gpui::Pixels>, cx: &mut VisualTestContext) {
                cx.simulate_click(at, Modifiers::default());
                self.settle(cx);
            }

            pub fn select(&self, workspace: u32, cx: &mut VisualTestContext) {
                let sidebar = self.shell.read_with(cx, |shell, _| shell.sidebar.clone());
                sidebar.unwrap().update(cx, |sidebar, cx| {
                    sidebar.select(Selection::Workspace(id(workspace)), cx);
                });
                self.settle(cx);
            }

            pub fn archive(&self, cx: &mut VisualTestContext) {
                cx.dispatch_action(ArchiveWorkspace);
                self.settle(cx);
            }

            /// Whether the sidebar has workspace `n` as archived.
            pub fn is_archived(&self, n: u32, cx: &mut VisualTestContext) -> bool {
                self.shell.read_with(cx, |shell, cx| {
                    let sidebar = shell.sidebar.as_ref().unwrap().read(cx);
                    sidebar.overview().workspace(id(n)).unwrap().archived
                })
            }

            /// The top bar of the workspace shown says it's archived.
            pub fn shows_archived(&self, cx: &mut VisualTestContext) -> bool {
                self.shell.read_with(cx, |shell, cx| {
                    let view = shell
                        .workspace
                        .as_ref()
                        .expect("a workspace shows")
                        .read(cx);
                    view.top_bar().is_archived()
                })
            }

            pub fn last_notice(&self, cx: &mut VisualTestContext) -> Notice {
                cx.update(|_, cx| notice::shown(cx).last().cloned().expect("a toast"))
            }
        }

        /// Opens the top bar's menu and clicks its item at index `n`, counting from 0.
        pub fn click_menu_item(harness: &Harness, n: u32, cx: &mut VisualTestContext) {
            let trigger = cx.debug_bounds("workspace menu").expect("the menu shows");
            harness.click(trigger.center(), cx);
            let menu = cx.debug_bounds("dropdown-menu-content").expect("it opens");
            // The menu's 4px padding, `n` 32px rows, then half the next.
            let y = 4. + 32. * n as f32 + 16.;
            harness.click(gpui::point(menu.center().x, menu.top() + gpui::px(y)), cx);
        }

        /// Workspace 1, `agentdeck`, live; workspace 2, `old`, archived.
        pub fn daemon() -> Arc<FakeApi> {
            let old = Workspace {
                archived_at: Some("2026-11-13T00:00:00Z".into()),
                ..workspace(2, "old")
            };
            Arc::new(FakeApi::with(Snapshot {
                workspaces: vec![workspace(1, "agentdeck"), old],
                ..Snapshot::default()
            }))
        }
    }

    mod mute {
        use super::support::{Harness, daemon};
        use super::*;
        use crate::top_bar::MuteWorkspace;

        /// Opens the top bar's menu and clicks its second item, after Edit workspace.
        fn click_mute_item(harness: &Harness, cx: &mut VisualTestContext) {
            support::click_menu_item(harness, 1, cx);
        }

        /// Whether the sidebar has workspace `n` as muted, and the top bar shown agrees.
        fn is_muted(harness: &Harness, n: u32, cx: &mut VisualTestContext) -> bool {
            harness.shell.read_with(cx, |shell, cx| {
                let sidebar = shell.sidebar.as_ref().unwrap().read(cx);
                let muted = sidebar.overview().workspace(id(n)).unwrap().muted;
                let view = shell
                    .workspace
                    .as_ref()
                    .expect("a workspace shows")
                    .read(cx);
                assert_eq!(view.top_bar().is_muted(), muted, "the top bar follows");
                muted
            })
        }

        #[gpui::test]
        fn should_mute_the_selected_workspace_from_the_menus_second_item(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(), cx);

            click_mute_item(&harness, cx);

            assert_eq!(*harness.api.muted.lock().unwrap(), [(id(1), true)]);
            assert!(is_muted(&harness, 1, cx));
        }

        #[gpui::test]
        fn should_unmute_a_muted_workspace_from_the_same_item(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(), cx);
            click_mute_item(&harness, cx);

            click_mute_item(&harness, cx);

            assert_eq!(
                *harness.api.muted.lock().unwrap(),
                [(id(1), true), (id(1), false)]
            );
            assert!(!is_muted(&harness, 1, cx));
        }

        #[gpui::test]
        fn should_toast_a_failed_mute(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(), cx);
            harness.api.fail_next("connection refused");

            cx.dispatch_action(MuteWorkspace);
            harness.settle(cx);

            assert_eq!(harness.last_notice(cx).title, "Couldn't mute agentdeck");
            assert!(!is_muted(&harness, 1, cx));
        }
    }

    mod edit {
        use super::support::{Harness, click_menu_item, daemon};
        use super::*;
        use crate::edit_workspace::EditWorkspaceDialog;
        use crate::top_bar::EditWorkspace;

        fn dialog(harness: &Harness, cx: &mut VisualTestContext) -> Entity<EditWorkspaceDialog> {
            harness
                .shell
                .read_with(cx, |shell, _| shell.edit_workspace.clone().unwrap())
        }

        fn is_open(harness: &Harness, cx: &mut VisualTestContext) -> bool {
            dialog(harness, cx).read_with(cx, |d, _| d.is_open())
        }

        /// What the name field holds.
        fn name_field(harness: &Harness, cx: &mut VisualTestContext) -> String {
            let dialog = dialog(harness, cx);
            cx.update(|_, cx| dialog.read(cx).name(cx))
        }

        /// The sidebar's name for workspace `n`, and the top bar's, which must agree.
        fn shown_name(harness: &Harness, n: u32, cx: &mut VisualTestContext) -> SharedString {
            harness.shell.read_with(cx, |shell, cx| {
                let sidebar = shell.sidebar.as_ref().unwrap().read(cx);
                let name = sidebar.overview().workspace(id(n)).unwrap().name.clone();
                let view = shell
                    .workspace
                    .as_ref()
                    .expect("a workspace shows")
                    .read(cx);
                assert_eq!(view.top_bar().name(), &name, "the top bar follows");
                name
            })
        }

        #[gpui::test]
        fn should_open_on_the_selected_workspace_from_the_menus_first_item(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(daemon(), cx);

            click_menu_item(&harness, 0, cx);

            assert!(is_open(&harness, cx));
            assert_eq!(name_field(&harness, cx), "agentdeck");
            assert_eq!(*harness.api.muted.lock().unwrap(), []);
        }

        #[gpui::test]
        fn should_open_from_an_archived_workspaces_menu(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(), cx);
            harness.select(2, cx);

            click_menu_item(&harness, 0, cx);

            assert!(is_open(&harness, cx));
            assert_eq!(name_field(&harness, cx), "old");
        }

        #[gpui::test]
        fn should_show_the_new_name_once_renamed(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(), cx);
            cx.dispatch_action(EditWorkspace);
            harness.settle(cx);

            cx.simulate_input("deck");
            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(
                *harness.api.renamed.lock().unwrap(),
                [(id(1), "deck".into())]
            );
            assert!(!is_open(&harness, cx));
            assert_eq!(shown_name(&harness, 1, cx), "deck");
        }
    }

    mod archive {
        use super::support::{Harness, daemon};
        use super::*;

        #[gpui::test]
        fn should_archive_the_selected_workspace_from_the_top_bar_menu(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(), cx);

            // The third item, after Edit workspace and Mute notifications.
            support::click_menu_item(&harness, 2, cx);

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

                pick_from_menu(&harness, 4, cx);

                assert!(is_open(&harness, cx));
                assert_eq!(*harness.api.archived.lock().unwrap(), []);
            }

            #[gpui::test]
            fn should_open_from_an_archived_workspaces_menu(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(daemon(), cx);
                harness.select(2, cx);

                pick_from_menu(&harness, 2, cx);

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

    mod notifications {
        use agentdeck_client::types::{Pane, PullRequest};
        use agentdeck_client::ws::{Event, Remote};
        use agentdeck_core::ws::{Entity as Changed, ServerMessage};
        use agentdeck_core::{Agent, AgentState, ChecksState, PaneId, PaneKind, PrState, TaskKind};

        use super::*;
        use crate::overview::PaneState;
        use crate::overview::fixtures::{pane, tab, task};
        use crate::pane::fixtures::shell_pane;

        /// A shell over a fake daemon, in a window of its own.
        struct Open {
            shell: Entity<Shell>,
            api: Arc<FakeApi>,
            notifier: Rc<FakeNotifier>,
            remote: Remote,
            cx: &'static mut VisualTestContext,
        }

        impl Open {
            fn new(api: Arc<FakeApi>, cx: &mut TestAppContext) -> Open {
                cx.update(brrritscold_core_gpui::init);
                cx.update(crate::terminal::init);
                let notifier = Rc::new(FakeNotifier::default());
                let connector = Rc::new(FakeConnector::default());
                let remote = connector.then_connect_to(api.clone());
                let mut shell = None;
                let window = cx.add_window(|window, cx| {
                    let connection = cx.new(|cx| Connection::new(connector, cx));
                    let view = cx.new(|cx| Shell::new(connection, notifier.clone(), window, cx));
                    shell = Some(view.clone());
                    Root::new(view, window, cx)
                });
                let mut open = Open {
                    shell: shell.unwrap(),
                    api,
                    notifier,
                    remote,
                    cx: VisualTestContext::from_window(*window, cx).into_mut(),
                };
                open.cx.update(|window, _| window.activate_window());
                open.settle();
                open
            }

            fn settle(&mut self) {
                self.cx.run_until_parked();
                self.cx.update(|window, cx| window.draw(cx).clear(cx));
                self.cx.run_until_parked();
            }

            fn send(&mut self, message: ServerMessage) {
                self.remote
                    .events
                    .try_send(Event::Message(message))
                    .unwrap();
                self.settle();
            }

            fn turn(&mut self, pane: u32, state: AgentState) {
                self.send(ServerMessage::State {
                    pane: id(pane),
                    state,
                });
            }

            fn shown(&self) -> Vec<agentdeck_notify::Notification> {
                self.notifier.shown()
            }

            fn click(&mut self, tag: &str) {
                self.cx.cx.update(|cx| self.notifier.click(tag, cx));
                self.settle();
            }

            fn is_active(&mut self) -> bool {
                self.cx.update(|window, _| window.is_window_active())
            }

            /// The workspace shown and its tab shown.
            fn showing(&mut self) -> Option<(WorkspaceId, Option<agentdeck_core::TabId>)> {
                self.shell.read_with(self.cx, |shell, cx| {
                    (shell.workspace.as_ref()).map(|view| {
                        let view = view.read(cx);
                        (view.workspace(), view.selected())
                    })
                })
            }
        }

        /// Claude Code's pane `n` in tab `tab`, as the workspace view reads it.
        fn claude(n: u32, tab: u32) -> Pane {
            Pane {
                agent: Some(Agent::Claude),
                kind: PaneKind::Agent,
                ..shell_pane(n, id(tab))
            }
        }

        /// `web` (id 1), shown, with Claude Code in pane 7 in its first tab (1) and pane 9 in
        /// its second (3); `api` (id 2) with Claude Code in pane 8, in tab 2.
        fn daemon() -> Arc<FakeApi> {
            let agent = |n, workspace, tab: &str| PaneState {
                agent: Some(Agent::Claude),
                tab: tab.into(),
                ..pane(n, id(workspace), AgentState::Working)
            };
            let api = Arc::new(FakeApi::with(Snapshot {
                workspaces: vec![workspace(1, "web"), workspace(2, "api")],
                panes: vec![
                    agent(7, 1, "first"),
                    agent(9, 1, "second"),
                    agent(8, 2, "agents"),
                ],
                ..Snapshot::default()
            }));
            api.set_tabs(
                id(1),
                vec![tab(1, id(1), 0, Some(id(7))), tab(3, id(1), 1, Some(id(9)))],
            );
            api.set_tabs(id(2), vec![tab(2, id(2), 0, Some(id(8)))]);
            api.set_panes(vec![claude(7, 1), claude(9, 3), claude(8, 2)]);
            api
        }

        mod panes {
            use super::*;

            #[gpui::test]
            fn should_notify_when_a_pane_in_another_workspace_turns_blocked(
                cx: &mut TestAppContext,
            ) {
                let mut open = Open::new(daemon(), cx);

                open.turn(8, AgentState::Blocked);

                let shown = open.shown();
                assert_eq!(shown.len(), 1, "{shown:?}");
                assert_eq!(shown[0].title, "Claude Code is blocked");
                assert_eq!(shown[0].body, "agents · api");
            }

            #[gpui::test]
            fn should_notify_when_a_pane_in_a_tab_not_shown_is_done(cx: &mut TestAppContext) {
                let mut open = Open::new(daemon(), cx);

                open.turn(9, AgentState::Done);

                let titles: Vec<_> = open.shown().into_iter().map(|n| n.title).collect();
                assert_eq!(titles, ["Claude Code is done"]);
            }

            #[gpui::test]
            fn should_not_notify_for_a_pane_in_view(cx: &mut TestAppContext) {
                let mut open = Open::new(daemon(), cx);
                assert!(open.is_active());

                open.turn(7, AgentState::Blocked);

                assert_eq!(open.shown(), []);
            }

            #[gpui::test]
            fn should_notify_for_a_pane_in_view_while_the_window_is_inactive(
                cx: &mut TestAppContext,
            ) {
                let mut open = Open::new(daemon(), cx);
                open.cx.deactivate_window();

                open.turn(7, AgentState::Blocked);

                assert_eq!(open.shown().len(), 1);
            }

            #[gpui::test]
            fn should_notify_once_per_change(cx: &mut TestAppContext) {
                let mut open = Open::new(daemon(), cx);

                open.turn(8, AgentState::Blocked);
                open.turn(8, AgentState::Blocked);
                open.turn(8, AgentState::Working);
                open.turn(8, AgentState::Blocked);

                assert_eq!(open.shown().len(), 2);
            }

            #[gpui::test]
            fn should_log_each_alert_it_shows_by_tag_and_one_it_leaves_out(
                cx: &mut TestAppContext,
            ) {
                let root = tempfile::tempdir().unwrap();
                let path = root.path().join("agentdeck-gui.log");
                let (subscriber, _) = crate::logging::subscriber(Some(&path), Some("debug"));
                let _logging = tracing::subscriber::set_default(subscriber);
                let mut open = Open::new(daemon(), cx);

                open.turn(8, AgentState::Blocked);
                open.turn(7, AgentState::Blocked);

                let log = std::fs::read_to_string(&path).unwrap();
                let line = |pane: u32| {
                    let tag = format!("tag=pane:{}", id::<PaneId>(pane));
                    log.lines()
                        .find(|line| line.contains(&tag))
                        .map(str::to_owned)
                };
                let shown = line(8).expect("a line for pane 8");
                assert!(shown.contains("INFO"), "{shown}");
                assert!(shown.contains("showing a desktop alert"), "{shown}");
                let left_out = line(7).expect("a line for pane 7");
                assert!(left_out.contains("in view"), "{left_out}");
            }

            #[gpui::test]
            fn should_not_notify_for_a_muted_workspace(cx: &mut TestAppContext) {
                let api = daemon();
                let mut snapshot = api.snapshot_now();
                snapshot.workspaces[1].muted = true;
                api.set(snapshot);
                let mut open = Open::new(api, cx);

                open.turn(8, AgentState::Blocked);

                assert_eq!(open.shown(), []);
            }
        }

        mod withdraw {
            use super::*;

            fn tag(pane: u32) -> SharedString {
                format!("pane:{}", id::<PaneId>(pane)).into()
            }

            #[gpui::test]
            fn should_keep_a_panes_notification_while_it_is_out_of_view(cx: &mut TestAppContext) {
                let mut open = Open::new(daemon(), cx);

                open.turn(8, AgentState::Blocked);

                assert_eq!(open.notifier.open(), [tag(8)]);
            }

            #[gpui::test]
            fn should_withdraw_a_panes_notification_once_its_workspace_shows(
                cx: &mut TestAppContext,
            ) {
                let mut open = Open::new(daemon(), cx);
                open.turn(8, AgentState::Blocked);

                let shell = open.shell.clone();
                open.cx.update(|window, cx| {
                    shell.update(cx, |shell, cx| shell.show_workspace(id(2), window, cx));
                });
                open.settle();

                assert_eq!(open.notifier.open(), Vec::<SharedString>::new());
            }

            #[gpui::test]
            fn should_withdraw_a_panes_notification_once_its_tab_shows(cx: &mut TestAppContext) {
                let mut open = Open::new(daemon(), cx);
                open.turn(9, AgentState::Done);
                assert_eq!(open.notifier.open(), [tag(9)]);

                let view = open
                    .shell
                    .read_with(open.cx, |shell, _| shell.workspace.clone());
                let view = view.expect("web is shown");
                open.cx.update(|window, cx| {
                    view.update(cx, |view, cx| view.select(id(3), window, cx));
                });
                open.settle();

                assert_eq!(open.notifier.open(), Vec::<SharedString>::new());
            }

            #[gpui::test]
            fn should_withdraw_a_panes_notification_once_the_window_is_focused(
                cx: &mut TestAppContext,
            ) {
                let mut open = Open::new(daemon(), cx);
                open.cx.deactivate_window();
                open.turn(7, AgentState::Blocked);
                assert_eq!(open.notifier.open(), [tag(7)]);

                open.cx.update(|window, _| window.activate_window());
                open.settle();

                assert_eq!(open.notifier.open(), Vec::<SharedString>::new());
            }

            #[gpui::test]
            fn should_withdraw_a_prs_notification_once_its_task_sheet_shows(
                cx: &mut TestAppContext,
            ) {
                let mut open = failing_checks(cx);
                assert_eq!(open.notifier.open().len(), 1);

                let shell = open.shell.clone();
                open.cx.update(|window, cx| {
                    shell.update(cx, |shell, cx| {
                        shell.open_task(OpenTask(id(20)), window, cx)
                    });
                });
                open.settle();

                assert_eq!(open.notifier.open(), Vec::<SharedString>::new());
            }

            #[gpui::test]
            fn should_not_notify_for_a_pr_whose_task_sheet_is_in_view(cx: &mut TestAppContext) {
                let api = daemon();
                let mut snapshot = api.snapshot_now();
                snapshot.tasks = vec![click::task_with_checks(20, ChecksState::Pending)];
                api.set(snapshot.clone());
                let mut open = Open::new(api, cx);
                let shell = open.shell.clone();
                open.cx.update(|window, cx| {
                    shell.update(cx, |shell, cx| {
                        shell.open_task(OpenTask(id(20)), window, cx)
                    });
                });
                open.settle();

                snapshot.tasks = vec![click::task_with_checks(20, ChecksState::Failure)];
                open.api.set(snapshot);
                open.send(ServerMessage::Changed {
                    entity: Changed::Task,
                    id: id::<agentdeck_core::TaskId>(20).to_string(),
                });

                assert_eq!(open.shown(), []);
            }
        }

        /// A shell whose PR task 20's checks just failed, notified.
        fn failing_checks(cx: &mut TestAppContext) -> Open {
            let checks = |checks| click::task_with_checks(20, checks);
            let api = daemon();
            let mut snapshot = api.snapshot_now();
            snapshot.tasks = vec![checks(ChecksState::Pending)];
            api.set(snapshot.clone());
            let mut open = Open::new(api, cx);
            snapshot.tasks = vec![checks(ChecksState::Failure)];
            open.api.set(snapshot);
            open.send(ServerMessage::Changed {
                entity: Changed::Task,
                id: id::<agentdeck_core::TaskId>(20).to_string(),
            });
            open
        }

        mod click {
            use super::*;

            #[gpui::test]
            fn should_focus_the_window_and_show_the_panes_workspace(cx: &mut TestAppContext) {
                let mut open = Open::new(daemon(), cx);
                open.turn(8, AgentState::Blocked);
                open.cx.deactivate_window();

                let tag = open.shown()[0].tag.to_string();
                open.click(&tag);

                assert!(open.is_active());
                assert_eq!(open.showing(), Some((id(2), Some(id(2)))));
            }

            #[gpui::test]
            fn should_show_the_panes_tab(cx: &mut TestAppContext) {
                let mut open = Open::new(daemon(), cx);
                open.turn(9, AgentState::Blocked);

                let tag = open.shown()[0].tag.to_string();
                open.click(&tag);

                assert_eq!(open.showing(), Some((id(1), Some(id(3)))));
            }

            #[gpui::test]
            fn should_open_the_task_sheet_for_a_pr(cx: &mut TestAppContext) {
                let mut open = failing_checks(cx);

                let shown = open.shown();
                assert_eq!(shown.len(), 1, "{shown:?}");
                assert_eq!(shown[0].title, "Checks failed");
                open.click(&shown[0].tag);

                let sheet = open.shell.read_with(open.cx, |s, _| s.task_sheet.clone());
                let sheet = sheet.expect("the sheet");
                sheet.read_with(open.cx, |sheet, _| {
                    assert!(sheet.is_open());
                    assert_eq!(sheet.task(), Some(id(20)));
                });
            }

            /// PR task `n` in `web`, `acme/web #n`, with `checks`.
            pub(super) fn task_with_checks(
                n: u32,
                checks: ChecksState,
            ) -> agentdeck_client::types::Task {
                agentdeck_client::types::Task {
                    pr: Some(PullRequest {
                        checks: Some(checks),
                        draft: false,
                        head_ref: "feat/x".into(),
                        number: i64::from(n),
                        repo: "acme/web".into(),
                        review: None,
                        state: PrState::Open,
                        synced_at: "2026-11-14T00:00:00Z".into(),
                        updated_at: "2026-11-14T00:00:00Z".into(),
                        url: format!("https://github.com/acme/web/pull/{n}"),
                    }),
                    ..task(n, TaskKind::Pr, Some(id(1)))
                }
            }
        }

        /// The working agents' marks, in the sidebar and the panes shown (spec §11).
        mod spin {
            use gpui::{Modifiers, point, px};

            use super::*;
            use crate::activity::IDLE_AFTER;
            use crate::state_dot::tests::{play_frame, redraws_in_one_second};

            /// The shell over [`daemon`], with the pane shown, 7, working too.
            fn working(cx: &mut TestAppContext) -> Open {
                let mut open = Open::new(daemon(), cx);
                open.turn(7, AgentState::Working);
                open
            }

            impl Open {
                /// How often the sidebar and each pane shown asked to redraw in a second.
                fn redraws(&mut self) -> Vec<usize> {
                    let sidebar = self.shell.read_with(self.cx, |s, _| s.sidebar.clone());
                    let panes = self.shell.read_with(self.cx, |s, cx| {
                        (s.workspace.as_ref()).map_or_else(Vec::new, |v| v.read(cx).shown())
                    });
                    let mut redraws = vec![redraws_in_one_second(&sidebar.unwrap(), self.cx)];
                    for pane in panes {
                        redraws.push(redraws_in_one_second(&pane, self.cx));
                    }
                    redraws
                }

                /// Lets 30 s pass with no input.
                fn idle(&mut self) {
                    self.cx.executor().advance_clock(IDLE_AFTER);
                    for _ in 0..12 {
                        play_frame(self.cx);
                    }
                }
            }

            #[gpui::test]
            fn should_turn_while_the_user_is_at_the_window(cx: &mut TestAppContext) {
                let mut open = working(cx);
                let redraws = open.redraws();
                assert!(
                    redraws.len() > 1 && redraws.iter().all(|n| *n > 0),
                    "{redraws:?}"
                );
            }

            #[gpui::test]
            fn should_hold_still_after_30_s_without_input(cx: &mut TestAppContext) {
                let mut open = working(cx);
                open.idle();
                let redraws = open.redraws();
                assert!(redraws.iter().all(|n| *n == 0), "{redraws:?}");
            }

            /// A turning mark redraws the view it's in, and that alone: the views around it
            /// hear of nothing.
            #[gpui::test]
            fn should_leave_the_views_without_a_mark_alone_while_the_marks_turn(
                cx: &mut TestAppContext,
            ) {
                let open = working(cx);
                let (panel, inbox, tasks, sheet) = open.shell.read_with(open.cx, |s, _| {
                    let views = (s.panel.clone(), s.inbox.clone(), s.tasks.clone());
                    (views.0, views.1, views.2, s.task_sheet.clone())
                });
                let redraws = [
                    redraws_in_one_second(&panel.expect("connected"), open.cx),
                    redraws_in_one_second(&inbox.expect("connected"), open.cx),
                    redraws_in_one_second(&tasks.expect("connected"), open.cx),
                    redraws_in_one_second(&sheet.expect("connected"), open.cx),
                ];
                assert_eq!(redraws, [0; 4], "panel, inbox, tasks, task sheet");
            }

            /// The right panel, holding no turning mark here, is drawn from its cache.
            #[gpui::test]
            fn should_not_render_the_right_panel_again_while_the_marks_turn(
                cx: &mut TestAppContext,
            ) {
                let open = working(cx);
                let panel = open.shell.read_with(open.cx, |s, _| s.panel.clone());
                let panel = panel.expect("connected");
                let rendered =
                    |open: &Open| panel.read_with(open.cx, |panel, _| panel.items_rendered.get());
                let before = rendered(&open);
                for _ in 0..60 {
                    play_frame(open.cx);
                }
                assert!(before > 0);
                assert_eq!(rendered(&open), before);
            }

            #[gpui::test]
            fn should_turn_again_when_the_pointer_moves(cx: &mut TestAppContext) {
                let mut open = working(cx);
                open.idle();
                (open.cx).simulate_mouse_move(point(px(10.), px(10.)), None, Modifiers::default());
                play_frame(open.cx);
                let redraws = open.redraws();
                assert!(redraws.iter().all(|n| *n > 0), "{redraws:?}");
            }
        }
    }

    mod quitting {
        use agentdeck_client::types::{GuiSettings, Settings};
        use agentdeck_core::{Agent, AgentState};
        use gpui::Modifiers;

        use super::*;
        use crate::api::fake::default_settings;
        use crate::overview::PaneState;
        use crate::overview::fixtures::{id, pane};
        use crate::title_bar::Close;
        use agentdeck_client::ws::Remote;

        /// An agent pane in workspace 1 whose agent runs.
        fn agent(n: u32) -> PaneState {
            PaneState {
                agent: Some(Agent::Claude),
                ..pane(n, id(1), AgentState::Working)
            }
        }

        /// A shell pane in workspace 1 hosting Codex.
        fn hosting_shell(n: u32) -> PaneState {
            PaneState {
                agent: Some(Agent::Codex),
                ..pane(n, id(1), AgentState::Idle)
            }
        }

        /// A shell pane in workspace 1 hosting no agent.
        fn plain_shell(n: u32) -> PaneState {
            pane(n, id(1), AgentState::Idle)
        }

        /// A daemon with workspace 1 holding `panes`, asking before quitting as `confirm`.
        fn daemon(panes: Vec<PaneState>, confirm: bool) -> Arc<FakeApi> {
            let api = workspace_with_a_pane();
            let mut snapshot = api.snapshot_now();
            snapshot.panes = panes;
            api.set(snapshot);
            api.set_settings(Settings {
                gui: GuiSettings {
                    confirm_quit: confirm,
                },
                ..default_settings()
            });
            api
        }

        struct Harness {
            shell: Entity<Shell>,
            api: Arc<FakeApi>,
            _remote: Remote,
        }

        impl Harness {
            /// The main window, its shortcuts bound, over `api`.
            fn open(
                api: Arc<FakeApi>,
                cx: &mut TestAppContext,
            ) -> (Harness, &mut VisualTestContext) {
                cx.update(brrritscold_core_gpui::init);
                cx.update(crate::terminal::init);
                cx.update(crate::keymap::init);
                let connector = Rc::new(FakeConnector::default());
                let remote = connector.then_connect_to(api.clone());
                let mut shell = None;
                let window = cx.add_window(|window, cx| {
                    let connection = cx.new(|cx| Connection::new(connector, cx));
                    let view = cx.new(|cx| {
                        Shell::new(connection, Rc::new(FakeNotifier::default()), window, cx)
                    });
                    shell = Some(view.clone());
                    Root::new(view, window, cx)
                });
                let cx = VisualTestContext::from_window(*window, cx).into_mut();
                cx.update(|window, _| window.activate_window());
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
            }

            /// Presses Quit's shortcut.
            fn press_quit(&self, cx: &mut VisualTestContext) {
                cx.simulate_keystrokes("secondary-shift-q");
                self.settle(cx);
            }

            fn asking(&self, cx: &mut VisualTestContext) -> bool {
                self.shell
                    .read_with(cx, |shell, _| shell.quit_dialog.is_open())
            }

            fn quits(&self, cx: &mut VisualTestContext) -> usize {
                cx.update(|_, cx| crate::quit::quits(cx))
            }

            fn click(&self, selector: &'static str, cx: &mut VisualTestContext) {
                let bounds = cx.debug_bounds(selector).expect(selector);
                cx.simulate_click(bounds.center(), Modifiers::default());
                self.settle(cx);
            }
        }

        #[gpui::test]
        fn should_ask_on_quit_while_an_agent_runs(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(vec![agent(1), agent(2)], true), cx);

            harness.press_quit(cx);

            assert!(harness.asking(cx));
            assert_eq!(harness.quits(cx), 0);
            let shown = harness.shell.read_with(cx, |shell, _| shell.quit_running);
            assert_eq!(shown, 2);
        }

        #[gpui::test]
        fn should_ask_when_the_compositor_closes_the_window_while_an_agent_runs(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(daemon(vec![agent(1)], true), cx);

            let closed = cx.simulate_close();
            harness.settle(cx);

            assert!(!closed);
            assert!(harness.asking(cx));
        }

        #[gpui::test]
        fn should_ask_from_the_title_bars_close_button_while_an_agent_runs(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(daemon(vec![agent(1)], true), cx);
            let window = cx.update(|window, _| window.window_handle());

            cx.dispatch_action(Close);
            harness.settle(cx);

            assert!(harness.asking(cx));
            assert!(cx.windows().contains(&window));
        }

        #[gpui::test]
        fn should_ask_while_a_shell_hosts_an_agent(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(vec![hosting_shell(1)], true), cx);

            harness.press_quit(cx);

            assert!(harness.asking(cx));
        }

        #[gpui::test]
        fn should_quit_at_once_with_only_plain_shells(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(vec![plain_shell(1)], true), cx);

            harness.press_quit(cx);

            assert!(!harness.asking(cx));
            assert_eq!(harness.quits(cx), 1);
            assert!(cx.simulate_close());
        }

        #[gpui::test]
        fn should_quit_at_once_when_the_setting_is_off(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(vec![agent(1)], false), cx);

            harness.press_quit(cx);

            assert!(!harness.asking(cx));
            assert_eq!(harness.quits(cx), 1);
            assert!(cx.simulate_close());
        }

        #[gpui::test]
        fn should_quit_on_enter(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(vec![agent(1)], true), cx);
            harness.press_quit(cx);
            assert_eq!(harness.quits(cx), 0);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(harness.quits(cx), 1);
            assert!(harness.api.settings_patches.lock().unwrap().is_empty());
        }

        #[gpui::test]
        fn should_quit_once_from_the_quit_button(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(vec![agent(1)], true), cx);
            harness.press_quit(cx);

            harness.click("quit confirm", cx);

            assert_eq!(harness.quits(cx), 1);
            assert!(!harness.asking(cx));
        }

        #[gpui::test]
        fn should_keep_the_app_on_escape(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(vec![agent(1)], true), cx);
            harness.press_quit(cx);

            cx.simulate_keystrokes("escape");
            harness.settle(cx);

            assert!(!harness.asking(cx));
            assert_eq!(harness.quits(cx), 0);
        }

        #[gpui::test]
        fn should_save_dont_ask_again_then_quit_without_asking_next_time(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(vec![agent(1)], true), cx);
            harness.press_quit(cx);

            harness.click("quit dont ask again", cx);
            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(harness.quits(cx), 1);
            assert!(!harness.api.settings_now().gui.confirm_quit);
            harness.press_quit(cx);
            assert!(!harness.asking(cx));
            assert_eq!(harness.quits(cx), 2);
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
                let shell = cx
                    .new(|cx| Shell::new(connection, Rc::new(FakeNotifier::default()), window, cx));
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
