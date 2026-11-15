//! The edit workspace dialog (spec §11), Penpot "Screens" → "Edit workspace" and
//! "Edit workspace · error": the workspace's name, for now its only detail.
//!
//! It opens holding the name, selected. Save, and Enter, rename the workspace once the name
//! is neither blank nor unchanged; a blank name, or the daemon's `invalid_name`, shows under
//! the field, and any other failure toasts. Once renamed it closes and emits [`Renamed`]. The
//! slug and the dir stay as they are.

use std::sync::Arc;

use agentdeck_client::ApiError;
use agentdeck_core::WorkspaceId;
use brrritscold_core_gpui::{
    Button, ButtonVariant, Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter,
    DialogHandle, DialogHeader, DialogTitle, Field, FieldError, Input, InputState, Label,
};
use brrritscold_core_gpui_tokens::primitive::space;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
};
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::form_enter::form_enter;
use crate::workspace::failed;

/// What the field says while the name is blank.
const BLANK: &str = "Give the workspace a name.";

/// The dialog renamed a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Renamed(pub WorkspaceId);

/// The workspace the dialog edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditTarget {
    /// Its id.
    pub id: WorkspaceId,
    /// Its name as it stands.
    pub name: SharedString,
}

/// The name to save from what is `typed`, trimmed, or `None` while it is blank or still
/// `current`.
pub fn new_name<'a>(typed: &'a str, current: &str) -> Option<&'a str> {
    let typed = typed.trim();
    (!typed.is_empty() && typed != current).then_some(typed)
}

/// The daemon's words when it refuses the name (`invalid_name`).
fn refusal(err: &anyhow::Error) -> Option<SharedString> {
    let err = err.downcast_ref::<ApiError>()?;
    (err.code.as_deref() == Some("invalid_name")).then(|| err.message.clone().into())
}

/// The dialog, kept by the window and opened from the workspace's `…` menu.
pub struct EditWorkspaceDialog {
    api: Arc<dyn Api>,
    handle: DialogHandle,
    name: Entity<InputState>,
    target: Option<EditTarget>,
    /// The daemon's refusal of the name as typed; typing clears it.
    refusal: Option<SharedString>,
    /// Whether a rename is out; another waits for it.
    saving: bool,
    /// Whether the name field takes the keyboard once the dialog shows.
    focus_name: bool,
    _name: Subscription,
}

impl EventEmitter<Renamed> for EditWorkspaceDialog {}

