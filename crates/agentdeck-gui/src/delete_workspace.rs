//! The Delete workspace dialog (spec §11), Penpot "Screens" → "Delete workspace": what
//! deleting a workspace loses, a field to type its name into, and Archive instead.
//!
//! It reads the workspace's dirty report when it opens and lists each repo holding work on no
//! remote, then says what else deleting does: the running panes it stops and the tasks it
//! moves to the Inbox. Delete workspace stays disabled until the name is typed exactly; then it
//! deletes, dirty or not, since the user has seen the report. It emits [`DeleteEvent`] for the window.

use std::path::Path;
use std::sync::Arc;

use agentdeck_client::types::{DirtyRepo, DirtyReport, UnpushedBranch};
use agentdeck_core::{AgentState, WorkspaceId};
use brrritscold_core_gpui::{
    ActiveTheme as _, Alert, AlertDescription, AlertTitle, AlertVariant, Button, ButtonVariant,
    Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHandle,
    DialogHeader, DialogTitle, Field, Icon, IconName, Input, InputState, Label,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, Window,
    div,
};
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::notice;
use crate::overview::Snapshot;
use crate::workspace::failed;

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
    /// Its dir, which repo paths are shown under.
    pub dir: SharedString,
    /// Whether it is archived already: then there's no Archive instead.
    pub archived: bool,
}

/// What deleting a workspace does beyond its folder, counted when the dialog opens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Consequences {
    /// Its panes whose process hasn't exited, which deleting stops: what Archive refuses on.
    pub panes: usize,
    /// Its tasks, which move to the Inbox.
    pub tasks: usize,
}

impl Consequences {
    /// Counts `workspace`'s in `snapshot`.
    pub fn of(workspace: WorkspaceId, snapshot: &Snapshot) -> Self {
        Consequences {
            panes: (snapshot.panes.iter())
                .filter(|p| p.workspace == workspace && p.state != AgentState::Exited)
                .count(),
            tasks: (snapshot.tasks.iter())
                .filter(|t| t.workspace_id == Some(workspace))
                .count(),
        }
    }
}

/// Where the dirty report stands.
#[derive(Debug, Clone, PartialEq)]
enum Dirty {
    /// Reading it.
    Checking,
    /// The read failed; why.
    Failed(SharedString),
    /// What it says.
    Read(DirtyReport),
}

/// The dialog, kept by the window and opened from the top bar's menu.
pub struct DeleteWorkspaceDialog {
    api: Arc<dyn Api>,
    handle: DialogHandle,
    target: Option<DeleteTarget>,
    /// What deleting stops, moves and deletes.
    consequences: Consequences,
    dirty: Dirty,
    confirm: Entity<InputState>,
    /// Whether a delete is out; another waits for it.
    deleting: bool,
    /// The dirty report's read; a new open drops the last one.
    reading: Option<Task<()>>,
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
            dirty: Dirty::Checking,
            confirm,
            deleting: false,
            reading: None,
            focus_confirm: false,
            _enter,
        }
    }

    /// Opens it on `target`, counting what stops, moves and goes from `snapshot`, and starts reading
    /// the dirty report. The name field starts empty and takes the keyboard.
    pub fn open(
        &mut self,
        target: DeleteTarget,
        snapshot: &Snapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = target.id;
        self.consequences = Consequences::of(workspace, snapshot);
        self.confirm.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_placeholder(target.name.clone(), window, cx);
        });
        self.target = Some(target);
        self.dirty = Dirty::Checking;
        let reading = self.api.dirty(workspace, cx);
        self.reading = Some(cx.spawn_in(window, async move |this, cx| {
            let read = reading.await;
            let _ = this.update(cx, |this, cx| {
                this.dirty = match read {
                    Ok(report) => Dirty::Read(report),
                    Err(err) => Dirty::Failed(notice::message(&err)),
                };
                cx.notify();
            });
        }));
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

    /// What the dirty report says: a muted line while it reads or when it fails; when repos
    /// are dirty, Penpot's destructive `Alert` over a `RepoCard` for each.
    fn dirty_report(&self, cx: &App) -> Option<AnyElement> {
        let s = cx.theme().semantic();
        let muted = |selector: &'static str, text: SharedString| {
            div()
                .debug_selector(move || selector.into())
                .text_color(s.muted_foreground)
                .child(text)
                .into_any_element()
        };
        let dir = self.target.as_ref().map_or("".into(), |t| t.dir.clone());
        match &self.dirty {
            Dirty::Checking => Some(muted("checking", "Checking for unpushed work…".into())),
            Dirty::Failed(why) => Some(muted(
                "check failed",
                format!("Couldn't check for unpushed work: {why}").into(),
            )),
            Dirty::Read(report) if report.repos.is_empty() => None,
            Dirty::Read(report) => Some(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::S3)
                    .child(
                        div().debug_selector(|| "dirty alert".into()).child(
                            Alert::new()
                                .variant(AlertVariant::Destructive)
                                .icon(IconName::TriangleExclamationPoint)
                                .title(AlertTitle::new(alert_title(report.repos.len())))
                                .description(AlertDescription::new(
                                    "Deleting removes the folder and everything in it.",
                                )),
                        ),
                    )
                    .children(report.repos.iter().map(|repo| repo_card(repo, &dir, cx)))
                    .into_any_element(),
            ),
        }
    }
}

