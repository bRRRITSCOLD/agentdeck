//! The Delete workspace dialog (spec §11), Penpot "Screens" → "Delete workspace": what
//! deleting a workspace does, a field to type its name into, and Archive instead.
//!
//! It says what deleting does when the workspace has any: its Repositories are unlinked and
//! its running panes stopped. No folder is touched: a workspace has none. Delete workspace
//! stays disabled until the name is typed exactly. Once deleted, the GUI forgets what it kept
//! of the workspace in its own state ([`gui_state`]). It emits [`DeleteEvent`] for the window.

use std::sync::Arc;

use agentdeck_core::{AgentState, WorkspaceId};
use brrritscold_core_gpui::{
    ActiveTheme as _, Button, ButtonVariant, Dialog, DialogClose, DialogContent, DialogDescription,
    DialogFooter, DialogHandle, DialogHeader, DialogTitle, Field, Input, InputState, Label,
};
use brrritscold_core_gpui_tokens::primitive::space;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
};
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::gui_state;
use crate::overview::Snapshot;
use crate::workspace::failed;

/// The dialog's description, under its title.
pub const DESCRIPTION: &str =
    "This can't be undone. Archive it instead to keep its panes and linked Repositories.";

/// What the dialog asks of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteEvent {
    /// Archive instead was pressed: archive the workspace.
    ArchiveInstead(WorkspaceId),
    /// The workspace was deleted.
    Deleted(WorkspaceId),
}

/// The workspace the dialog would delete.
#[derive(Debug, Clone, PartialEq)]
pub struct DeleteTarget {
    /// Its id.
    pub id: WorkspaceId,
    /// Its name, which the user types to confirm.
    pub name: SharedString,
    /// Whether it is archived already: then there's no Archive instead.
    pub archived: bool,
}

/// What deleting a workspace does, counted when the dialog opens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Consequences {
    /// The Repositories it links, which deleting unlinks.
    pub repositories: usize,
    /// Its panes whose process hasn't exited, which deleting stops: what Archive refuses on.
    pub panes: usize,
}

impl Consequences {
    /// Counts `workspace`'s in `snapshot`.
    pub fn of(workspace: WorkspaceId, snapshot: &Snapshot) -> Self {
        Consequences {
            repositories: (snapshot.repositories.iter())
                .filter(|repository| repository.workspaces.contains(&workspace))
                .count(),
            panes: (snapshot.panes.iter())
                .filter(|p| p.workspace == workspace && p.state != AgentState::Exited)
                .count(),
        }
    }
}

/// The dialog, kept by the window and opened from the top bar's menu.
pub struct DeleteWorkspaceDialog {
    api: Arc<dyn Api>,
    handle: DialogHandle,
    target: Option<DeleteTarget>,
    /// What deleting does.
    consequences: Consequences,
    confirm: Entity<InputState>,
    /// Whether a delete is out; another waits for it.
    deleting: bool,
    /// Whether the name field takes the keyboard once the dialog shows.
    focus_confirm: bool,
    _enter: Subscription,
}

impl EventEmitter<DeleteEvent> for DeleteWorkspaceDialog {}

