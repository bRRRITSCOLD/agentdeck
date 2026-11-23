//! The main window's content: what the daemon connection allows. Until it connects, a
//! centred status; once it does, the sidebar beside the selected view, under a banner with a
//! Restart while the daemon predates the app: a workspace's panes, the Pull requests view or
//! Settings, and the new workspace dialog the sidebar's `+` opens. Settings is made as the window connects, so the saved theme applies
//! from the start. When the compositor leaves decorations to the app, a title bar and
//! resize edges frame it.
//!
//! It mutes and unmutes the selected workspace's notifications when the top bar asks, toasting
//! a failure. It archives and unarchives the selected workspace when the top bar asks, toasting
//! the outcome; an archive's toast offers Undo. The top bar's Edit workspace opens the edit
//! dialog on the selected workspace, and the sidebar reads again once it renames. Its Delete
//! workspace… opens the delete dialog; once a workspace is deleted the selection falls back to
//! the first workspace left, else Pull requests.
//!
//! Once an agent starts, the window shows its workspace and tab.
//!
//! It switches the workspace shown between its Panes and its Pull requests when the top bar or
//! `secondary-shift-p` asks. A Workspace's name in a group header, on either PR list, shows
//! that Workspace's Pull requests ([`Shell::open_workspace_prs`]).
//!
//! It shows the sidebar's [`Alerts`] as OS notifications (spec §11), leaving out one while the
//! user can see what it is about in the focused window: the pane's tab, or, for a PR, the Pull
//! requests view. Clicking one focuses the window and shows what it is about: a PR alert lands
//! on the PR's row ([`PullRequestsView::reveal`]). One still open is withdrawn once the user can
//! see what it is about: the window focused, the workspace, tab or Pull requests view shown.
//!
//! Closing the window (its own close button or the compositor's) and Quit ask first while an
//! agent runs, unless `gui.confirm_quit` is off (spec §11): "Quit agentdeck?", Penpot "Screens"
//! → "Quit · agents running". Quit, or Enter, quits, after saving `gui.confirm_quit = false`
//! when Don't ask again is ticked, and after stopping each running agent when Stop the agents
//! is; Cancel, or Escape, keeps the app.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use agentdeck_client::ApiError;
use agentdeck_core::{PaneId, WorkspaceId};
use agentdeck_notify::Notifier;
use brrritscold_core_gpui::{
    ActiveTheme as _, Alert, AlertAction, AlertDescription, AlertDialog, AlertDialogAction,
    AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter,
    AlertDialogHeader, AlertDialogTitle, AlertTitle, AlertVariant, Button, ButtonSize,
    ButtonVariant, Checkbox, DialogHandle, Field, FieldContent, FieldOrientation, IconName, Label,
    Separator, SeparatorOrientation,
};
use brrritscold_core_gpui_tokens::primitive::space;
use futures_util::future::{self, Either};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Decorations, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_base::actions::Confirm;