/// Penpot `RepoCard`: a folder icon and the repo's name over a line per kind of unpushed
/// work, in a `border` box with `radius-md`.
fn repo_card(repo: &DirtyRepo, dir: &str, cx: &App) -> AnyElement {
    let s = cx.theme().semantic();
    let name = repo_name(&repo.path, dir);
    let line = |icon: IconName, text: String| {
        div()
            .flex()
            .items_center()
            .gap(space::S1_5)
            .text_size(font_size::XS)
            .text_color(s.muted_foreground)
            .child(Icon::new(icon).size(space::S3).text_color(s.foreground))
            .child(text)
    };
    div()
        .debug_selector({
            let name = name.clone();
            move || format!("repo {name}")
        })
        .flex()
        .flex_col()
        .gap(space::S1)
        .px(space::S3)
        .py(space::S2)
        .rounded(s.radius_md)
        .border_1()
        .border_color(s.border)
        .child(
            div()
                .flex()
                .items_center()
                .gap(space::S2)
                .font_weight(font_weight::MEDIUM)
                .text_color(s.foreground)
                .child(Icon::new(IconName::Folder).size(space::S4))
                .child(name),
        )
        .when(!repo.uncommitted.is_empty(), |card| {
            card.child(line(
                IconName::TriangleExclamationPoint,
                uncommitted_line(&repo.uncommitted),
            ))
        })
        .children(
            (repo.unpushed_branches.iter())
                .map(|branch| line(IconName::GitBranch, branch_line(branch))),
        )
        .into_any_element()
}

/// "feat/tabs: 2 commits not pushed": a branch and how many of its commits are on no remote.
pub fn branch_line(branch: &UnpushedBranch) -> String {
    match branch.commits {
        1 => format!("{}: 1 commit not pushed", branch.name),
        n => format!("{}: {n} commits not pushed", branch.name),
    }
}

/// The alert's title for `repos` dirty repos.
pub fn alert_title(repos: usize) -> String {
    match repos {
        1 => "1 repo has work that isn't pushed".to_owned(),
        n => format!("{n} repos have work that isn't pushed"),
    }
}