impl DeleteWorkspaceDialog {
    /// A closed dialog that deletes workspaces through `api`.
    pub fn new(api: Arc<dyn Api>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let confirm = cx.new(|cx| InputState::new(window, cx));
        let _enter = cx.subscribe_in(&confirm, window, |this, _, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.delete(window, cx);
            }
        });
        DeleteWorkspaceDialog {
            api,
            handle: DialogHandle::new(false),
            target: None,
            consequences: Consequences::default(),
            confirm,
            deleting: false,
            focus_confirm: false,
            _enter,
        }
    }

    /// Opens it on `target`, counting what deleting does from `snapshot`. The name field
    /// starts empty and takes the keyboard.
    pub fn open(
        &mut self,
        target: DeleteTarget,
        snapshot: &Snapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.consequences = Consequences::of(target.id, snapshot);
        self.confirm.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_placeholder(target.name.clone(), window, cx);
        });
        self.target = Some(target);
        self.handle.open(window, cx);
        self.focus_confirm = true;
        cx.notify();
    }
    /// Whether it is open.
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.handle.is_open()
    }

    /// Whether the name typed lets Delete workspace go ahead: it is the workspace's, exactly.
    pub fn can_delete(&self, cx: &App) -> bool {
        let typed = self.confirm.read(cx).value();
        !self.deleting && self.target.as_ref().is_some_and(|t| typed == t.name)
    }

    /// Deletes the workspace once its name is typed: what Delete workspace and Enter do. On
    /// failure it stays open and toasts why.
    pub fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone().filter(|_| self.can_delete(cx)) else {
            return;
        };
        self.deleting = true;
        cx.notify();
        let deleting = self.api.delete_workspace(target.id, cx);
        cx.spawn_in(window, async move |this, cx| {
            let deleted = deleting.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.deleting = false;
                match deleted {
                    Ok(()) => {
                        // What the GUI kept of it would never be read again.
                        gui_state::forget_workspace(cx, target.id);
                        this.handle.close(window, cx);
                        cx.emit(DeleteEvent::Deleted(target.id));
                    }
                    Err(err) => {
                        failed(format!("Couldn't delete {}", target.name), &err, window, cx)
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Closes it and asks the window to archive the workspace instead.
    pub fn archive_instead(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = &self.target else {
            return;
        };
        let workspace = target.id;
        self.handle.close(window, cx);
        cx.emit(DeleteEvent::ArchiveInstead(workspace));
        cx.notify();
    }
}

/// What deleting does to the workspace's Links; nothing when it links no Repository
/// (Penpot `Consequence`).
pub fn repositories_line(repositories: usize) -> Option<String> {
    match repositories {
        0 => None,
        1 => Some("Its 1 Repository is unlinked. Its PRs stay in Pull requests.".to_owned()),
        n => Some(format!(
            "Its {n} Repositories are unlinked. Their PRs stay in Pull requests."
        )),
    }
}

/// What deleting does to the workspace's running panes; nothing when it has none (Penpot
/// `Consequence`).
pub fn panes_line(panes: usize) -> Option<String> {
    match panes {
        0 => None,
        1 => Some("Its 1 running pane will be stopped.".to_owned()),
        n => Some(format!("Its {n} running panes will be stopped.")),
    }
}

/// A muted line of the body that tests find as `selector`.
fn consequence(selector: &'static str, line: String, cx: &App) -> impl IntoElement {
    div()
        .debug_selector(move || selector.into())
        .text_color(cx.theme().semantic().muted_foreground)
        .child(line)
}

impl Render for DeleteWorkspaceDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_confirm) {
            // The dialog focuses itself in an effect it queues while drawing, after this
            // render; queueing from inside an effect lands after that one.
            let confirm = self.confirm.clone();
            window.defer(cx, move |window, cx| {
                window.defer(cx, move |window, cx| {
                    confirm.update(cx, |input, cx| input.focus(window, cx));
                });
            });
        }
        let (name, archived) =
            (self.target.as_ref()).map_or(("".into(), false), |t| (t.name.clone(), t.archived));
        let can_delete = self.can_delete(cx);
        Dialog::new("delete-workspace")
            .handle(self.handle.clone())
            .content(
                DialogContent::new()
                    .header(
                        DialogHeader::new()
                            .title(DialogTitle::new(format!("Delete {name}?")))
                            .description(DialogDescription::new(DESCRIPTION)),
                    )
                    // Penpot `DeleteBody`: what it unlinks, what stops, then the field,
                    // `space.3` apart.
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(space::S3)
                            .children(
                                repositories_line(self.consequences.repositories)
                                    .map(|line| consequence("repositories line", line, cx)),
                            )
                            .children(
                                panes_line(self.consequences.panes)
                                    .map(|line| consequence("panes line", line, cx)),
                            )
                            .child(
                                Field::new()
                                    .child(Label::new().child(format!("Type {name} to confirm")))
                                    .child(Input::new(&self.confirm)),
                            ),
                    )
                    // Penpot `agentdeck/DeleteFooterActions`.
                    .footer(
                        DialogFooter::new()
                            .when(!archived, |footer| {
                                footer.child(
                                    div().debug_selector(|| "archive instead".into()).child(
                                        Button::new("delete-workspace-archive")
                                            .variant(ButtonVariant::Outline)
                                            .label("Archive instead")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.archive_instead(window, cx);
                                            })),
                                    ),
                                )
                            })
                            .child(DialogClose::new(
                                Button::new("delete-workspace-cancel")
                                    .variant(ButtonVariant::Outline)
                                    .label("Cancel"),
                            ))
                            .child(
                                div().debug_selector(|| "delete workspace".into()).child(
                                    Button::new("delete-workspace-confirm")
                                        .variant(ButtonVariant::Destructive)
                                        .label("Delete workspace")
                                        .disabled(!can_delete)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.delete(window, cx);
                                        })),
                                ),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::rc::Rc;

    use agentdeck_core::RepositoryId;
    use brrritscold_core_gpui::Root;
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::gui_state::{self, GuiState, Side};
    use crate::notice;
    use crate::overview::fixtures::{id, workspace};

    struct Harness {
        api: Arc<FakeApi>,
        dialog: Entity<DeleteWorkspaceDialog>,
        events: Rc<RefCell<Vec<DeleteEvent>>>,
        _events: Subscription,
    }

    /// Workspace 1, `tabs-a11y`.
    fn snapshot() -> Snapshot {
        Snapshot {
            workspaces: vec![workspace(1, "tabs-a11y")],
            ..Snapshot::default()
        }
    }

    fn target() -> DeleteTarget {
        DeleteTarget {
            id: id(1),
            name: "tabs-a11y".into(),
            archived: false,
        }
    }

    impl Harness {
        /// A window holding the dialog over `api`; not opened yet.
        fn new(api: Arc<FakeApi>, cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            let mut dialog = None;
            let window = cx.add_window(|window, cx| {
                let api: Arc<dyn Api> = api.clone();
                let view = cx.new(|cx| DeleteWorkspaceDialog::new(api, window, cx));
                dialog = Some(view.clone());
                Root::new(view, window, cx)
            });
            let cx = VisualTestContext::from_window(*window, cx).into_mut();
            let dialog = dialog.unwrap();
            let events = Rc::new(RefCell::new(Vec::new()));
            let seen = events.clone();
            let _events = cx.update(|_, cx| {
                cx.subscribe(&dialog, move |_, event: &DeleteEvent, _| {
                    seen.borrow_mut().push(*event)
                })
            });
            let harness = Harness {
                api,
                dialog,
                events,
                _events,
            };
            (harness, cx)
        }

        /// Opens it on `target`, counting from `snapshot`.
        fn open_on(&self, target: DeleteTarget, snapshot: &Snapshot, cx: &mut VisualTestContext) {
            cx.update(|window, cx| {
                self.dialog
                    .update(cx, |d, cx| d.open(target, snapshot, window, cx))
            });
            self.settle(cx);
        }

        /// Opens it on [`target`].
        fn open(&self, cx: &mut VisualTestContext) {
            self.open_on(target(), &snapshot(), cx);
        }

        fn settle(&self, cx: &mut VisualTestContext) {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        fn type_name(&self, name: &str, cx: &mut VisualTestContext) {
            cx.update(|window, cx| {
                let input = self.dialog.read(cx).confirm.clone();
                input.update(cx, |input, cx| input.set_value(name.to_owned(), window, cx));
            });
            self.settle(cx);
        }

        fn can_delete(&self, cx: &mut VisualTestContext) -> bool {
            cx.update(|_, cx| self.dialog.read(cx).can_delete(cx))
        }

        fn delete(&self, cx: &mut VisualTestContext) {
            cx.update(|window, cx| self.dialog.update(cx, |d, cx| d.delete(window, cx)));
            self.settle(cx);
        }

        fn is_open(&self, cx: &mut VisualTestContext) -> bool {
            self.dialog.read_with(cx, |d, _| d.is_open())
        }

        fn deleted(&self) -> Vec<WorkspaceId> {
            self.api.deleted.lock().unwrap().clone()
        }
    }

    mod dialog {
        use super::*;

        #[gpui::test]
        fn should_keep_delete_disabled_until_the_exact_name_is_typed(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::new(Arc::new(FakeApi::with(snapshot())), cx);
            harness.open(cx);

            for almost in ["", "tabs-a11", "Tabs-a11y", "tabs-a11y "] {
                harness.type_name(almost, cx);
                assert!(!harness.can_delete(cx), "{almost:?}");
                harness.delete(cx);
            }
            assert_eq!(harness.deleted(), []);

            harness.type_name("tabs-a11y", cx);
            assert!(harness.can_delete(cx));
        }

        #[gpui::test]
        fn should_delete_once_the_name_is_typed_and_close(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::new(Arc::new(FakeApi::with(snapshot())), cx);
            harness.open(cx);
            harness.type_name("tabs-a11y", cx);

            harness.delete(cx);

            assert_eq!(harness.deleted(), [id(1)]);
            assert!(!harness.is_open(cx));
            assert_eq!(*harness.events.borrow(), [DeleteEvent::Deleted(id(1))]);
        }

        #[gpui::test]
        fn should_delete_on_enter(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::new(Arc::new(FakeApi::with(snapshot())), cx);
            harness.open(cx);
            cx.update(|window, cx| {
                let input = harness.dialog.read(cx).confirm.clone();
                input.update(cx, |input, cx| input.focus(window, cx));
            });

            cx.simulate_input("tabs-a11y");
            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(harness.deleted(), [id(1)]);
            assert!(!harness.is_open(cx));
            assert_eq!(*harness.events.borrow(), [DeleteEvent::Deleted(id(1))]);
        }

        #[gpui::test]
        fn should_stay_open_on_enter_before_the_name_is_typed(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::new(Arc::new(FakeApi::with(snapshot())), cx);
            harness.open(cx);
            cx.update(|window, cx| {
                let input = harness.dialog.read(cx).confirm.clone();
                input.update(cx, |input, cx| input.focus(window, cx));
            });

            cx.simulate_input("tabs-a11");
            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert!(harness.is_open(cx));
            assert_eq!(harness.deleted(), []);
            assert!(harness.events.borrow().is_empty());
        }

        #[gpui::test]
        fn should_stay_open_and_toast_when_the_delete_fails(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::new(Arc::new(FakeApi::with(snapshot())), cx);
            harness.open(cx);
            harness.type_name("tabs-a11y", cx);
            harness.api.fail_next("connection refused");

            harness.delete(cx);

            assert!(harness.is_open(cx));
            assert!(harness.events.borrow().is_empty());
            let shown = cx.update(|_, cx| notice::shown(cx));
            assert_eq!(shown.last().unwrap().title, "Couldn't delete tabs-a11y");
        }

        /// A side, a collapsed group and a dismissal for each of Workspaces `ns`, saved to a
        /// `gui.json` the GUI reads and writes; the temp dir holds it.
        fn remember(ns: &[u32], cx: &mut TestAppContext) -> (tempfile::TempDir, PathBuf) {
            let dir = tempfile::tempdir().expect("a temp dir");
            let path = dir.path().join(gui_state::FILE);
            gui_state::save(&path, &remembering(ns)).expect("save");
            cx.update(|cx| gui_state::init(Some(path.clone()), cx));
            (dir, path)
        }

        fn remembering(ns: &[u32]) -> GuiState {
            let mut state = GuiState::default();
            for &n in ns {
                state.set_side(id(n), Side::PullRequests);
                (state.collapsed).set_workspace(id(n), RepositoryId::from(7), true);
                state.dismiss(id(n), RepositoryId::from(8));
            }
            state
        }

        #[gpui::test]
        fn should_forget_what_the_gui_kept_of_the_deleted_workspace(cx: &mut TestAppContext) {
            let (_dir, path) = remember(&[1, 2], cx);
            let (harness, cx) = Harness::new(Arc::new(FakeApi::with(snapshot())), cx);
            harness.open(cx);
            harness.type_name("tabs-a11y", cx);

            harness.delete(cx);

            assert_eq!(gui_state::load(&path), remembering(&[2]));
            let kept = cx.update(|_, cx| gui_state::read(cx, GuiState::workspaces));
            assert_eq!(kept, BTreeSet::from([id(2)]));
        }

        #[gpui::test]
        fn should_forget_nothing_when_the_delete_fails(cx: &mut TestAppContext) {
            let (_dir, path) = remember(&[1, 2], cx);
            let (harness, cx) = Harness::new(Arc::new(FakeApi::with(snapshot())), cx);
            harness.open(cx);
            harness.type_name("tabs-a11y", cx);
            harness.api.fail_next("connection refused");

            harness.delete(cx);

            assert_eq!(gui_state::load(&path), remembering(&[1, 2]));
        }

        #[gpui::test]
        fn should_close_and_ask_for_an_archive_instead(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::new(Arc::new(FakeApi::with(snapshot())), cx);
            harness.open(cx);

            let button = cx.debug_bounds("archive instead").expect("shown");
            cx.simulate_click(button.center(), gpui::Modifiers::default());
            harness.settle(cx);

            assert!(!harness.is_open(cx));
            assert_eq!(
                *harness.events.borrow(),
                [DeleteEvent::ArchiveInstead(id(1))]
            );
            assert_eq!(harness.deleted(), []);
        }

        #[gpui::test]
        fn should_offer_no_archive_for_an_archived_workspace(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::new(Arc::new(FakeApi::with(snapshot())), cx);
            let archived = DeleteTarget {
                archived: true,
                ..target()
            };
            harness.open_on(archived, &snapshot(), cx);

            assert!(cx.debug_bounds("archive instead").is_none());
        }

        #[gpui::test]
        fn should_cancel_on_escape(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::new(Arc::new(FakeApi::with(snapshot())), cx);
            harness.open(cx);

            cx.simulate_keystrokes("escape");
            harness.settle(cx);

            assert!(!harness.is_open(cx));
            assert_eq!(harness.deleted(), []);
        }
    }

    mod consequences {
        use agentdeck_core::AgentState;

        use super::*;
        use crate::overview::fixtures::{links, pane};

        /// Workspace 1 linking widgets and gadgets, gadgets linked by workspace 2 too, with
        /// two running panes.
        fn linked_and_running() -> Snapshot {
            let (ws, other) = (id(1), id(2));
            Snapshot {
                repositories: vec![
                    links("acme/gadgets", &[ws, other]),
                    links("acme/tools", &[other]),
                    links("acme/widgets", &[ws]),
                ],
                panes: vec![
                    pane(40, ws, AgentState::Working),
                    pane(41, ws, AgentState::Idle),
                ],
                ..snapshot()
            }
        }

        #[gpui::test]
        fn should_say_its_repositories_are_unlinked_over_its_running_panes(
            cx: &mut TestAppContext,
        ) {
            let snapshot = linked_and_running();
            let (harness, cx) = Harness::new(Arc::new(FakeApi::with(snapshot.clone())), cx);

            harness.open_on(target(), &snapshot, cx);

            let repositories = cx
                .debug_bounds("repositories line")
                .expect("the Links line");
            let panes = cx.debug_bounds("panes line").expect("the panes line");
            assert!(repositories.top() < panes.top());
            assert_eq!(
                harness.dialog.read_with(cx, |d, _| d.consequences),
                Consequences {
                    repositories: 2,
                    panes: 2
                }
            );
        }

        #[gpui::test]
        fn should_say_nothing_of_repositories_or_panes_it_has_none_of(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::new(Arc::new(FakeApi::with(snapshot())), cx);

            harness.open(cx);

            assert!(cx.debug_bounds("repositories line").is_none());
            assert!(cx.debug_bounds("panes line").is_none());
        }

        #[test]
        fn should_offer_to_archive_to_keep_its_panes_and_repositories() {
            assert_eq!(
                DESCRIPTION,
                "This can't be undone. Archive it instead to keep its panes and linked \
                 Repositories."
            );
        }
    }

    mod repositories_line {
        use super::*;

        #[test]
        fn should_say_its_repositories_are_unlinked_and_their_prs_stay() {
            assert_eq!(
                repositories_line(2).as_deref(),
                Some("Its 2 Repositories are unlinked. Their PRs stay in Pull requests.")
            );
        }

        #[test]
        fn should_speak_of_one_repository_in_the_singular() {
            assert_eq!(
                repositories_line(1).as_deref(),
                Some("Its 1 Repository is unlinked. Its PRs stay in Pull requests.")
            );
        }

        #[test]
        fn should_say_nothing_when_it_links_none() {
            assert_eq!(repositories_line(0), None);
        }
    }

    mod panes_line {
        use agentdeck_core::AgentState;

        use super::*;
        use crate::overview::fixtures::pane;

        #[test]
        fn should_say_its_running_panes_will_be_stopped() {
            assert_eq!(
                panes_line(2).as_deref(),
                Some("Its 2 running panes will be stopped.")
            );
        }

        #[test]
        fn should_speak_of_one_pane_in_the_singular() {
            assert_eq!(
                panes_line(1).as_deref(),
                Some("Its 1 running pane will be stopped.")
            );
        }

        #[test]
        fn should_say_nothing_with_no_running_pane() {
            assert_eq!(panes_line(0), None);
        }

        #[gpui::test]
        fn should_count_the_workspaces_panes_whose_process_has_not_exited(cx: &mut TestAppContext) {
            let mut running = snapshot();
            let (ws, other) = (id(1), id(2));
            running.panes = vec![
                pane(40, ws, AgentState::Working),
                pane(41, ws, AgentState::Idle),
                pane(42, ws, AgentState::Unknown),
                pane(43, ws, AgentState::Exited),
                pane(44, other, AgentState::Working),
            ];
            let (harness, cx) = Harness::new(Arc::new(FakeApi::with(running.clone())), cx);

            cx.update(|window, cx| {
                harness
                    .dialog
                    .update(cx, |d, cx| d.open(target(), &running, window, cx))
            });
            harness.settle(cx);

            assert_eq!(harness.dialog.read_with(cx, |d, _| d.consequences.panes), 3);
        }
    }
}
