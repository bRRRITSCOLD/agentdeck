//! The new task dialog (spec §11), Penpot "Screens" → "New task": a title, an optional
//! description, and the workspace it goes to or the Inbox. It makes a `todo` task.
//!
//! The Tasks view's "New task" and the right panel's Tasks & PRs `+` ask the window for it
//! with [`NewTask`], preset to the Inbox or to that workspace. Create is disabled while the
//! title is blank; Enter in the title creates. Once made it closes, and the task comes back
//! through the daemon's `changed`; a failure keeps it open and toasts why.

use std::sync::Arc;

use agentdeck_core::WorkspaceId;
use brrritscold_core_gpui::{
    Button, ButtonVariant, Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter,
    DialogHandle, DialogHeader, DialogTitle, Field, Input, InputState, Label, Select,
    SelectContent, SelectItem, SelectTrigger, Textarea, TextareaState,
};
use brrritscold_core_gpui_tokens::primitive::space;
use gpui::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
};
use gpui_base::actions::Confirm;
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::overview::Snapshot;
use crate::workspace::failed;

/// A view asks the window for the dialog, preset to this workspace, or to the Inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewTask(pub Option<WorkspaceId>);

/// What the dialog asks the daemon to make: a `todo` task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The title, trimmed.
    pub title: String,
    /// The description, trimmed; empty for none.
    pub body: String,
    /// The workspace; `None` for the Inbox.
    pub workspace: Option<WorkspaceId>,
}

/// The Select's value for the Inbox.
const INBOX: &str = "inbox";

/// The dialog, kept by the window.
pub struct NewTaskDialog {
    api: Arc<dyn Api>,
    handle: DialogHandle,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    /// The unarchived workspaces offered, oldest first.
    workspaces: Vec<(WorkspaceId, SharedString)>,
    /// The workspace picked; `None` for the Inbox.
    workspace: Option<WorkspaceId>,
    /// Whether a request is out; another waits for it.
    creating: bool,
    /// Whether the title field takes the keyboard once the dialog shows.
    focus_title: bool,
    _title: Subscription,
}