use crate::api::Api;
use crate::connection::{Connection, Status};
use crate::delete_workspace::{DeleteEvent, DeleteTarget, DeleteWorkspaceDialog};
use crate::edit_workspace::{EditTarget, EditWorkspaceDialog, Renamed};
use crate::frame;
use crate::gui_state::{self, Side};
use crate::keymap::ToggleSide;
use crate::new_agent::{self, Launched, NewAgent, NewAgentDialog, Target, home_folder};
use crate::new_workspace::{Created, NewWorkspaceDialog};
use crate::notice::{self, Notice, NoticeAction};
use crate::notifications::{self, Alert as Notification};
use crate::pull_requests::{OpenWorkspace, PullRequestsView, Scope, Synced};
use crate::quit;
use crate::settings::SettingsView;
use crate::sidebar::{Alerts, Selection, SidebarEvent, SidebarView};
use crate::sync_parts::OpenSettings;
use crate::title_bar::{Close, Minimize, TitleBar, ToggleMaximize};
use crate::top_bar::{
    ArchiveWorkspace, DeleteWorkspace, EditWorkspace, MuteWorkspace, ShowPanes, ShowPullRequests,
    TopBar, UnarchiveWorkspace, UnmuteWorkspace,
};
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
    _new_workspace: Vec<Subscription>,
    /// The selected workspace.
    workspace: Option<Entity<WorkspaceView>>,
    /// Follows the tab the selected workspace shows.
    _workspace: Vec<Subscription>,
    /// The Pull requests view, while connected; shown while the sidebar selects it.
    pull_requests: Option<Entity<PullRequestsView>>,
    /// Settings, while connected; shown while the sidebar's gear selects it. It also applies
    /// the saved theme.
    settings: Option<Entity<SettingsView>>,
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
    /// Opens and closes "Quit agentdeck?".
    quit_dialog: DialogHandle,
    /// The panes whose agents ran when the quit dialog opened.
    quit_running: Vec<PaneId>,
    /// Whether the quit dialog's Don't ask again is ticked.
    dont_ask_again: bool,
    /// Whether the quit dialog's Stop the agents is ticked.
    stop_agents: bool,
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
            _new_workspace: Vec::new(),
            workspace: None,
            _workspace: Vec::new(),
            pull_requests: None,
            settings: None,
            selection: None,
            focus,
            notifier,
            alerts: HashMap::new(),
            _activation: activation,
            quit_dialog: DialogHandle::new(false),
            quit_running: Vec::new(),
            dont_ask_again: false,
            stop_agents: false,
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
        let running = (self.sidebar.as_ref()).map_or(Vec::new(), |sidebar| {
            quit::running_agents(sidebar.read(cx).snapshot())
        });
        let confirm = (self.settings.as_ref()).is_none_or(|view| view.read(cx).confirm_quit());
        if running.is_empty() || !confirm {
            return false;
        }
        if !self.quit_dialog.is_open() {
            self.quit_running = running;
            self.dont_ask_again = false;
            self.stop_agents = false;
            self.quit_dialog.open(window, cx);
        }
        cx.notify();
        true
    }

    /// The quit dialog's Quit, and Enter: saves `gui.confirm_quit = false` first when Don't ask
    /// again is ticked, then quits, whether or not the save went through. With Stop the agents
    /// ticked it also stops each agent the dialog counted, and waits for the stops up to
    /// [`quit::STOP_GRACE`] from now, quitting then whether or not they all landed; a failed
    /// stop is logged, and that agent keeps running. Nothing remembers the tick: the next quit
    /// leaves agents running unless it is ticked again.
    fn quit_confirmed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quit_dialog.close(window, cx);
        let saving = (self.settings.clone())
            .filter(|_| self.dont_ask_again)
            .map(|view| view.update(cx, |view, cx| view.set_confirm_quit(false, window, cx)));
        let stops: Vec<_> = match self.api(cx).filter(|_| self.stop_agents) {
            Some(api) => (self.quit_running.iter())
                .map(|pane| {
                    let stop = api.stop_pane(*pane, cx);
                    let pane = *pane;
                    async move {
                        if let Err(err) = stop.await {
                            tracing::warn!(%pane, "couldn't stop the agent on quit: {err:#}");
                        }
                    }
                })
                .collect(),
            None => Vec::new(),
        };
        let grace = cx.background_executor().timer(quit::STOP_GRACE);
        cx.spawn(async move |_, cx| {
            if let Some(saving) = saving {
                saving.await;
            }
            if !stops.is_empty() {
                let stopping = future::join_all(stops);
                if let Either::Right(_) = future::select(Box::pin(stopping), grace).await {
                    tracing::warn!("quitting before every agent's stop landed");
                }
            }
            cx.update(quit::now);
        })
        .detach();
    }

    /// One of the quit dialog's horizontal Fields: a Checkbox, `on` or not, labelled `label`,
    /// whose tick `set` saves; clicking the label ticks it too. Its checkbox is element `id`,
    /// its label `<id>-label`, and `selector` finds it under test.
    fn quit_option(
        id: &'static str,
        selector: &'static str,
        label: &'static str,
        on: bool,
        set: fn(&mut Shell, bool),
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let tick = move |on: bool, cx: &mut App| {
            let _ = this.update(cx, |shell, cx| {
                set(shell, on);
                cx.notify();
            });
        };
        let tick_label = tick.clone();
        Field::new()
            .orientation(FieldOrientation::Horizontal)
            .child(
                div().debug_selector(move || selector.into()).child(
                    Checkbox::new(id)
                        .checked(on)
                        .accessibility_label(label)
                        .on_change(move |on, _, cx| tick(on, cx)),
                ),
            )
            .content(
                FieldContent::new().child(
                    div()
                        .id(SharedString::from(format!("{id}-label")))
                        .cursor_pointer()
                        .on_click(move |_, _, cx| tick_label(!on, cx))
                        .child(Label::new().child(label)),
                ),
            )
    }

    /// "Quit agentdeck?", Penpot "Screens" → "Quit · agents running" (and "· stop ticked"): the
    /// running count over Don't ask again and Stop the agents, then Cancel and Quit. Enter and
    /// the Quit button dispatch the dialog's `Confirm`, caught here on its way down to the
    /// dialog, which then closes.
    fn quit_confirmation(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // core-gpui's content has no body part, so the options lead the footer's column,
        // `space.3` apart and `space.6` above the buttons, as Penpot places them between the
        // header and the footer.
        let options = div()
            .flex()
            .flex_col()
            .gap(space::S3)
            .child(Self::quit_option(
                "quit-dont-ask-again",
                "quit dont ask again",
                "Don't ask again",
                self.dont_ask_again,
                |shell, on| shell.dont_ask_again = on,
                cx,
            ))
            .child(Self::quit_option(
                "quit-stop-agents",
                "quit stop agents",
                "Stop the agents",
                self.stop_agents,
                |shell, on| shell.stop_agents = on,
                cx,
            ));
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
                                        self.quit_running.len(),
                                        self.stop_agents,
                                    ))),
                            )
                            .footer(
                                AlertDialogFooter::new()
                                    .flex_col()
                                    .gap(space::S6)
                                    .child(options)
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
                    let pull_requests =
                        cx.new(|cx| PullRequestsView::new(api.clone(), Scope::All, cx));
                    let new_agent = cx.new(|_| NewAgentDialog::new(api.clone()));
                    let edit = cx.new(|cx| EditWorkspaceDialog::new(api.clone(), window, cx));
                    let delete = cx.new(|cx| DeleteWorkspaceDialog::new(api.clone(), window, cx));
                    let settings =
                        cx.new(|cx| SettingsView::new(api.clone(), &connection, window, cx));
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
                        // A sync that changes no PR broadcasts nothing, yet moves when PRs
                        // last synced.
                        cx.subscribe(&pull_requests, |shell, _, _: &Synced, cx| {
                            if let Some(sidebar) = &shell.sidebar {
                                sidebar.update(cx, |sidebar, cx| sidebar.refresh(cx));
                            }
                        }),
                        cx.subscribe(&pull_requests, |shell, _, _: &OpenSettings, cx| {
                            shell.open_settings(cx);
                        }),
                        cx.subscribe_in(
                            &pull_requests,
                            window,
                            |shell, _, OpenWorkspace(id): &OpenWorkspace, window, cx| {
                                shell.open_workspace_prs(*id, window, cx);
                            },
                        ),
                        // Show the agent's workspace and tab, and focus it: it may be asking
                        // about trust.
                        cx.subscribe_in(
                            &new_agent,
                            window,
                            |shell, _, launched: &Launched, window, cx| {
                                shell.show_launched(*launched, window, cx);
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
                        cx.subscribe(&dialog, |shell, _, Created(id), cx| {
                            if let Some(sidebar) = &shell.sidebar {
                                let selection = Selection::Workspace(*id);
                                sidebar.update(cx, |sidebar, cx| sidebar.select(selection, cx));
                            }
                        }),
                    ];
                    self.new_workspace = Some(dialog);
                    self.pull_requests = Some(pull_requests);
                    self.new_agent = Some(new_agent);
                    self.edit_workspace = Some(edit);
                    self.delete_workspace = Some(delete);
                    self.settings = Some(settings);
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
                self.pull_requests = None;
                self.settings = None;
                self.new_agent = None;
                self.edit_workspace = None;
                self.delete_workspace = None;
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
        if let Some(view) = &self.pull_requests {
            view.update(cx, |view, cx| {
                view.set_snapshot(snapshot, cx);
                if selection != Selection::PullRequests {
                    view.forget_revealed(cx);
                }
            });
        }
        let Status::Connected(session) = self.connection.read(cx).status() else {
            return;
        };
        let sidebar = sidebar.read(cx);
        let snapshot = sidebar.snapshot().clone();
        let row = match selection {
            Selection::Workspace(selected) => sidebar.overview().workspace(selected),
            Selection::PullRequests | Selection::Settings => None,
        };
        let Some(row) = row else {
            self.workspace = None;
            cx.notify();
            return;
        };
        let top_bar = TopBar::new(row.name.clone())
            .archived(row.archived)
            .muted(row.muted)
            .prs(row.counts.prs);
        let selected = row.id;
        // Archiving or unarchiving swaps the view: an archived one holds no terminals.
        if let Some(view) = &self.workspace
            && view.read(cx).workspace() == selected
            && view.read(cx).top_bar().is_archived() == row.archived
        {
            view.update(cx, |view, cx| {
                view.set_top_bar(top_bar, cx);
                view.set_snapshot(snapshot, cx);
            });
            return;
        }
        let (api, sender) = (session.api.clone(), session.sender.clone());
        let connection = self.connection.clone();
        let view = cx.new(|cx| {
            let mut view =
                WorkspaceView::new(selected, top_bar, api, connection, sender, window, cx);
            view.set_snapshot(snapshot, cx);
            view
        });
        self._workspace = vec![
            // Its tabs read, another tab shown, or the other side.
            cx.observe_in(&view, window, |shell, _, window, cx| {
                shell.withdraw_seen(window, cx);
            }),
            cx.subscribe_in(
                &view,
                window,
                |shell, _, OpenWorkspace(id): &OpenWorkspace, window, cx| {
                    shell.open_workspace_prs(*id, window, cx);
                },
            ),
            cx.subscribe(&view, |shell, _, _: &OpenSettings, cx| {
                shell.open_settings(cx)
            }),
        ];
        self.workspace = Some(view);
        cx.notify();
    }

    /// Opens the new agent dialog on the workspace shown.
    fn new_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Selection::Workspace(selected)) = self.selection {
            self.open_new_agent(selected, window, cx);
        }
    }

    /// Opens the new agent dialog on `workspace`, offering where a new pane there starts, its
    /// panes' folders while it is shown, and `~`.
    fn open_new_agent(
        &mut self,
        workspace: WorkspaceId,
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
        // Where a new pane in the workspace starts, while it is shown (spec §11); else `~`.
        let shown = (self.workspace.as_ref()).filter(|view| view.read(cx).workspace() == workspace);
        let (folder, panes) = shown.map_or_else(
            || (home_folder(), Vec::new()),
            |view| {
                let view = view.read(cx);
                (view.new_pane_folder(window, cx), view.pane_folders())
            },
        );
        let target = Target {
            id: row.id,
            name: row.name.clone(),
            folder: folder.clone().into(),
        };
        let home = std::env::home_dir();
        let folders = new_agent::folders(&folder, &panes, home.as_deref());
        dialog.update(cx, |dialog, cx| {
            dialog.open(target, folders, window, cx);
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
    /// tab shown; for a PR, or several, the Pull requests view.
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
            notifications::Target::Pr(_) | notifications::Target::PullRequests => {
                self.selection == Some(Selection::PullRequests)
            }
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
            notifications::Target::Pr(pr) => {
                self.show_pull_requests(cx);
                if let Some(view) = &self.pull_requests {
                    view.update(cx, |view, cx| view.reveal(pr, cx));
                }
            }
            notifications::Target::PullRequests => self.show_pull_requests(cx),
        }
    }

    /// Shows `workspace`'s Pull requests, as a click on its name in a group header asks:
    /// remembers the side, then shows it, selecting the workspace first if another view shows.
    pub fn open_workspace_prs(
        &mut self,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        gui_state::update(cx, |state| state.set_side(workspace, Side::PullRequests));
        if let Some(view) = self.show_workspace(workspace, window, cx) {
            view.update(cx, |view, cx| view.set_side(Side::PullRequests, window, cx));
        }
    }

    /// Shows `side` of the workspace shown.
    fn show_side(&mut self, side: Option<Side>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = &self.workspace {
            view.update(cx, |view, cx| match side {
                Some(side) => view.set_side(side, window, cx),
                None => view.toggle_side(window, cx),
            });
        }
    }

    /// Selects the Pull requests view.
    fn show_pull_requests(&mut self, cx: &mut Context<Self>) {
        if let Some(sidebar) = &self.sidebar {
            sidebar.update(cx, |sidebar, cx| {
                sidebar.select(Selection::PullRequests, cx)
            });
        }
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

    /// Shows Settings at its GitHub section, the top: what the Pull requests view's Open
    /// Settings and "Settings" links ask for.
    fn open_settings(&mut self, cx: &mut Context<Self>) {
        if let Some(sidebar) = &self.sidebar {
            let settings = Selection::Settings;
            sidebar.update(cx, |sidebar, cx| sidebar.select(settings, cx));
        }
        if let Some(settings) = &self.settings {
            settings.update(cx, |settings, cx| settings.scroll_to_github(cx));
        }
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
                        .description("Its panes and linked Repositories are kept.")
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
    /// no design for it, so it is the Pull requests view's logged-out banner (a default `Alert`
    /// with the `Info` icon) with the failure alert's outline sm action. Restart is the user's choice, since
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
                    body.children(self.workspace.clone())
                }
                Some(Selection::PullRequests) if self.pull_requests.is_some() => {
                    body.children(self.pull_requests.clone())
                }
                Some(Selection::Settings) if self.settings.is_some() => {
                    body.children(self.settings.clone())
                }
                _ => body.child(div().flex_1()),
            })
            .children(self.new_workspace.clone())
            .children(self.new_agent.clone())
            .children(self.edit_workspace.clone())
            .children(self.delete_workspace.clone())
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
            .on_action(cx.listener(|this, _: &ShowPanes, window, cx| {
                this.show_side(Some(Side::Panes), window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowPullRequests, window, cx| {
                this.show_side(Some(Side::PullRequests), window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleSide, window, cx| {
                this.show_side(None, window, cx);
            }))
            .on_action(cx.listener(|this, _: &UnarchiveWorkspace, window, cx| {
                if let Some((workspace, name, true)) = this.selected_workspace(cx) {
                    this.unarchive(workspace, name, window, cx);
                }
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

        #[gpui::test]
        fn should_draw_a_workspaces_pull_requests_in_both_themes(cx: &mut TestAppContext) {
            use crate::gui_state::{self, Side};
            use crate::overview::fixtures::{links, pull_request};

            let connector = Rc::new(FakeConnector::default());
            let api = workspace_with_a_pane();
            api.set(Snapshot {
                workspaces: vec![workspace(1, "agentdeck")],
                repositories: vec![
                    links("acme/widgets", &[id(1)]),
                    links("acme/tools", &[id(1)]),
                ],
                prs: vec![pull_request(1, "acme/widgets")],
                ..Snapshot::default()
            });
            let _remote = connector.then_connect_to(api);
            cx.update(|cx| gui_state::update(cx, |s| s.set_side(id(1), Side::PullRequests)));

            draw_in_both_themes(connector, cx);
        }

        #[gpui::test]
        fn should_draw_the_sidebar_with_no_workspaces_in_both_themes(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let _remote = connector.then_connect_to(Arc::new(FakeApi::with(Snapshot::default())));

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

        mod new_agent {
            use super::*;

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
                assert_eq!(dialog.read_with(cx, |d, _| d.workspace()), Some(id(1)));
            }

            #[gpui::test]
            fn should_start_the_new_agent_in_the_focused_panes_folder(cx: &mut TestAppContext) {
                let connector = Rc::new(FakeConnector::default());
                let api = workspace_with_a_pane();
                api.set_panes(vec![agentdeck_client::types::Pane {
                    folder: "/srv/notes".into(),
                    ..crate::pane::fixtures::shell_pane(7, id(1))
                }]);
                let _remote = connector.then_connect_to(api);
                let shell = open(connector, cx);
                let window = cx.windows()[0];

                window
                    .update(cx, |_, window, cx| {
                        window.dispatch_action(Box::new(NewAgent), cx)
                    })
                    .unwrap();
                cx.run_until_parked();

                let dialog = shell.read_with(cx, |shell, _| shell.new_agent.clone().unwrap());
                let (folder, offered) = dialog.read_with(cx, |d, _| d.folders());
                assert_eq!(folder, "/srv/notes");
                let offered: Vec<String> = offered.iter().map(|f| f.path.to_string()).collect();
                assert_eq!(offered, ["/srv/notes".to_owned(), home_folder()]);
            }
        }

        #[gpui::test]
        fn should_fall_back_to_the_pull_requests_view_with_no_workspace(cx: &mut TestAppContext) {
            let connector = Rc::new(FakeConnector::default());
            let _remote = connector.then_connect_to(Arc::new(FakeApi::default()));

            let shell = open(connector, cx);

            shell.read_with(cx, |shell, _| {
                assert_eq!(shell.selection, Some(Selection::PullRequests));
                assert!(shell.pull_requests.is_some());
            });
            assert_eq!(shown_workspace(&shell, cx), None);
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
            fn should_show_the_pull_requests_view_with_the_prs_the_sidebar_reads(
                cx: &mut TestAppContext,
            ) {
                let connector = Rc::new(FakeConnector::default());
                let api = workspace_with_a_pane();
                api.set_prs(vec![crate::overview::fixtures::pull_request(
                    7,
                    "acme/widgets",
                )]);
                let _remote = connector.then_connect_to(api);
                let shell = open(connector, cx);
                let sidebar = shell.read_with(cx, |shell, _| shell.sidebar.clone().unwrap());

                sidebar.update(cx, |sidebar, cx| {
                    sidebar.select(Selection::PullRequests, cx)
                });
                cx.run_until_parked();

                let view = shell.read_with(cx, |shell, _| {
                    assert_eq!(shell.selection, Some(Selection::PullRequests));
                    shell.pull_requests.clone().unwrap()
                });
                let groups = view.read_with(cx, |view, cx| view.groups(cx));
                assert_eq!(groups.len(), 1);
                assert_eq!(groups[0].prs[0].number, 7);
                assert_eq!(shown_workspace(&shell, cx), None);
            }

            #[gpui::test]
            fn should_show_settings_when_the_pull_requests_view_asks(cx: &mut TestAppContext) {
                let connector = Rc::new(FakeConnector::default());
                let _remote = connector.then_connect_to(workspace_with_a_pane());
                let shell = open(connector, cx);
                let view = shell.read_with(cx, |shell, _| shell.pull_requests.clone().unwrap());

                view.update(cx, |_, cx| cx.emit(OpenSettings));
                cx.run_until_parked();

                shell.read_with(cx, |shell, _| {
                    assert_eq!(shell.selection, Some(Selection::Settings));
                });
            }

            #[gpui::test]
            fn should_read_again_once_a_sync_from_the_pull_requests_view_ends(
                cx: &mut TestAppContext,
            ) {
                let connector = Rc::new(FakeConnector::default());
                let api = workspace_with_a_pane();
                let _remote = connector.then_connect_to(api.clone());
                let shell = open(connector, cx);
                let view = shell.read_with(cx, |shell, _| shell.pull_requests.clone().unwrap());
                let reads = api.reads();

                view.update(cx, |_, cx| cx.emit(Synced));
                cx.run_until_parked();

                assert_eq!(api.reads(), reads + 1);
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

                assert_eq!(shown_top_bar(&shell, cx), Some(TopBar::new("agentdeck")));
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

                assert_eq!(shown_top_bar(&shell, cx), Some(TopBar::new("renamed")));
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

    /// A Workspace's two sides, Panes and Pull requests, switched in its top bar.
    mod side {
        use agentdeck_client::types::Workspace;

        use super::support::Harness;
        use super::*;
        use crate::edit_workspace::EditWorkspaceDialog;
        use crate::gui_state::{self, Side};
        use crate::overview::fixtures::{links, pull_request};
        use crate::top_bar;

        /// Workspace 1, `agentdeck`, with a tab, linking widgets, which has two open PRs;
        /// workspace 2, `docs`, linking nothing; workspace 3, `old`, archived.
        fn daemon() -> Arc<FakeApi> {
            let old = Workspace {
                archived_at: Some("2026-11-13T00:00:00Z".into()),
                ..workspace(3, "old")
            };
            let api = Arc::new(FakeApi::with(Snapshot {
                workspaces: vec![workspace(1, "agentdeck"), workspace(2, "docs"), old],
                repositories: vec![links("acme/widgets", &[id(1)])],
                prs: vec![
                    pull_request(1, "acme/widgets"),
                    pull_request(2, "acme/widgets"),
                ],
                ..Snapshot::default()
            }));
            api.set_tabs(id(1), vec![tab(1, id(1), 0, Some(id(7)))]);
            api
        }

        fn open(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            cx.update(crate::keymap::init);
            Harness::open(daemon(), cx)
        }

        /// The workspace shown and its side.
        fn showing(harness: &Harness, cx: &mut VisualTestContext) -> Option<(WorkspaceId, Side)> {
            harness.shell.read_with(cx, |shell, cx| {
                (shell.workspace.as_ref()).map(|view| {
                    let view = view.read(cx);
                    (view.workspace(), view.side())
                })
            })
        }

        fn shows(selector: &'static str, cx: &mut VisualTestContext) -> bool {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.debug_bounds(selector).is_some()
        }

        fn click(harness: &Harness, selector: &'static str, cx: &mut VisualTestContext) {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let at = cx.debug_bounds(selector).expect(selector).center();
            harness.click(at, cx);
        }

        /// Clicks `side` in the top bar's switch: Panes at its left end, Pull requests at its
        /// right.
        fn click_side(harness: &Harness, side: Side, cx: &mut VisualTestContext) {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let switch = cx.debug_bounds("side switch").expect("the switch");
            let x = match side {
                Side::Panes => switch.left() + gpui::px(8.),
                Side::PullRequests => switch.right() - gpui::px(8.),
            };
            harness.click(gpui::point(x, switch.center().y), cx);
        }

        #[gpui::test]
        fn should_open_a_new_workspace_on_its_panes(cx: &mut TestAppContext) {
            let (harness, cx) = open(cx);

            assert_eq!(showing(&harness, cx), Some((id(1), Side::Panes)));
            let name = cx.debug_bounds("workspace name").expect("the name");
            let switch = cx.debug_bounds("side switch").expect("the switch");
            let new_agent = cx.debug_bounds("new agent").expect("New agent");
            assert!(name.right() < switch.left() && switch.right() < new_agent.left());
            assert!(!shows("workspace pull requests", cx));
        }

        #[gpui::test]
        fn should_count_its_open_prs_in_the_switch(cx: &mut TestAppContext) {
            let (harness, cx) = open(cx);

            let prs = harness.shell.read_with(cx, |shell, cx| {
                shell
                    .workspace
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .top_bar()
                    .open_prs()
            });
            assert_eq!(prs, 2);
            assert_eq!(top_bar::pull_requests_label(2), "Pull requests · 2");
            assert_eq!(top_bar::pull_requests_label(0), "Pull requests");
        }

        #[gpui::test]
        fn should_switch_sides_from_the_top_bar(cx: &mut TestAppContext) {
            let (harness, cx) = open(cx);

            click_side(&harness, Side::PullRequests, cx);

            assert_eq!(showing(&harness, cx), Some((id(1), Side::PullRequests)));
            assert!(shows("workspace pull requests", cx));
            assert!(cx.debug_bounds("pr acme/widgets#1").is_some());
            click_side(&harness, Side::Panes, cx);
            assert_eq!(showing(&harness, cx), Some((id(1), Side::Panes)));
            assert!(!shows("workspace pull requests", cx));
        }

        #[gpui::test]
        fn should_toggle_sides_with_secondary_shift_p(cx: &mut TestAppContext) {
            let (harness, cx) = open(cx);

            cx.simulate_keystrokes("secondary-shift-p");
            harness.settle(cx);
            assert_eq!(showing(&harness, cx), Some((id(1), Side::PullRequests)));

            cx.simulate_keystrokes("secondary-shift-p");
            harness.settle(cx);
            assert_eq!(showing(&harness, cx), Some((id(1), Side::Panes)));
        }

        #[gpui::test]
        fn should_remember_each_workspaces_side_across_restarts(cx: &mut TestAppContext) {
            let dir = tempfile::tempdir().expect("a temp dir");
            let path = dir.path().join(gui_state::FILE);
            cx.update(|cx| gui_state::init(Some(path.clone()), cx));
            let (harness, window) = open(cx);
            click_side(&harness, Side::PullRequests, window);

            assert_eq!(gui_state::load(&path).side(id(1)), Side::PullRequests);
            assert_eq!(gui_state::load(&path).side(id(2)), Side::Panes);
            // The next start reads the file again.
            cx.update(|cx| gui_state::init(Some(path), cx));
            let (harness, cx) = open(cx);
            assert_eq!(showing(&harness, cx), Some((id(1), Side::PullRequests)));
        }

        #[gpui::test]
        fn should_open_the_remembered_side_from_a_sidebar_row(cx: &mut TestAppContext) {
            let (harness, cx) = open(cx);
            click_side(&harness, Side::PullRequests, cx);

            harness.select(2, cx);
            assert_eq!(showing(&harness, cx), Some((id(2), Side::Panes)));
            harness.select(1, cx);
            assert_eq!(showing(&harness, cx), Some((id(1), Side::PullRequests)));
        }

        #[gpui::test]
        fn should_open_a_workspaces_pull_requests_from_a_group_header(cx: &mut TestAppContext) {
            let (harness, cx) = open(cx);
            let sidebar = harness
                .shell
                .read_with(cx, |shell, _| shell.sidebar.clone());
            sidebar.unwrap().update(cx, |sidebar, cx| {
                sidebar.select(Selection::PullRequests, cx);
            });
            harness.settle(cx);

            click(&harness, "workspace link acme/widgets agentdeck", cx);

            assert_eq!(showing(&harness, cx), Some((id(1), Side::PullRequests)));
        }

        #[gpui::test]
        fn should_open_its_own_pull_requests_from_its_name_on_its_panes_side(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = open(cx);
            click_side(&harness, Side::PullRequests, cx);
            click_side(&harness, Side::Panes, cx);
            cx.update(|window, cx| {
                (harness.shell).update(cx, |shell, cx| shell.open_workspace_prs(id(1), window, cx));
            });
            harness.settle(cx);

            assert_eq!(showing(&harness, cx), Some((id(1), Side::PullRequests)));
        }

        #[gpui::test]
        fn should_show_link_repository_for_a_workspace_linking_nothing(cx: &mut TestAppContext) {
            let (harness, cx) = open(cx);
            harness.select(2, cx);
            click_side(&harness, Side::PullRequests, cx);

            click(&harness, "link repository", cx);

            let dialog: Entity<EditWorkspaceDialog> = harness
                .shell
                .read_with(cx, |shell, _| shell.edit_workspace.clone().unwrap());
            assert!(dialog.read_with(cx, |d, _| d.is_open()));
            assert_eq!(cx.update(|_, cx| dialog.read(cx).name(cx)), "docs");
        }

        #[gpui::test]
        fn should_hide_the_switch_on_an_archived_workspace(cx: &mut TestAppContext) {
            let (harness, cx) = open(cx);
            click_side(&harness, Side::PullRequests, cx);
            harness.select(3, cx);

            assert!(shows("archived", cx));
            assert!(!shows("side switch", cx));
            assert!(!shows("workspace pull requests", cx));
            cx.simulate_keystrokes("secondary-shift-p");
            harness.settle(cx);
            assert!(shows("archived", cx));
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

    mod workspace_menu {
        use super::support::{Harness, daemon};
        use super::*;

        /// Penpot "Screens · Main" → "Workspace menu": its right edge on the trigger's, 4px
        /// below it.
        #[gpui::test]
        fn should_open_end_aligned_4px_below_its_trigger(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(), cx);
            let trigger = cx.debug_bounds("workspace menu").expect("the menu shows");

            harness.click(trigger.center(), cx);

            let menu = cx.debug_bounds("dropdown-menu-content").expect("it opens");
            assert_eq!(menu.right(), trigger.right());
            assert_eq!(menu.top(), trigger.bottom() + px(4.));
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
                Some("Its panes and linked Repositories are kept.")
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
            fn should_select_pull_requests_once_the_last_workspace_is_deleted(
                cx: &mut TestAppContext,
            ) {
                let api = Arc::new(FakeApi::with(Snapshot {
                    workspaces: vec![workspace(1, "agentdeck")],
                    ..Snapshot::default()
                }));
                let (harness, cx) = Harness::open(api, cx);

                open_and_delete(&harness, "agentdeck", cx);

                assert_eq!(*harness.api.deleted.lock().unwrap(), [id(1)]);
                assert_eq!(selection(&harness, cx), Some(Selection::PullRequests));
            }
        }
    }

    mod notifications {
        use agentdeck_client::types::Pane;
        use agentdeck_client::ws::{Event, Remote};
        use agentdeck_core::ws::{Entity as Changed, ServerMessage};
        use agentdeck_core::{Agent, AgentState, ChecksState, PaneId, PaneKind};

        use super::*;
        use crate::overview::PaneState;
        use crate::overview::fixtures::{pane, tab};
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
            fn should_notify_for_a_pane_while_its_workspace_shows_its_pull_requests(
                cx: &mut TestAppContext,
            ) {
                let mut open = Open::new(daemon(), cx);
                open.cx.dispatch_action(ShowPullRequests);
                open.settle();

                open.turn(7, AgentState::Blocked);

                assert_eq!(open.shown().len(), 1);
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
        }

        /// PR alerts (spec §11): only PRs you authored, one per PR, a click landing on its row
        /// in the Pull requests view, muted by the Workspaces linking its Repository.
        mod pr_alerts {
            use agentdeck_client::types::PullRequest;
            use agentdeck_core::{RepositoryId, ReviewState};

            use super::*;
            use crate::gui_state;
            use crate::notifications::pr_tag;
            use crate::overview::fixtures::{links, pr_status, pull_request, repository_id};
            use crate::pull_requests::PrKey;

            /// The signed-in login; [`pull_request`] is authored by it.
            const ME: &str = "octocat";

            /// PR `n` of `repo`, "PR n", authored by [`ME`], with `checks`.
            fn pr(n: u32, repo: &str, checks: ChecksState) -> PullRequest {
                PullRequest {
                    checks: Some(checks),
                    ..pull_request(n, repo)
                }
            }

            fn key(n: u32, repo: &str) -> PrKey {
                PrKey {
                    repository: repository_id(repo),
                    number: n,
                }
            }

            fn tag(n: u32, repo: &str) -> SharedString {
                pr_tag(key(n, repo))
            }

            /// [`daemon`]'s workspaces, synced as [`ME`], holding `prs`, `acme/web` linked by
            /// `web` (1) and `acme/api` by `api` (2).
            fn holding(prs: Vec<PullRequest>) -> Arc<FakeApi> {
                let api = daemon();
                let mut snapshot = api.snapshot_now();
                let mut status = pr_status();
                status.synced_as = Some(ME.into());
                snapshot.pr_sync = Some(status);
                snapshot.prs = prs;
                snapshot.repositories =
                    vec![links("acme/web", &[id(1)]), links("acme/api", &[id(2)])];
                api.set(snapshot);
                api
            }

            impl Open {
                /// A PR sync ends holding `prs`.
                fn sync(&mut self, prs: Vec<PullRequest>) {
                    let mut snapshot = self.api.snapshot_now();
                    snapshot.prs = prs;
                    self.api.set(snapshot);
                    self.send(ServerMessage::Changed {
                        entity: Changed::PrSync,
                        id: "status".into(),
                    });
                }

                fn titles(&self) -> Vec<String> {
                    (self.shown().into_iter())
                        .map(|n| n.title.to_string())
                        .collect()
                }

                fn selection(&mut self) -> Option<Selection> {
                    self.shell.read_with(self.cx, |shell, _| shell.selection)
                }

                fn pull_requests(&mut self) -> Entity<PullRequestsView> {
                    let view = self
                        .shell
                        .read_with(self.cx, |s, _| s.pull_requests.clone());
                    view.expect("connected")
                }

                fn show_pull_requests(&mut self) {
                    let shell = self.shell.clone();
                    self.cx.update(|_, cx| {
                        shell.update(cx, |shell, cx| shell.show_pull_requests(cx));
                    });
                    self.settle();
                }
            }

            mod triggers {
                use super::*;

                #[gpui::test]
                fn should_alert_when_checks_turn_failure(cx: &mut TestAppContext) {
                    let web = |checks| pr(5, "acme/web", checks);
                    let mut open = Open::new(holding(vec![web(ChecksState::Pending)]), cx);

                    open.sync(vec![web(ChecksState::Failure)]);

                    let shown = open.shown();
                    assert_eq!(shown.len(), 1, "{shown:?}");
                    assert_eq!(shown[0].tag, tag(5, "acme/web"));
                    assert_eq!(shown[0].title, "Checks failed");
                    assert_eq!(shown[0].body, "acme/web #5 · PR 5");
                }

                #[gpui::test]
                fn should_alert_when_a_review_turns_approved_or_changes_requested(
                    cx: &mut TestAppContext,
                ) {
                    let reviewed = |n, review| PullRequest {
                        review,
                        ..pull_request(n, "acme/web")
                    };
                    let required = Some(ReviewState::ReviewRequired);
                    let mut open = Open::new(
                        holding(vec![reviewed(5, required), reviewed(6, required)]),
                        cx,
                    );

                    open.sync(vec![
                        reviewed(5, Some(ReviewState::Approved)),
                        reviewed(6, Some(ReviewState::ChangesRequested)),
                    ]);

                    assert_eq!(open.titles(), ["Approved", "Changes requested"]);
                }

                #[gpui::test]
                fn should_not_alert_for_a_pr_someone_else_authored(cx: &mut TestAppContext) {
                    let theirs = |checks| PullRequest {
                        author: Some("hubot".into()),
                        ..pr(5, "acme/web", checks)
                    };
                    let mut open = Open::new(holding(vec![theirs(ChecksState::Pending)]), cx);

                    open.sync(vec![theirs(ChecksState::Failure)]);

                    assert_eq!(open.shown(), []);
                }
            }

            mod one_per_pr {
                use super::*;

                #[gpui::test]
                fn should_replace_a_prs_alert_when_it_changes_again(cx: &mut TestAppContext) {
                    let web = |checks, review| PullRequest {
                        review,
                        ..pr(5, "acme/web", checks)
                    };
                    let mut open = Open::new(holding(vec![web(ChecksState::Pending, None)]), cx);

                    open.sync(vec![web(ChecksState::Failure, None)]);
                    open.sync(vec![web(
                        ChecksState::Failure,
                        Some(ReviewState::ChangesRequested),
                    )]);

                    assert_eq!(open.titles(), ["Checks failed", "Changes requested"]);
                    let tags: Vec<_> = open.shown().into_iter().map(|n| n.tag).collect();
                    assert_eq!(tags, [tag(5, "acme/web"), tag(5, "acme/web")]);
                    assert_eq!(open.notifier.open(), [tag(5, "acme/web")]);
                }
            }

            mod click {
                use super::*;

                /// A shell whose PR 5 of `acme/web` was merged, Finished, in a collapsed
                /// group, with Show finished off, as its checks turned `failure`.
                fn finished_and_failed(cx: &mut TestAppContext) -> Open {
                    let merged = |checks| PullRequest {
                        state: agentdeck_core::PrState::Merged,
                        finished: true,
                        ..pr(5, "acme/web", checks)
                    };
                    let mut open = Open::new(
                        holding(vec![
                            pr(1, "acme/web", ChecksState::Success),
                            merged(ChecksState::Pending),
                        ]),
                        cx,
                    );
                    open.cx.update(|_, cx| {
                        gui_state::update(cx, |state| {
                            state
                                .collapsed
                                .set_pull_requests(repository_id("acme/web"), true);
                        });
                    });
                    open.sync(vec![
                        pr(1, "acme/web", ChecksState::Success),
                        merged(ChecksState::Failure),
                    ]);
                    open
                }

                fn collapsed(open: &mut Open) -> std::collections::BTreeSet<RepositoryId> {
                    open.cx.update(|_, cx| {
                        gui_state::read(cx, |state| state.collapsed.pull_requests.clone())
                    })
                }

                #[gpui::test]
                fn should_open_the_pull_requests_view_at_the_prs_row(cx: &mut TestAppContext) {
                    let mut open = finished_and_failed(cx);
                    open.cx.deactivate_window();

                    open.click(&tag(5, "acme/web"));

                    assert!(open.is_active());
                    assert_eq!(open.selection(), Some(Selection::PullRequests));
                    let view = open.pull_requests();
                    let (rows, highlighted) = view.read_with(open.cx, |view, cx| {
                        let groups = view.groups(cx);
                        let rows: Vec<u32> = (groups.iter().flat_map(|g| &g.prs))
                            .map(|pr| pr.number)
                            .collect();
                        (rows, view.highlighted())
                    });
                    assert_eq!(rows, [1, 5], "the Finished row shows");
                    assert_eq!(highlighted, Some(key(5, "acme/web")));
                }

                #[gpui::test]
                fn should_open_its_group_and_leave_show_finished_off(cx: &mut TestAppContext) {
                    let mut open = finished_and_failed(cx);

                    open.click(&tag(5, "acme/web"));

                    assert_eq!(collapsed(&mut open), Default::default());
                    let show_finished = open
                        .cx
                        .update(|_, cx| gui_state::read(cx, |s| s.show_finished));
                    assert!(!show_finished);
                }

                #[gpui::test]
                fn should_hide_the_finished_row_again_once_the_view_is_left(
                    cx: &mut TestAppContext,
                ) {
                    let mut open = finished_and_failed(cx);
                    open.click(&tag(5, "acme/web"));

                    let shell = open.shell.clone();
                    open.cx.update(|window, cx| {
                        shell.update(cx, |shell, cx| shell.show_workspace(id(1), window, cx));
                    });
                    open.settle();

                    let view = open.pull_requests();
                    let groups = view.read_with(open.cx, |view, cx| view.groups(cx));
                    let rows: Vec<u32> = (groups.iter().flat_map(|g| &g.prs))
                        .map(|pr| pr.number)
                        .collect();
                    assert_eq!(rows, [1]);
                }

                #[gpui::test]
                fn should_open_the_pull_requests_view_for_several_prs(cx: &mut TestAppContext) {
                    let web = |n, checks| pr(n, "acme/web", checks);
                    let mut open = Open::new(
                        holding((1..=4).map(|n| web(n, ChecksState::Pending)).collect()),
                        cx,
                    );
                    open.sync((1..=4).map(|n| web(n, ChecksState::Failure)).collect());
                    assert_eq!(open.titles(), ["4 PRs changed"]);

                    open.click("prs");

                    assert_eq!(open.selection(), Some(Selection::PullRequests));
                }
            }

            mod withdraw {
                use super::*;

                #[gpui::test]
                fn should_withdraw_a_pr_alert_once_the_pull_requests_view_shows(
                    cx: &mut TestAppContext,
                ) {
                    let web = |checks| pr(5, "acme/web", checks);
                    let mut open = Open::new(holding(vec![web(ChecksState::Pending)]), cx);
                    open.sync(vec![web(ChecksState::Failure)]);
                    assert_eq!(open.notifier.open(), [tag(5, "acme/web")]);

                    open.show_pull_requests();

                    assert_eq!(open.notifier.open(), Vec::<SharedString>::new());
                }

                #[gpui::test]
                fn should_not_alert_while_the_pull_requests_view_shows(cx: &mut TestAppContext) {
                    let web = |checks| pr(5, "acme/web", checks);
                    let mut open = Open::new(holding(vec![web(ChecksState::Pending)]), cx);
                    open.show_pull_requests();

                    open.sync(vec![web(ChecksState::Failure)]);

                    assert_eq!(open.shown(), []);
                }
            }

            mod muting {
                use super::*;

                /// The alerts for `acme/web` #5's checks failing, with `acme/web` linked by
                /// `linking`, and each workspace of `set` muted and archived as it says.
                fn alerts(
                    linking: &[u32],
                    set: &[(u32, bool, bool)],
                    cx: &mut TestAppContext,
                ) -> Vec<agentdeck_notify::Notification> {
                    let web = |checks| pr(5, "acme/web", checks);
                    let api = holding(vec![web(ChecksState::Pending)]);
                    let mut snapshot = api.snapshot_now();
                    for (n, muted, archived) in set {
                        let w = (snapshot.workspaces.iter_mut())
                            .find(|w| w.id == id(*n))
                            .expect("a workspace");
                        w.muted = *muted;
                        w.archived_at = archived.then(|| "2026-11-20T00:00:00Z".into());
                    }
                    let linking: Vec<WorkspaceId> = linking.iter().map(|n| id(*n)).collect();
                    snapshot.repositories = vec![links("acme/web", &linking)];
                    api.set(snapshot);
                    let mut open = Open::new(api, cx);
                    open.sync(vec![web(ChecksState::Failure)]);
                    open.shown()
                }

                #[gpui::test]
                fn should_mute_when_every_linking_workspace_is_muted(cx: &mut TestAppContext) {
                    let shown = alerts(&[1, 2], &[(1, true, false), (2, true, false)], cx);

                    assert_eq!(shown, []);
                }

                #[gpui::test]
                fn should_alert_while_one_linking_workspace_is_unmuted(cx: &mut TestAppContext) {
                    let shown = alerts(&[1, 2], &[(1, true, false)], cx);

                    assert_eq!(shown.len(), 1);
                }

                #[gpui::test]
                fn should_alert_when_only_archived_muted_workspaces_link_it(
                    cx: &mut TestAppContext,
                ) {
                    let shown = alerts(&[1], &[(1, true, true)], cx);

                    assert_eq!(shown.len(), 1);
                }

                #[gpui::test]
                fn should_alert_for_a_repository_linked_nowhere(cx: &mut TestAppContext) {
                    let shown = alerts(&[], &[(1, true, false), (2, true, false)], cx);

                    assert_eq!(shown.len(), 1);
                }
            }
        }

        /// Frames drawn while only the sidebar redraws, as its footer's tick makes it.
        mod sidebar_redraws {
            use super::*;
            use crate::state_dot::tests::{play_frame, redraws_in_one_second};

            impl Open {
                /// Redraws the sidebar, and nothing else, on each of a second's 60 frames.
                fn redraw_the_sidebar(&mut self) {
                    let sidebar = self.shell.read_with(self.cx, |s, _| s.sidebar.clone());
                    let sidebar = sidebar.expect("connected");
                    for _ in 0..60 {
                        sidebar.update(self.cx, |_, cx| cx.notify());
                        play_frame(self.cx);
                    }
                }
            }

            /// Counts each redraw `view` asks for in `redraws`.
            fn count<V: 'static>(
                view: &Entity<V>,
                redraws: &Rc<std::cell::Cell<usize>>,
                cx: &mut App,
            ) -> Subscription {
                let redraws = redraws.clone();
                cx.observe(view, move |_, _| redraws.set(redraws.get() + 1))
            }

            /// The view that follows the sidebar's selection and snapshot hears of neither
            /// changing.
            #[gpui::test]
            fn should_leave_the_views_that_follow_it_alone(cx: &mut TestAppContext) {
                let mut open = Open::new(daemon(), cx);
                let view = open
                    .shell
                    .read_with(open.cx, |s, _| s.pull_requests.clone());
                let view = view.expect("connected");
                let redraws = Rc::new(std::cell::Cell::new(0));
                let _observing = open.cx.update(|_, cx| count(&view, &redraws, cx));
                open.redraw_the_sidebar();
                assert_eq!(redraws.get(), 0, "Pull requests view redraws");
            }

            /// No mark turns: a working agent's, in the sidebar or a pane header, is drawn
            /// still (spec §11).
            #[gpui::test]
            fn should_hold_the_working_agents_marks_still(cx: &mut TestAppContext) {
                let mut open = Open::new(daemon(), cx);
                open.turn(7, AgentState::Working);
                let sidebar = open.shell.read_with(open.cx, |s, _| s.sidebar.clone());
                let panes = open.shell.read_with(open.cx, |s, cx| {
                    (s.workspace.as_ref()).map_or_else(Vec::new, |v| v.read(cx).shown())
                });
                assert!(!panes.is_empty());
                assert_eq!(
                    redraws_in_one_second(&sidebar.expect("connected"), open.cx),
                    0
                );
                for pane in panes {
                    assert_eq!(redraws_in_one_second(&pane, open.cx), 0);
                }
            }
        }
    }

    mod quitting {
        use agentdeck_client::types::{GuiSettings, Settings};
        use agentdeck_core::{Agent, AgentState};
        use std::time::Duration;

        use gpui::{Modifiers, point, px};

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
            remote: Remote,
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
                    remote,
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
            let shown = harness
                .shell
                .read_with(cx, |shell, _| shell.quit_running.len());
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

        mod after_dont_ask_again {
            use agentdeck_client::ws::Event;
            use agentdeck_core::ws::{Entity as Changed, ServerMessage};

            use super::*;

            /// Quits with Don't ask again ticked, so the next quit doesn't ask.
            fn dont_ask_again(harness: &Harness, cx: &mut VisualTestContext) {
                harness.press_quit(cx);
                harness.click("quit dont ask again", cx);
                cx.simulate_keystrokes("enter");
                harness.settle(cx);
                assert!(!harness.api.settings_now().gui.confirm_quit);
            }

            /// Opens Settings and flips "Ask before quitting while agents run".
            fn flip_the_switch(harness: &Harness, cx: &mut VisualTestContext) {
                let sidebar = harness
                    .shell
                    .read_with(cx, |shell, _| shell.sidebar.clone().unwrap());
                sidebar.update(cx, |sidebar, cx| sidebar.select(Selection::Settings, cx));
                harness.settle(cx);
                // General is last, under the fold.
                let settings = harness
                    .shell
                    .read_with(cx, |shell, _| shell.settings.clone().unwrap());
                settings.update(cx, |settings, cx| settings.scroll_to_bottom(cx));
                harness.settle(cx);
                harness.click("settings confirm quit", cx);
            }

            #[gpui::test]
            fn should_ask_again_once_the_settings_switch_turns_it_back_on(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(daemon(vec![agent(1)], true), cx);
                dont_ask_again(&harness, cx);

                flip_the_switch(&harness, cx);
                harness.press_quit(cx);

                assert!(harness.asking(cx));
                assert_eq!(harness.quits(cx), 1);
            }

            #[gpui::test]
            fn should_ask_again_once_the_switch_turns_it_back_on_after_reopening(
                cx: &mut TestAppContext,
            ) {
                let (harness, cx) = Harness::open(daemon(vec![agent(1)], false), cx);

                flip_the_switch(&harness, cx);
                harness.press_quit(cx);

                assert!(harness.asking(cx));
                assert_eq!(harness.quits(cx), 0);
            }

            #[gpui::test]
            fn should_ask_again_once_another_client_turns_it_back_on(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(daemon(vec![agent(1)], true), cx);
                dont_ask_again(&harness, cx);

                harness.api.set_settings(default_settings());
                (harness.remote.events)
                    .try_send(Event::Message(ServerMessage::Changed {
                        entity: Changed::Settings,
                        id: "settings".into(),
                    }))
                    .unwrap();
                harness.settle(cx);
                harness.press_quit(cx);

                assert!(harness.asking(cx));
                assert_eq!(harness.quits(cx), 1);
            }
        }
        #[gpui::test]
        fn should_stop_each_running_agent_then_quit_with_stop_the_agents_ticked(
            cx: &mut TestAppContext,
        ) {
            let panes = vec![agent(1), hosting_shell(2), plain_shell(3)];
            let (harness, cx) = Harness::open(daemon(panes, true), cx);
            harness.press_quit(cx);

            harness.click("quit stop agents", cx);
            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(*harness.api.stopped.lock().unwrap(), [id(1), id(2)]);
            assert_eq!(harness.quits(cx), 1);
        }

        #[gpui::test]
        fn should_tick_stop_the_agents_from_its_label_too(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(vec![agent(1), agent(2)], true), cx);
            harness.press_quit(cx);
            let checkbox = cx.debug_bounds("quit stop agents").expect("the checkbox");

            // The label sits right of the checkbox, on its row.
            let label = point(checkbox.right() + px(40.), checkbox.center().y);
            cx.simulate_click(label, Modifiers::default());
            harness.settle(cx);

            let ticked = harness.shell.read_with(cx, |shell, _| shell.stop_agents);
            assert!(ticked);
        }

        #[gpui::test]
        fn should_leave_the_agents_running_with_stop_the_agents_unticked(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(vec![agent(1)], true), cx);
            harness.press_quit(cx);

            harness.click("quit stop agents", cx);
            harness.click("quit stop agents", cx);
            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert!(harness.api.stopped.lock().unwrap().is_empty());
            assert_eq!(harness.quits(cx), 1);
        }

        #[gpui::test]
        fn should_untick_stop_the_agents_each_time_it_opens(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(vec![agent(1)], true), cx);
            harness.press_quit(cx);
            harness.click("quit stop agents", cx);
            cx.simulate_keystrokes("escape");
            harness.settle(cx);

            harness.press_quit(cx);

            let ticked = harness.shell.read_with(cx, |shell, _| shell.stop_agents);
            assert!(!ticked);
        }

        #[gpui::test]
        fn should_wait_for_the_stops_up_to_the_kill_grace_then_quit(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(vec![agent(1)], true), cx);
            let _held = harness.api.hold_stop();
            harness.press_quit(cx);
            harness.click("quit stop agents", cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);
            assert_eq!(*harness.api.stopped.lock().unwrap(), [id(1)]);
            cx.executor()
                .advance_clock(crate::quit::STOP_GRACE - Duration::from_millis(1));
            harness.settle(cx);
            assert_eq!(harness.quits(cx), 0, "it waits for the stop");

            cx.executor().advance_clock(Duration::from_millis(1));
            harness.settle(cx);
            assert_eq!(harness.quits(cx), 1, "it quits once the grace is over");
        }

        #[gpui::test]
        fn should_quit_once_a_held_stop_lands(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(daemon(vec![agent(1)], true), cx);
            let release = harness.api.hold_stop();
            harness.press_quit(cx);
            harness.click("quit stop agents", cx);
            cx.simulate_keystrokes("enter");
            harness.settle(cx);
            assert_eq!(harness.quits(cx), 0);

            release.send(()).unwrap();
            harness.settle(cx);

            assert_eq!(harness.quits(cx), 1);
        }

        #[gpui::test]
        fn should_log_a_stop_that_fails_and_quit_anyway(cx: &mut TestAppContext) {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("agentdeck-gui.log");
            let (subscriber, _) = crate::logging::subscriber(Some(&path), Some("info"));
            let _logging = tracing::subscriber::set_default(subscriber);
            let (harness, cx) = Harness::open(daemon(vec![agent(1), agent(2)], true), cx);
            harness.press_quit(cx);
            harness.click("quit stop agents", cx);

            harness.api.fail_next("the daemon went away");
            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(*harness.api.stopped.lock().unwrap(), [id(2)]);
            assert_eq!(harness.quits(cx), 1);
            let log = std::fs::read_to_string(&path).unwrap();
            let line = (log.lines())
                .find(|line| line.contains("couldn't stop the agent on quit"))
                .unwrap_or_else(|| panic!("no failed stop in {log}"));
            assert!(line.contains("WARN"), "{line}");
            assert!(line.contains(&id::<PaneId>(1).to_string()), "{line}");
            assert!(line.contains("the daemon went away"), "{line}");
        }

        #[gpui::test]
        fn should_stop_this_quits_agents_only_when_dont_ask_again_is_ticked_too(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(daemon(vec![agent(1)], true), cx);
            harness.press_quit(cx);
            harness.click("quit dont ask again", cx);
            harness.click("quit stop agents", cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);
            assert_eq!(*harness.api.stopped.lock().unwrap(), [id(1)]);
            assert!(!harness.api.settings_now().gui.confirm_quit);

            harness.press_quit(cx);

            assert_eq!(harness.quits(cx), 2);
            assert_eq!(
                *harness.api.stopped.lock().unwrap(),
                [id(1)],
                "nothing remembers Stop the agents"
            );
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