/// What deleting does to the workspace's running panes and tasks, panes first; nothing when it
/// has neither (Penpot `Consequence`).
pub fn consequence_line(consequences: Consequences) -> Option<String> {
    let Consequences { panes, tasks } = consequences;
    let panes = match panes {
        0 => None,
        1 => Some("Its 1 running pane will be stopped.".to_owned()),
        n => Some(format!("Its {n} running panes will be stopped.")),
    };
    let tasks = match tasks {
        0 => None,
        1 => Some("Its 1 task moves to the Inbox.".to_owned()),
        n => Some(format!("Its {n} tasks move to the Inbox.")),
    };
    let parts: Vec<String> = [panes, tasks].into_iter().flatten().collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// The name a repo at `repo` goes by: its path under the workspace's `dir`, or its folder's
/// name when it is the workspace's dir itself or outside it.
pub fn repo_name(repo: &str, dir: &str) -> String {
    let repo = Path::new(repo);
    match repo.strip_prefix(dir) {
        Ok(rest) if !rest.as_os_str().is_empty() => rest.display().to_string(),
        _ => repo.file_name().map_or_else(
            || repo.display().to_string(),
            |n| n.to_string_lossy().into(),
        ),
    }
}

/// "3 uncommitted files in packages/core-react": how many `files` changed, and the folder
/// they share inside the repo, if they share one.
pub fn uncommitted_line(files: &[String]) -> String {
    let count = match files.len() {
        1 => "1 uncommitted file".to_owned(),
        n => format!("{n} uncommitted files"),
    };
    let folders = files.iter().map(|file| {
        let mut parts: Vec<&str> = file.split('/').collect();
        parts.pop();
        parts
    });
    let shared = folders.reduce(|shared, folder| {
        let same = shared
            .iter()
            .zip(&folder)
            .take_while(|(a, b)| a == b)
            .count();
        shared[..same].to_vec()
    });
    match shared {
        Some(shared) if !shared.is_empty() => format!("{count} in {}", shared.join("/")),
        _ => count,
    }
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
        let s = cx.theme().semantic();
        let can_delete = self.can_delete(cx);
        Dialog::new("delete-workspace")
            .handle(self.handle.clone())
            .content(
                DialogContent::new()
                    .header(
                        DialogHeader::new()
                            .title(DialogTitle::new(format!("Delete {name}?")))
                            .description(DialogDescription::new(
                                "This can't be undone. Archive it instead to keep its folder.",
                            )),
                    )
                    // Penpot `DeleteBody`: the report, what moves, then the field, `space.3`
                    // apart.
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(space::S3)
                            .children(self.dirty_report(cx))
                            .children(
                                consequence_line(self.consequences)
                                    .map(|line| div().text_color(s.muted_foreground).child(line)),
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
    use std::rc::Rc;

    use agentdeck_core::TaskKind;
    use brrritscold_core_gpui::Root;
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::notice;
    use crate::overview::fixtures::{id, task, workspace};

    struct Harness {
        api: Arc<FakeApi>,
        dialog: Entity<DeleteWorkspaceDialog>,
        events: Rc<RefCell<Vec<DeleteEvent>>>,
        _events: Subscription,
    }

    /// Workspace 1, `tabs-a11y` in `/w/tabs-a11y`, with two tasks.
    fn snapshot() -> Snapshot {
        let ws = workspace(1, "tabs-a11y");
        Snapshot {
            tasks: vec![
                task(10, TaskKind::Plain, Some(ws.id)),
                task(11, TaskKind::Pr, Some(ws.id)),
                task(12, TaskKind::Plain, None),
            ],
            workspaces: vec![ws],
            ..Snapshot::default()
        }
    }

    fn target() -> DeleteTarget {
        DeleteTarget {
            id: id(1),
            name: "tabs-a11y".into(),
            dir: "/w/tabs-a11y".into(),
            archived: false,
        }
    }

    /// Two dirty repos in workspace 1, as the design shows them.
    fn two_dirty_repos() -> DirtyReport {
        DirtyReport {
            repos: vec![
                DirtyRepo {
                    path: "/w/tabs-a11y/ts-packages".into(),
                    uncommitted: vec![
                        "packages/core-react/src/tabs.tsx".into(),
                        "packages/core-react/src/tabs.stories.tsx".into(),
                        "packages/core-react/src/tabs.keyboard.test.tsx".into(),
                    ],
                    unpushed_branches: vec![UnpushedBranch {
                        name: "feat/tabs-keyboard".into(),
                        commits: 2,
                    }],
                },
                DirtyRepo {
                    path: "/w/tabs-a11y/rust-packages".into(),
                    uncommitted: vec![],
                    unpushed_branches: vec![UnpushedBranch {
                        name: "fix/tab-focus-ring".into(),
                        commits: 1,
                    }],
                },
            ],
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

        /// Opens it on [`target`] without letting the dirty report's read land.
        fn open_only(&self, target: DeleteTarget, cx: &mut VisualTestContext) {
            cx.update(|window, cx| {
                self.dialog
                    .update(cx, |d, cx| d.open(target, &snapshot(), window, cx))
            });
        }

        /// Opens it on [`target`] and lets the dirty report land.
        fn open(&self, cx: &mut VisualTestContext) {
            self.open_only(target(), cx);
            self.settle(cx);
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
            harness.open_only(archived, cx);
            harness.settle(cx);

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

    mod dirty_report {
        use super::*;

        #[gpui::test]
        fn should_say_it_is_checking_until_the_report_lands(cx: &mut TestAppContext) {
            let api = Arc::new(FakeApi::with(snapshot()));
            api.set_dirty(id(1), two_dirty_repos());
            let (harness, cx) = Harness::new(api, cx);

            harness.open_only(target(), cx);
            cx.update(|window, cx| window.draw(cx).clear(cx));
            assert!(cx.debug_bounds("checking").is_some());

            harness.settle(cx);
            assert!(cx.debug_bounds("checking").is_none());
        }

        #[gpui::test]
        fn should_list_each_dirty_repo_under_an_alert(cx: &mut TestAppContext) {
            let api = Arc::new(FakeApi::with(snapshot()));
            api.set_dirty(id(1), two_dirty_repos());
            let (harness, cx) = Harness::new(api, cx);

            harness.open(cx);

            let alert = cx.debug_bounds("dirty alert").expect("an alert");
            let first = cx.debug_bounds("repo ts-packages").expect("ts-packages");
            let second = cx
                .debug_bounds("repo rust-packages")
                .expect("rust-packages");
            assert!(alert.top() < first.top() && first.top() < second.top());
        }

        #[gpui::test]
        fn should_show_no_alert_for_a_clean_workspace(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::new(Arc::new(FakeApi::with(snapshot())), cx);

            harness.open(cx);

            assert!(cx.debug_bounds("dirty alert").is_none());
            assert!(cx.debug_bounds("checking").is_none());
        }
    }

    mod alert_title {
        use super::*;

        #[test]
        fn should_count_the_repos() {
            assert_eq!(alert_title(1), "1 repo has work that isn't pushed");
            assert_eq!(alert_title(2), "2 repos have work that isn't pushed");
        }
    }

    mod consequence_line {
        use agentdeck_core::AgentState;

        use super::*;
        use crate::overview::fixtures::pane;

        fn line(panes: usize, tasks: usize) -> Option<String> {
            consequence_line(Consequences { panes, tasks })
        }

        #[test]
        fn should_say_where_the_tasks_go() {
            assert_eq!(
                line(0, 4).as_deref(),
                Some("Its 4 tasks move to the Inbox.")
            );
        }

        #[test]
        fn should_say_first_that_its_running_panes_will_be_stopped() {
            assert_eq!(
                line(3, 4).as_deref(),
                Some("Its 3 running panes will be stopped. Its 4 tasks move to the Inbox.")
            );
            assert_eq!(
                line(2, 0).as_deref(),
                Some("Its 2 running panes will be stopped.")
            );
        }

        #[test]
        fn should_speak_of_one_pane_or_task_in_the_singular() {
            assert_eq!(
                line(1, 1).as_deref(),
                Some("Its 1 running pane will be stopped. Its 1 task moves to the Inbox.")
            );
        }

        #[test]
        fn should_leave_out_what_it_has_none_of() {
            assert_eq!(line(0, 0), None);
        }

        #[gpui::test]
        fn should_count_the_workspaces_own_tasks(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::new(Arc::new(FakeApi::with(snapshot())), cx);

            harness.open(cx);

            let counted = harness.dialog.read_with(cx, |d, _| d.consequences);
            assert_eq!(counted.tasks, 2);
            assert_eq!(counted.panes, 0);
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

    mod repo_name {
        use super::*;

        #[test]
        fn should_be_its_path_under_the_workspace() {
            assert_eq!(repo_name("/w/x/ts-packages", "/w/x"), "ts-packages");
            assert_eq!(repo_name("/w/x/group/rust", "/w/x"), "group/rust");
        }

        #[test]
        fn should_be_the_folders_name_for_the_workspace_dir_itself() {
            assert_eq!(repo_name("/w/x", "/w/x"), "x");
            assert_eq!(repo_name("/w/x/", "/w/x"), "x");
        }
    }

    mod branch_line {
        use super::*;

        fn branch(name: &str, commits: u32) -> UnpushedBranch {
            UnpushedBranch {
                name: name.into(),
                commits,
            }
        }

        #[test]
        fn should_say_how_many_commits_are_not_pushed() {
            assert_eq!(
                branch_line(&branch("feat/tabs-keyboard", 2)),
                "feat/tabs-keyboard: 2 commits not pushed"
            );
        }

        #[test]
        fn should_speak_of_one_commit_in_the_singular() {
            assert_eq!(
                branch_line(&branch("fix/tab-focus-ring", 1)),
                "fix/tab-focus-ring: 1 commit not pushed"
            );
        }
    }

    mod uncommitted_line {
        use super::*;

        fn files(paths: &[&str]) -> Vec<String> {
            paths.iter().map(|p| (*p).to_owned()).collect()
        }

        #[test]
        fn should_name_the_folder_the_files_share() {
            let shared = files(&["packages/core-react/a.tsx", "packages/core-react/src/b.tsx"]);

            assert_eq!(
                uncommitted_line(&shared),
                "2 uncommitted files in packages/core-react"
            );
        }

        #[test]
        fn should_name_no_folder_when_they_share_none() {
            assert_eq!(
                uncommitted_line(&files(&["a.txt", "src/b.rs"])),
                "2 uncommitted files"
            );
        }

        #[test]
        fn should_name_a_lone_files_folder() {
            assert_eq!(
                uncommitted_line(&files(&["docs/spec.md"])),
                "1 uncommitted file in docs"
            );
        }

        #[test]
        fn should_not_take_a_shared_name_prefix_for_a_folder() {
            assert_eq!(
                uncommitted_line(&files(&["src-a/x.rs", "src-b/y.rs"])),
                "2 uncommitted files"
            );
        }
    }
}