impl NewTaskDialog {
    /// A closed dialog that makes tasks through `api`.
    pub fn new(api: Arc<dyn Api>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("What needs doing?"));
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Details for whoever picks it up")
                .auto_grow(2, 6)
        });
        let _title = cx.subscribe_in(&title, window, |this, _, event, window, cx| match event {
            InputEvent::PressEnter { .. } => this.create(window, cx),
            // Create follows whether the title is blank.
            InputEvent::Change => cx.notify(),
            _ => {}
        });
        NewTaskDialog {
            api,
            handle: DialogHandle::new(false),
            title,
            body,
            workspaces: Vec::new(),
            workspace: None,
            creating: false,
            focus_title: false,
            _title,
        }
    }

    /// Opens it empty, the title focused, offering `snapshot`'s unarchived workspaces and
    /// the Inbox, with `preset` picked; an archived or unknown one picks the Inbox.
    pub fn open(
        &mut self,
        preset: Option<WorkspaceId>,
        snapshot: &Snapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspaces = (snapshot.workspaces.iter())
            .filter(|w| w.archived_at.is_none())
            .map(|w| (w.id, SharedString::from(w.name.clone())))
            .collect();
        self.workspace = preset.filter(|p| self.workspaces.iter().any(|(id, _)| id == p));
        self.title
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.body
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.handle.open(window, cx);
        self.focus_title = true;
        cx.notify();
    }

    /// Whether it is open.
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.handle.is_open()
    }

    /// The workspace picked; `None` for the Inbox.
    #[cfg(test)]
    pub fn workspace(&self) -> Option<WorkspaceId> {
        self.workspace
    }

    /// Picks the workspace, or the Inbox.
    pub fn set_workspace(&mut self, workspace: Option<WorkspaceId>, cx: &mut Context<Self>) {
        self.workspace = workspace;
        cx.notify();
    }

    /// Whether Create task can go ahead: the title isn't blank and no request is out.
    pub fn can_create(&self, cx: &App) -> bool {
        !self.creating && !self.title.read(cx).value().trim().is_empty()
    }

    /// Makes the task the form describes: what Create task and Enter in the title do.
    pub fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_create(cx) {
            return;
        }
        let request = Request {
            title: self.title.read(cx).value().trim().to_owned(),
            body: self.body.read(cx).value().trim().to_owned(),
            workspace: self.workspace,
        };
        self.creating = true;
        cx.notify();
        let creating = self.api.new_task(request, cx);
        cx.spawn_in(window, async move |this, cx| {
            let made = creating.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.creating = false;
                match made {
                    Ok(_) => this.handle.close(window, cx),
                    Err(err) => failed("Couldn't make the task", &err, window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Penpot `agentdeck/NewTaskForm`: three vertical fields, `space.4` apart.
    fn form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let workspace = Select::new("new-task-workspace")
            .trigger(SelectTrigger::new().w_full())
            .content(self.workspaces.iter().fold(
                SelectContent::new().item(SelectItem::new(INBOX, "Inbox (no workspace)")),
                |c, (id, name)| c.item(SelectItem::new(id.to_string(), name.clone())),
            ))
            .value(Some(
                self.workspace
                    .map_or(SharedString::from(INBOX), |w| w.to_string().into()),
            ))
            .on_value_change(cx.listener(|this, value: &SharedString, _, cx| {
                this.set_workspace(value.parse().ok(), cx);
            }));
        div()
            .flex()
            .flex_col()
            .gap(space::S4)
            .child(
                Field::new().child(Label::new().child("Title")).child(
                    // The input passes Enter on as the dialog's Confirm, which would close it
                    // whatever `create` makes of it; Enter is `create`'s alone.
                    div()
                        .on_action(|_: &Confirm, _, _| {})
                        .child(Input::new(&self.title)),
                ),
            )
            .child(
                Field::new()
                    .child(Label::new().child("Description (optional)"))
                    .child(Textarea::new(&self.body)),
            )
            .child(
                Field::new()
                    .child(Label::new().child("Workspace"))
                    .child(workspace),
            )
    }
}

impl Render for NewTaskDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_title) {
            // The dialog focuses itself in an effect it queues while drawing, after this
            // render; queueing from inside an effect lands after that one.
            let title = self.title.clone();
            window.defer(cx, move |window, cx| {
                window.defer(cx, move |window, cx| {
                    title.update(cx, |input, cx| input.focus(window, cx));
                });
            });
        }
        let can_create = self.can_create(cx);
        Dialog::new("new-task").handle(self.handle.clone()).content(
            DialogContent::new()
                .header(
                    DialogHeader::new()
                        .title(DialogTitle::new("New task"))
                        .description(DialogDescription::new(
                            "Add a task to a workspace, or to the Inbox.",
                        )),
                )
                .child(self.form(cx))
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new(
                            Button::new("new-task-cancel")
                                .variant(ButtonVariant::Outline)
                                .label("Cancel"),
                        ))
                        .child(
                            div().debug_selector(|| "create task".into()).child(
                                Button::new("new-task-create")
                                    .label("Create task")
                                    .disabled(!can_create)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.create(window, cx);
                                    })),
                            ),
                        ),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use agentdeck_client::types::Workspace;
    use brrritscold_core_gpui::{Root, Theme, ThemePreference};
    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::notice;
    use crate::overview::fixtures::{id, workspace};

    struct Harness {
        api: Arc<FakeApi>,
        dialog: Entity<NewTaskDialog>,
    }

    /// Workspaces 1 `tabs-a11y` and 2 `gpui-terminal`, and 3 `old`, archived.
    fn snapshot() -> Snapshot {
        Snapshot {
            workspaces: vec![
                workspace(1, "tabs-a11y"),
                workspace(2, "gpui-terminal"),
                Workspace {
                    archived_at: Some("2026-11-12T01:00:00Z".into()),
                    ..workspace(3, "old")
                },
            ],
            ..Snapshot::default()
        }
    }

    impl Harness {
        /// A window holding the dialog, opened with `preset` picked.
        fn open(
            preset: Option<WorkspaceId>,
            cx: &mut TestAppContext,
        ) -> (Harness, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            let api = Arc::new(FakeApi::with(snapshot()));
            let mut dialog = None;
            let window = cx.add_window(|window, cx| {
                let api: Arc<dyn Api> = api.clone();
                let view = cx.new(|cx| NewTaskDialog::new(api, window, cx));
                dialog = Some(view.clone());
                Root::new(view, window, cx)
            });
            let cx = VisualTestContext::from_window(*window, cx).into_mut();
            let harness = Harness {
                api,
                dialog: dialog.unwrap(),
            };
            harness.reopen(preset, cx);
            (harness, cx)
        }

        fn reopen(&self, preset: Option<WorkspaceId>, cx: &mut VisualTestContext) {
            cx.update(|window, cx| {
                self.dialog
                    .update(cx, |d, cx| d.open(preset, &snapshot(), window, cx))
            });
            self.settle(cx);
        }

        fn settle(&self, cx: &mut VisualTestContext) {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        fn type_title(&self, title: &str, cx: &mut VisualTestContext) {
            cx.update(|window, cx| {
                let input = self.dialog.read(cx).title.clone();
                input.update(cx, |input, cx| {
                    input.set_value(title.to_owned(), window, cx)
                });
            });
        }

        fn type_body(&self, body: &str, cx: &mut VisualTestContext) {
            cx.update(|window, cx| {
                let input = self.dialog.read(cx).body.clone();
                input.update(cx, |input, cx| input.set_value(body.to_owned(), window, cx));
            });
        }

        fn create(&self, cx: &mut VisualTestContext) {
            cx.update(|window, cx| self.dialog.update(cx, |d, cx| d.create(window, cx)));
            self.settle(cx);
        }

        fn can_create(&self, cx: &mut VisualTestContext) -> bool {
            cx.update(|_, cx| self.dialog.read(cx).can_create(cx))
        }

        fn is_open(&self, cx: &mut VisualTestContext) -> bool {
            self.dialog.read_with(cx, |d, _| d.is_open())
        }

        fn picked(&self, cx: &mut VisualTestContext) -> Option<WorkspaceId> {
            self.dialog.read_with(cx, |d, _| d.workspace())
        }

        fn made(&self) -> Vec<Request> {
            self.api.made_tasks.lock().unwrap().clone()
        }
    }

    mod dialog {
        use super::*;

        #[gpui::test]
        fn should_make_the_task_typed_in_the_preset_workspace_and_close(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(Some(id(1)), cx);

            cx.simulate_input("  Write the codex state rules ");
            harness.type_body(" From recorded sessions. ", cx);
            harness.create(cx);

            assert_eq!(
                harness.made(),
                [Request {
                    title: "Write the codex state rules".into(),
                    body: "From recorded sessions.".into(),
                    workspace: Some(id(1)),
                }]
            );
            assert!(!harness.is_open(cx));
        }

        #[gpui::test]
        fn should_make_it_on_enter_in_the_title(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(None, cx);

            cx.simulate_input("Release core-react");
            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(harness.made().len(), 1);
            assert_eq!(harness.made()[0].workspace, None);
        }

        #[gpui::test]
        fn should_keep_create_disabled_while_the_title_is_blank(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(None, cx);
            assert!(!harness.can_create(cx));

            cx.simulate_input("   ");
            assert!(!harness.can_create(cx));
            cx.simulate_keystrokes("enter");
            harness.settle(cx);
            let create = cx.debug_bounds("create task").expect("shown");
            cx.simulate_click(create.center(), Modifiers::default());
            harness.settle(cx);
            assert_eq!(harness.made(), []);

            harness.type_title("x", cx);
            assert!(harness.can_create(cx));
        }

        #[gpui::test]
        fn should_make_it_from_the_create_button(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(None, cx);
            cx.simulate_input("Cache cargo builds");
            harness.settle(cx);

            let create = cx.debug_bounds("create task").expect("shown");
            cx.simulate_click(create.center(), Modifiers::default());
            harness.settle(cx);

            assert_eq!(harness.made().len(), 1);
        }

        #[gpui::test]
        fn should_offer_the_inbox_then_the_unarchived_workspaces(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(None, cx);

            let offered: Vec<WorkspaceId> = harness
                .dialog
                .read_with(cx, |d, _| d.workspaces.iter().map(|(id, _)| *id).collect());
            assert_eq!(offered, [id(1), id(2)]);
            assert_eq!(harness.picked(cx), None);
        }

        #[gpui::test]
        fn should_preset_the_workspace_it_was_opened_on(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(Some(id(2)), cx);

            assert_eq!(harness.picked(cx), Some(id(2)));
        }

        #[gpui::test]
        fn should_preset_the_inbox_for_an_archived_workspace(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(Some(id(3)), cx);

            assert_eq!(harness.picked(cx), None);
        }

        #[gpui::test]
        fn should_make_it_in_the_workspace_picked(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(None, cx);
            cx.simulate_input("Fix flaky Tabs play test");
            harness
                .dialog
                .update(cx, |d, cx| d.set_workspace(Some(id(2)), cx));

            harness.create(cx);

            assert_eq!(harness.made()[0].workspace, Some(id(2)));
        }

        #[gpui::test]
        fn should_stay_open_and_toast_when_the_task_cant_be_made(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(None, cx);
            cx.simulate_input("x");
            harness.api.fail_next("connection refused");

            harness.create(cx);

            assert!(harness.is_open(cx));
            let shown = cx.update(|_, cx| notice::shown(cx));
            assert_eq!(shown.last().unwrap().title, "Couldn't make the task");
        }

        #[gpui::test]
        fn should_open_empty_again(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(None, cx);
            cx.simulate_input("x");
            harness.create(cx);

            harness.reopen(None, cx);

            assert!(!harness.can_create(cx));
        }

        #[gpui::test]
        fn should_cancel_on_escape(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(None, cx);
            cx.simulate_input("x");

            cx.simulate_keystrokes("escape");
            harness.settle(cx);

            assert!(!harness.is_open(cx));
            assert_eq!(harness.made(), []);
        }

        #[gpui::test]
        fn should_draw_in_both_themes(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(Some(id(1)), cx);

            for preference in [ThemePreference::Light, ThemePreference::Dark] {
                cx.update(|_, cx| Theme::set_preference(preference, cx));
                harness.settle(cx);
            }
        }
    }
}
