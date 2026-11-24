//! The edit workspace dialog (spec §11), Penpot "Screens · Dialogs" → "Edit workspace" and
//! its states: the workspace's name and the Repositories it links ([`LinkPicker`]).
//!
//! It opens holding the name, selected, or with the picker focused when opened to link a
//! Repository. Links and unlinks are staged until Save, which applies them with the name in
//! one request; Cancel discards them, and nothing asks to confirm. Save, and Enter in the name,
//! go ahead once the name isn't blank and something changed. A blank name, or the daemon's
//! `invalid_name`, shows under the name; a Repository the daemon can't link shows under the
//! picker, and any other failure toasts. Once saved it closes and emits [`Saved`]; each
//! Repository it unlinked counts as Not now for the workspace's Suggestions. The slug stays as
//! it is. An archived workspace can be edited too.

use std::sync::Arc;

use agentdeck_client::ApiError;
use agentdeck_core::WorkspaceId;
use brrritscold_core_gpui::{
    Button, ButtonVariant, Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter,
    DialogHandle, DialogHeader, DialogTitle, Field, FieldError, FieldLabel, Input, InputState,
};
use brrritscold_core_gpui_tokens::primitive::space;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
};
use gpui_base::input::InputEvent;

use crate::api::{Api, Change};
use crate::connection::Connection;
use crate::gui_state;
use crate::link_picker::{LinkPicker, Offer, Staging};
use crate::workspace::failed;

/// What the field says while the name is blank.
const BLANK: &str = "Give the workspace a name.";

/// The dialog's description, under its title.
pub const DESCRIPTION: &str = "Its pull requests come from the repositories it links.";

/// The dialog saved a workspace's changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Saved(pub WorkspaceId);

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
    /// The Repositories it links, and what is staged.
    picker: Entity<LinkPicker>,
    /// The daemon's refusal of the name as typed; typing clears it.
    refusal: Option<SharedString>,
    /// Whether a save is out; another waits for it.
    saving: bool,
    /// Whether the name field takes the keyboard once the dialog shows.
    focus_name: bool,
    _name: Subscription,
    _picker: Subscription,
}

impl EventEmitter<Saved> for EditWorkspaceDialog {}