impl EditWorkspaceDialog {
    /// A closed dialog that renames workspaces through `api`.
    pub fn new(api: Arc<dyn Api>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx));
        let _name = cx.subscribe_in(&name, window, |this, _, event, window, cx| match event {
            InputEvent::PressEnter { .. } => this.save(window, cx),
            InputEvent::Change => {
                this.refusal = None;
                cx.notify();
            }
            InputEvent::Focus | InputEvent::Blur => {}
        });
        EditWorkspaceDialog {
            api,
            handle: DialogHandle::new(false),
            name,
            target: None,
            refusal: None,
            saving: false,
            focus_name: false,
            _name,
        }
    }

    /// Opens it on `target`, the name field holding its name, selected and focused.
    pub fn open(&mut self, target: EditTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.name.update(cx, |input, cx| {
            input.set_value(target.name.clone(), window, cx);
            input.select_all(window, cx);
        });
        self.target = Some(target);
        self.refusal = None;
        self.handle.open(window, cx);
        self.focus_name = true;
        cx.notify();
    }

    /// Whether it is open.
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.handle.is_open()
    }

    /// What the name field holds.
    #[cfg(test)]
    pub fn name(&self, cx: &App) -> String {
        self.name.read(cx).value().to_string()
    }

    /// What shows under the field: why the name can't be saved, if it can't.
    pub fn problem(&self, cx: &App) -> Option<SharedString> {
        if self.name.read(cx).value().trim().is_empty() {
            return Some(BLANK.into());
        }
        self.refusal.clone()
    }

    /// Whether Save can go ahead: the name is neither blank nor unchanged, and no rename is
    /// out.
    pub fn can_save(&self, cx: &App) -> bool {
        let typed = self.name.read(cx).value();
        !self.saving && (self.target.as_ref()).is_some_and(|t| new_name(&typed, &t.name).is_some())
    }

    /// Renames the workspace to the name typed, once it can: what Save and Enter do.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone().filter(|_| self.can_save(cx)) else {
            return;
        };
        let typed = self.name.read(cx).value();
        let Some(name) = new_name(&typed, &target.name) else {
            return;
        };
        self.saving = true;
        cx.notify();
        let renaming = self.api.rename_workspace(target.id, name.to_owned(), cx);
        cx.spawn_in(window, async move |this, cx| {
            let renamed = renaming.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match renamed {
                    Ok(()) => {
                        this.handle.close(window, cx);
                        cx.emit(Renamed(target.id));
                    }
                    Err(err) => match refusal(&err) {
                        Some(why) => this.refusal = Some(why),
                        None => {
                            failed(format!("Couldn't rename {}", target.name), &err, window, cx)
                        }
                    },
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Penpot `agentdeck/EditWorkspaceForm`: the Name field, then its error, `space.4` apart.
    fn form(&self, cx: &App) -> impl IntoElement {
        let problem = self.problem(cx);
        div()
            .flex()
            .flex_col()
            .gap(space::S4)
            .child(
                Field::new().child(Label::new().child("Name")).child(
                    div()
                        .debug_selector(|| "edit workspace name".into())
                        .child(form_enter(
                            Input::new(&self.name).aria_invalid(problem.is_some()),
                        )),
                ),
            )
            .when_some(problem, |form, why| {
                form.child(
                    div()
                        .debug_selector(|| "edit workspace error".into())
                        .child(FieldError::new(why)),
                )
            })
    }
}

impl Render for EditWorkspaceDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_name) {
            // The dialog focuses itself in an effect it queues while drawing, after this
            // render; queueing from inside an effect lands after that one.
            let name = self.name.clone();
            window.defer(cx, move |window, cx| {
                window.defer(cx, move |window, cx| {
                    name.update(cx, |name, cx| name.focus(window, cx));
                });
            });
        }
        let name = (self.target.as_ref()).map_or_else(SharedString::default, |t| t.name.clone());
        let can_save = self.can_save(cx);
        Dialog::new("edit-workspace")
            .handle(self.handle.clone())
            .content(
                DialogContent::new()
                    .header(
                        DialogHeader::new()
                            .title(DialogTitle::new(format!("Edit {name}")))
                            .description(DialogDescription::new("Its folder stays where it is.")),
                    )
                    .child(self.form(cx))
                    .footer(
                        DialogFooter::new()
                            .child(DialogClose::new(
                                Button::new("edit-workspace-cancel")
                                    .variant(ButtonVariant::Outline)
                                    .label("Cancel"),
                            ))
                            .child(
                                div().debug_selector(|| "edit workspace save".into()).child(
                                    Button::new("edit-workspace-save")
                                        .label("Save")
                                        .disabled(!can_save)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.save(window, cx);
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

    use brrritscold_core_gpui::Root;
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::notice;
    use crate::overview::fixtures::id;

    struct Harness {
        api: Arc<FakeApi>,
        dialog: Entity<EditWorkspaceDialog>,
        renamed: Rc<RefCell<Vec<WorkspaceId>>>,
        _renamed: Subscription,
    }

    fn target() -> EditTarget {
        EditTarget {
            id: id(1),
            name: "tabs-a11y".into(),
        }
    }

    impl Harness {
        /// A window holding the dialog, opened on [`target`].
        fn open(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            let api = Arc::new(FakeApi::default());
            let mut dialog = None;
            let window = cx.add_window(|window, cx| {
                let api: Arc<dyn Api> = api.clone();
                let view = cx.new(|cx| EditWorkspaceDialog::new(api, window, cx));
                dialog = Some(view.clone());
                Root::new(view, window, cx)
            });
            let cx = VisualTestContext::from_window(*window, cx).into_mut();
            let dialog = dialog.unwrap();
            let renamed = Rc::new(RefCell::new(Vec::new()));
            let seen = renamed.clone();
            let _renamed = cx.update(|_, cx| {
                cx.subscribe(&dialog, move |_, Renamed(id), _| {
                    seen.borrow_mut().push(*id)
                })
            });
            cx.update(|window, cx| dialog.update(cx, |d, cx| d.open(target(), window, cx)));
            let harness = Harness {
                api,
                dialog,
                renamed,
                _renamed,
            };
            harness.settle(cx);
            (harness, cx)
        }

        fn settle(&self, cx: &mut VisualTestContext) {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        fn set_name(&self, name: &str, cx: &mut VisualTestContext) {
            cx.update(|window, cx| {
                let input = self.dialog.read(cx).name.clone();
                input.update(cx, |input, cx| input.set_value(name.to_owned(), window, cx));
            });
            self.settle(cx);
        }

        fn save(&self, cx: &mut VisualTestContext) {
            cx.update(|window, cx| self.dialog.update(cx, |d, cx| d.save(window, cx)));
            self.settle(cx);
        }

        fn is_open(&self, cx: &mut VisualTestContext) -> bool {
            self.dialog.read_with(cx, |d, _| d.is_open())
        }

        fn can_save(&self, cx: &mut VisualTestContext) -> bool {
            cx.update(|_, cx| self.dialog.read(cx).can_save(cx))
        }

        fn problem(&self, cx: &mut VisualTestContext) -> Option<SharedString> {
            cx.update(|_, cx| self.dialog.read(cx).problem(cx))
        }

        fn renames(&self) -> Vec<(WorkspaceId, String)> {
            self.api.renamed.lock().unwrap().clone()
        }
    }

    mod dialog {
        use super::*;

        #[gpui::test]
        fn should_open_holding_the_name_selected(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);

            let (value, selected) = cx.update(|_, cx| {
                let input = harness.dialog.read(cx).name.read(cx);
                (input.value(), input.selected_range())
            });

            assert_eq!(value, "tabs-a11y");
            assert_eq!(selected, 0.."tabs-a11y".len());
            assert!(harness.is_open(cx));
            assert_eq!(harness.problem(cx), None);
        }

        #[gpui::test]
        fn should_keep_save_disabled_while_the_name_is_unchanged(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            assert!(!harness.can_save(cx));

            harness.set_name(" tabs-a11y ", cx);
            assert!(!harness.can_save(cx));
            harness.save(cx);

            assert_eq!(harness.renames(), []);
            assert!(harness.is_open(cx));
        }

        #[gpui::test]
        fn should_rename_to_the_trimmed_name_and_close(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            harness.set_name("  tabs keyboard ", cx);
            assert!(harness.can_save(cx));

            harness.save(cx);

            assert_eq!(harness.renames(), [(id(1), "tabs keyboard".to_owned())]);
            assert_eq!(*harness.renamed.borrow(), [id(1)]);
            assert!(!harness.is_open(cx));
        }

        #[gpui::test]
        fn should_replace_the_selected_name_as_typed_and_rename_on_enter(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);

            cx.simulate_input("keyboard");
            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(harness.renames(), [(id(1), "keyboard".to_owned())]);
            assert!(!harness.is_open(cx));
        }

        #[gpui::test]
        fn should_show_the_blank_name_problem_and_stay_open_on_enter(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);

            cx.simulate_keystrokes("backspace");
            harness.settle(cx);
            assert_eq!(harness.problem(cx), Some(BLANK.into()));
            assert!(!harness.can_save(cx));
            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(harness.renames(), []);
            assert!(harness.is_open(cx));
        }

        #[gpui::test]
        fn should_show_the_daemons_refusal_under_the_field_until_the_name_changes(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(cx);
            cx.simulate_input("bad");
            harness
                .api
                .refuse_next("invalid_name", "a workspace's name can't be empty");

            harness.save(cx);

            assert_eq!(
                harness.problem(cx),
                Some("a workspace's name can't be empty".into())
            );
            assert!(harness.is_open(cx));
            cx.simulate_input("ly");
            harness.settle(cx);
            assert_eq!(harness.problem(cx), None);
        }

        #[gpui::test]
        fn should_toast_another_failure_and_stay_open(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            cx.simulate_input("keyboard");
            harness.api.fail_next("connection refused");

            harness.save(cx);

            assert!(harness.is_open(cx));
            assert_eq!(harness.problem(cx), None);
            let shown = cx.update(|_, cx| notice::shown(cx));
            assert_eq!(shown.last().unwrap().title, "Couldn't rename tabs-a11y");
            assert!(harness.renamed.borrow().is_empty());
        }

        #[gpui::test]
        fn should_open_on_another_workspace_without_the_last_problem(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            harness.set_name("", cx);

            let other = EditTarget {
                id: id(2),
                name: "gpui-terminal".into(),
            };
            cx.update(|window, cx| harness.dialog.update(cx, |d, cx| d.open(other, window, cx)));
            harness.settle(cx);

            assert_eq!(harness.problem(cx), None);
            cx.simulate_input("terminal");
            harness.save(cx);
            assert_eq!(harness.renames(), [(id(2), "terminal".to_owned())]);
        }
    }

    mod new_name {
        use super::*;

        #[test]
        fn should_trim_what_is_typed() {
            assert_eq!(new_name("  keyboard ", "tabs-a11y"), Some("keyboard"));
        }

        #[test]
        fn should_be_none_while_blank() {
            assert_eq!(new_name("   ", "tabs-a11y"), None);
        }

        #[test]
        fn should_be_none_while_unchanged() {
            assert_eq!(new_name(" tabs-a11y", "tabs-a11y"), None);
        }
    }
}