impl EditWorkspaceDialog {
    /// A closed dialog that renames workspaces through `api`, its picker following
    /// `connection`'s events.
    pub fn new(
        api: Arc<dyn Api>,
        connection: &Entity<Connection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx));
        let _name = cx.subscribe_in(&name, window, |this, _, event, window, cx| match event {
            InputEvent::PressEnter { .. } => this.save(window, cx),
            InputEvent::Change => {
                this.refusal = None;
                cx.notify();
            }
            InputEvent::Focus | InputEvent::Blur => {}
        });
        let picker = cx.new(|cx| LinkPicker::new(api.clone(), connection, window, cx));
        let _picker = cx.subscribe(&picker, |_, _, _: &Staging, cx| cx.notify());
        EditWorkspaceDialog {
            api,
            handle: DialogHandle::new(false),
            name,
            target: None,
            picker,
            refusal: None,
            saving: false,
            focus_name: false,
            _name,
            _picker,
        }
    }

    /// Opens it on `target`, with what `offer` holds for it and nothing staged: the name field
    /// holding its name, selected, and focused, or the picker focused when `link` (the empty
    /// state's Link repository).
    pub fn open(
        &mut self,
        target: EditTarget,
        offer: Offer,
        link: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.name.update(cx, |input, cx| {
            input.set_value(target.name.clone(), window, cx);
            input.select_all(window, cx);
        });
        self.picker.update(cx, |picker, cx| {
            picker.reset(offer, window, cx);
            if link {
                picker.focus(cx);
            }
        });
        self.target = Some(target);
        self.refusal = None;
        self.handle.open(window, cx);
        self.focus_name = !link;
        cx.notify();
    }

    /// The Repositories part.
    #[cfg(test)]
    pub fn picker(&self) -> &Entity<LinkPicker> {
        &self.picker
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

    /// Whether Save can go ahead: the name isn't blank, it or the Repositories changed, and
    /// neither a save nor a typed Repository's lookup is out.
    pub fn can_save(&self, cx: &App) -> bool {
        let Some(target) = &self.target else {
            return false;
        };
        let typed = self.name.read(cx).value();
        let picker = self.picker.read(cx);
        !self.saving
            && !picker.is_looking_up()
            && !typed.trim().is_empty()
            && (new_name(&typed, &target.name).is_some() || picker.is_changed())
    }

    /// Saves the name typed and the staged links and unlinks in one request, once it can:
    /// what Save and Enter do.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone().filter(|_| self.can_save(cx)) else {
            return;
        };
        let typed = self.name.read(cx).value();
        let (link, unlink) = self.picker.read(cx).change();
        let unlinked = unlink.clone();
        let change = Change {
            name: new_name(&typed, &target.name).map(str::to_owned),
            link,
            unlink,
        };
        self.saving = true;
        cx.notify();
        let saving = self.api.edit_workspace(target.id, change, cx);
        cx.spawn_in(window, async move |this, cx| {
            let saved = saving.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match saved {
                    Ok(()) => {
                        // An unlink in the GUI counts as Not now for the Workspace's Suggestions.
                        gui_state::update(cx, |state| {
                            for repository in unlinked {
                                state.dismiss(target.id, repository);
                            }
                        });
                        this.handle.close(window, cx);
                        cx.emit(Saved(target.id));
                    }
                    Err(err) => match refusal(&err) {
                        Some(why) => this.refusal = Some(why),
                        None if this.picker.update(cx, |p, cx| p.refused(&err, cx)) => {}
                        None => failed(format!("Couldn't save {}", target.name), &err, window, cx),
                    },
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Penpot `agentdeck/EditWorkspaceForm`: the Name field, its error, then the
    /// Repositories, `space.4` apart.
    fn form(&self, cx: &App) -> impl IntoElement {
        let problem = self.problem(cx);
        div()
            .flex()
            .flex_col()
            .gap(space::S4)
            .child(
                Field::new()
                    .child(
                        div()
                            .debug_selector(|| "edit workspace name label".into())
                            .child(FieldLabel::new().child("Name")),
                    )
                    .child(
                        div()
                            .debug_selector(|| "edit workspace name".into())
                            .child(Input::new(&self.name).aria_invalid(problem.is_some())),
                    ),
            )
            .when_some(problem, |form, why| {
                form.child(
                    div()
                        .debug_selector(|| "edit workspace error".into())
                        .child(FieldError::new(why)),
                )
            })
            .child(self.picker.clone())
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
                            .description(DialogDescription::new(DESCRIPTION)),
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
    use crate::link_picker::Staged;
    use crate::notice;
    use crate::overview::Snapshot;
    use crate::overview::fixtures::{id, links, pull_request, repository_id, workspace};

    struct Harness {
        api: Arc<FakeApi>,
        dialog: Entity<EditWorkspaceDialog>,
        renamed: Rc<RefCell<Vec<WorkspaceId>>>,
        _renamed: Subscription,
    }

    const WIDGETS: &str = "acme/widgets";
    const TOOLS: &str = "acme/tools";

    /// Workspace 1, tabs-a11y, linking widgets, which has 2 open PRs; you also have a PR in
    /// tools.
    fn snapshot() -> Snapshot {
        Snapshot {
            workspaces: vec![workspace(1, "tabs-a11y")],
            repositories: vec![links(TOOLS, &[]), links(WIDGETS, &[id(1)])],
            prs: vec![
                pull_request(1, WIDGETS),
                pull_request(2, WIDGETS),
                pull_request(3, TOOLS),
            ],
            ..Snapshot::default()
        }
    }

    fn target() -> EditTarget {
        EditTarget {
            id: id(1),
            name: "tabs-a11y".into(),
        }
    }

    impl Harness {
        /// A window holding the dialog, opened on [`target`] over [`snapshot`].
        fn open(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            Harness::open_on(snapshot(), false, cx)
        }

        /// A window holding the dialog, opened on [`target`] over `snapshot`, the picker
        /// focused when `link`.
        fn open_on(
            snapshot: Snapshot,
            link: bool,
            cx: &mut TestAppContext,
        ) -> (Harness, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            let api = Arc::new(FakeApi::with(snapshot));
            let connector = std::rc::Rc::new(crate::connection::fake::FakeConnector::default());
            let _remote = connector.then_connect_to(api.clone());
            let connection = cx.update(|cx| cx.new(|cx| Connection::new(connector, cx)));
            let mut dialog = None;
            let window = cx.add_window(|window, cx| {
                let api: Arc<dyn Api> = api.clone();
                let view = cx.new(|cx| EditWorkspaceDialog::new(api, &connection, window, cx));
                dialog = Some(view.clone());
                Root::new(view, window, cx)
            });
            let cx = VisualTestContext::from_window(*window, cx).into_mut();
            cx.update(|window, _| window.activate_window());
            let dialog = dialog.unwrap();
            let renamed = Rc::new(RefCell::new(Vec::new()));
            let seen = renamed.clone();
            let _renamed = cx.update(|_, cx| {
                cx.subscribe(&dialog, move |_, Saved(id), _| seen.borrow_mut().push(*id))
            });
            let offer = Offer::read(&api.snapshot_now(), Some(id(1)));
            cx.update(|window, cx| {
                dialog.update(cx, |d, cx| d.open(target(), offer, link, window, cx))
            });
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
            self.api.renames()
        }

        fn click(&self, selector: &'static str, cx: &mut VisualTestContext) {
            let at = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} shown"));
            cx.simulate_click(at.center(), gpui::Modifiers::default());
            self.settle(cx);
        }

        /// Types `text` into the picker.
        fn type_in_picker(&self, text: &str, cx: &mut VisualTestContext) {
            self.picker(cx).update(cx, |p, cx| p.focus(cx));
            self.settle(cx);
            cx.simulate_input(text);
            self.settle(cx);
        }

        fn picker(&self, cx: &mut VisualTestContext) -> Entity<LinkPicker> {
            self.dialog.read_with(cx, |d, _| d.picker().clone())
        }

        fn rows(&self, cx: &mut VisualTestContext) -> Vec<(String, Staged)> {
            let picker = self.picker(cx);
            picker.read_with(cx, |p, _| {
                (p.rows().iter())
                    .map(|r| (r.repository.name.to_string(), r.staged))
                    .collect()
            })
        }

        /// The Repositories workspace 1 links now, by name.
        fn linked_now(&self) -> Vec<String> {
            let snapshot = self.api.snapshot_now();
            (snapshot.repositories.iter())
                .filter(|r| r.workspaces.contains(&id(1)))
                .map(|r| r.name.to_string())
                .collect()
        }

        fn edits(&self) -> Vec<(WorkspaceId, Change)> {
            self.api.edits.lock().unwrap().clone()
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
        fn should_close_without_renaming_on_escape(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);

            cx.simulate_input("keyboard");
            cx.simulate_keystrokes("escape");
            harness.settle(cx);

            assert!(harness.renames().is_empty());
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
            assert_eq!(shown.last().unwrap().title, "Couldn't save tabs-a11y");
            assert!(harness.renamed.borrow().is_empty());
        }

        #[gpui::test]
        fn should_say_where_its_pull_requests_come_from_not_where_a_folder_is(
            cx: &mut TestAppContext,
        ) {
            let (_harness, cx) = Harness::open(cx);

            assert_eq!(
                DESCRIPTION,
                "Its pull requests come from the repositories it links."
            );
            assert!(cx.debug_bounds("link picker").is_some());
        }

        #[gpui::test]
        fn should_open_on_another_workspace_without_the_last_problem(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            harness.set_name("", cx);

            let other = EditTarget {
                id: id(2),
                name: "gpui-terminal".into(),
            };
            cx.update(|window, cx| {
                harness.dialog.update(cx, |d, cx| {
                    d.open(other, Offer::default(), false, window, cx)
                })
            });
            harness.settle(cx);

            assert_eq!(harness.problem(cx), None);
            cx.simulate_input("terminal");
            harness.save(cx);
            assert_eq!(harness.renames(), [(id(2), "terminal".to_owned())]);
        }
    }

    /// Where the form's lines sit (#405), against Penpot "Screens · Dialogs" → "Edit
    /// workspace": each line's top below the Name label's top, as core-react lays it out,
    /// within the pixel GPUI snaps to. Penpot draws each `leading-snug` label 20px tall
    /// where code has 19.25, so its lines sit 0.75px lower (input 32, rows 106). The
    /// header above is core-gpui's, and the test platform's text widths wrap its
    /// description differently, so the form is measured from its own first line.
    mod layout {
        use super::*;

        /// The Name input's top: the label is a `Field/Label`, core-react's FieldLabel
        /// (`leading-snug`, 19.25px), then the Field's `gap-3`.
        const NAME_INPUT: f32 = 31.25;
        /// The linked rows' top: the 36px input, the form's `space.4`, the Repositories
        /// label (a plain `Label`, `leading-none`, 14px), then the picker's `space.2`.
        const REPOSITORY_ROWS: f32 = 105.25;

        fn assert_below_the_name_label(
            selector: &'static str,
            expected: f32,
            cx: &mut VisualTestContext,
        ) {
            let label = cx
                .debug_bounds("edit workspace name label")
                .expect("the Name label");
            let at = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} shown"));
            let app = f32::from(at.top() - label.top());
            assert!(
                (app - expected).abs() <= 1.,
                "{selector}: {app}px below the Name label's top, expected {expected}px"
            );
        }

        #[gpui::test]
        fn should_place_the_name_input_under_a_field_label(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(cx);

            assert_below_the_name_label("edit workspace name", NAME_INPUT, cx);
        }

        #[gpui::test]
        fn should_place_the_repositories_under_the_name_field(cx: &mut TestAppContext) {
            let (_harness, cx) = Harness::open(cx);

            assert_below_the_name_label("repository rows", REPOSITORY_ROWS, cx);
        }
    }

    mod repositories {
        use super::*;

        #[gpui::test]
        fn should_list_the_linked_repositories(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);

            assert_eq!(harness.rows(cx), [(WIDGETS.to_owned(), Staged::Linked)]);
            assert!(cx.debug_bounds("unlink acme/widgets").is_some());
        }

        #[gpui::test]
        fn should_stage_an_unlink_until_save_then_apply_it_with_the_name(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            harness.set_name("keyboard", cx);
            harness.click("unlink acme/widgets", cx);
            harness.type_in_picker("tools", cx);
            harness.click("pick acme/tools", cx);

            assert_eq!(harness.edits(), []);
            assert_eq!(harness.linked_now(), [WIDGETS]);
            assert!(cx.debug_bounds("leaving note").is_some());
            harness.save(cx);

            assert_eq!(
                harness.edits(),
                [(
                    id(1),
                    Change {
                        name: Some("keyboard".into()),
                        link: vec![TOOLS.parse().unwrap()],
                        unlink: vec![repository_id(WIDGETS)],
                    }
                )]
            );
            assert_eq!(harness.linked_now(), [TOOLS]);
            // No confirm dialog: one Save, and it's closed.
            assert!(!harness.is_open(cx));
            assert_eq!(*harness.renamed.borrow(), [id(1)]);
        }

        #[gpui::test]
        fn should_save_a_staged_change_with_the_name_unchanged(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            assert!(!harness.can_save(cx));

            harness.click("unlink acme/widgets", cx);
            assert!(harness.can_save(cx));
            harness.click("undo acme/widgets", cx);
            assert!(!harness.can_save(cx));
            harness.click("unlink acme/widgets", cx);
            harness.save(cx);

            let (_, change) = harness.edits().remove(0);
            assert_eq!(change.name, None);
            assert_eq!(change.unlink, [repository_id(WIDGETS)]);
            assert_eq!(harness.linked_now(), Vec::<String>::new());
        }

        #[gpui::test]
        fn should_discard_what_was_staged_on_cancel(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            harness.click("unlink acme/widgets", cx);
            harness.type_in_picker("tools", cx);
            harness.click("pick acme/tools", cx);

            cx.simulate_keystrokes("escape");
            harness.settle(cx);
            cx.simulate_keystrokes("escape");
            harness.settle(cx);
            assert!(!harness.is_open(cx));
            let offer = Offer::read(&harness.api.snapshot_now(), Some(id(1)));
            cx.update(|window, cx| {
                harness
                    .dialog
                    .update(cx, |d, cx| d.open(target(), offer, false, window, cx))
            });
            harness.settle(cx);

            assert_eq!(harness.edits(), []);
            assert_eq!(harness.rows(cx), [(WIDGETS.to_owned(), Staged::Linked)]);
            assert!(cx.debug_bounds("leaving note").is_none());
        }

        #[gpui::test]
        fn should_show_a_repository_github_cannot_see_under_the_picker_and_stay_open(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(cx);
            harness.click("unlink acme/widgets", cx);
            harness.api.refuse_next(
                "unknown_repository",
                "GitHub can't see acme/tools. Check the name or your token's access.",
            );

            harness.save(cx);

            assert!(harness.is_open(cx));
            assert!(cx.debug_bounds("picker error").is_some());
            assert_eq!(harness.problem(cx), None);
            assert_eq!(harness.rows(cx), [(WIDGETS.to_owned(), Staged::Unlinking)]);
        }

        #[gpui::test]
        fn should_count_a_saved_unlink_as_not_now(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            harness.click("unlink acme/widgets", cx);

            harness.save(cx);

            let dismissed = cx.update(|_, cx| {
                gui_state::read(cx, |s| s.is_dismissed(id(1), repository_id(WIDGETS)))
            });
            assert!(dismissed);
        }

        #[gpui::test]
        fn should_not_count_an_unlink_cancelled_or_refused(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            harness.click("unlink acme/widgets", cx);
            harness.api.fail_next("the daemon went away");

            harness.save(cx);

            let dismissed = cx.update(|_, cx| {
                gui_state::read(cx, |s| s.is_dismissed(id(1), repository_id(WIDGETS)))
            });
            assert!(!dismissed);
        }

        #[gpui::test]
        fn should_suggest_in_the_picker_what_was_told_not_now(cx: &mut TestAppContext) {
            let mut snapshot = snapshot();
            snapshot.suggestions.insert(
                id(1),
                vec![crate::overview::fixtures::suggestion("acme/infra")],
            );
            cx.update(|cx| {
                gui_state::update(cx, |s| s.dismiss(id(1), repository_id("acme/infra")))
            });

            let (_harness, cx) = Harness::open_on(snapshot, true, cx);

            assert!(cx.debug_bounds("suggested acme/infra").is_some());
        }

        #[gpui::test]
        fn should_link_on_an_archived_workspace(cx: &mut TestAppContext) {
            let mut snapshot = snapshot();
            snapshot.workspaces[0].archived_at = Some("2026-11-20T00:00:00Z".into());
            let (harness, cx) = Harness::open_on(snapshot, false, cx);
            harness.type_in_picker("tools", cx);
            harness.click("pick acme/tools", cx);

            harness.save(cx);

            assert_eq!(harness.linked_now(), [TOOLS, WIDGETS]);
            assert!(!harness.is_open(cx));
        }

        #[gpui::test]
        fn should_focus_the_picker_with_its_list_open_when_opened_to_link(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open_on(snapshot(), true, cx);

            let picker = harness.picker(cx);
            assert!(picker.read_with(cx, |p, _| p.is_open()));
            assert!(cx.debug_bounds("pick acme/tools").is_some());
            let name_focused = cx.update(|window, cx| {
                let name = harness.dialog.read(cx).name.read(cx);
                gpui::Focusable::focus_handle(name, cx).is_focused(window)
            });
            assert!(!name_focused);
            cx.simulate_input("too");
            harness.settle(cx);
            cx.simulate_keystrokes("enter");
            harness.settle(cx);
            assert_eq!(harness.rows(cx)[1], (TOOLS.to_owned(), Staged::Linking));
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
