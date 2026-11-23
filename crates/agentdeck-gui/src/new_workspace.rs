//! The new workspace dialog (spec §11), Penpot "Screens · Dialogs" → "New workspace" and
//! "New workspace · picker": a name, and optionally Repositories to link, picked with the
//! "Link a repository…" picker ([`LinkPicker`]). A workspace has no folder.
//!
//! It checks the name before asking ([`request`]), shows the daemon's `invalid_name` under
//! it, a Repository the daemon can't link under the picker, and reports any other failure in
//! a toast. Making the workspace links what was picked, in the one request. Once made it
//! closes and emits [`Created`].

use std::sync::Arc;

use agentdeck_client::ApiError;
use agentdeck_core::{RepositoryName, WorkspaceId};
use brrritscold_core_gpui::{
    Button, ButtonVariant, Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter,
    DialogHandle, DialogHeader, DialogTitle, Field, FieldDescription, FieldError, Input,
    InputState, Label,
};
use brrritscold_core_gpui_tokens::primitive::space;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
};
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::link_picker::{Choice, LinkPicker, Offer};
use crate::workspace::failed;

/// The dialog's description, under its title.
pub const DESCRIPTION: &str =
    "An area of work, such as one product, for any number of repos and agents.";

/// The name field's description.
pub const NAME_HINT: &str = "Names the sidebar entry.";

/// What the name field says when it is blank.
pub const NO_NAME: &str = "Give the workspace a name.";

/// The dialog made a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Created(pub WorkspaceId);

/// The dialog, kept by the window and opened from the sidebar's `+`.
pub struct NewWorkspaceDialog {
    api: Arc<dyn Api>,
    handle: DialogHandle,
    name: Entity<InputState>,
    /// The Repositories picked to link.
    picker: Entity<LinkPicker>,
    /// Why the last try failed, shown under the name.
    problem: Option<SharedString>,
    /// Whether a request is out; another waits for it.
    creating: bool,
    /// Whether the name field takes the keyboard once the dialog shows.
    focus_name: bool,
    _enter: Subscription,
}

impl EventEmitter<Created> for NewWorkspaceDialog {}

impl NewWorkspaceDialog {
    /// A closed dialog that makes workspaces through `api`.
    pub fn new(api: Arc<dyn Api>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. tabs-a11y"));
        let _enter = cx.subscribe_in(&name, window, |this, _, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.create(window, cx);
            }
        });
        let picker = cx.new(|cx| LinkPicker::new(api.clone(), window, cx));
        NewWorkspaceDialog {
            api,
            handle: DialogHandle::new(false),
            name,
            picker,
            problem: None,
            creating: false,
            focus_name: false,
            _enter,
        }
    }

    /// Opens it empty, the name field focused, its picker offering what `offer` holds.
    pub fn open(&mut self, offer: Offer, window: &mut Window, cx: &mut Context<Self>) {
        self.name
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.picker
            .update(cx, |picker, cx| picker.reset(offer, window, cx));
        self.problem = None;
        self.handle.open(window, cx);
        self.focus_name = true;
        cx.notify();
    }

    /// Opens it as [`open`](Self::open) does, with `picked` already in its Repositories, to
    /// link: a Repository group's New workspace.
    pub fn open_linking(
        &mut self,
        offer: Offer,
        picked: Choice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open(offer, window, cx);
        self.picker
            .update(cx, |picker, cx| picker.pick(picked, window, cx));
    }

    /// Whether it is open.
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.handle.is_open()
    }

    /// Why the last try failed, if it did.
    #[cfg(test)]
    pub fn problem(&self) -> Option<&SharedString> {
        self.problem.as_ref()
    }

    /// The Repositories part.
    #[cfg(test)]
    pub fn picker(&self) -> &Entity<LinkPicker> {
        &self.picker
    }

    /// Asks the daemon for the workspace the form describes: what its button and Enter do.
    pub fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.creating || self.picker.read(cx).is_looking_up() {
            return;
        }
        let (link, _) = self.picker.read(cx).change();
        let request = match request(&self.name.read(cx).value(), link) {
            Ok(request) => request,
            Err(problem) => {
                self.problem = Some(problem);
                cx.notify();
                return;
            }
        };
        self.creating = true;
        self.problem = None;
        let creating = self.api.new_workspace(request, cx);
        cx.spawn_in(window, async move |this, cx| {
            let made = creating.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.creating = false;
                match made {
                    Ok(id) => {
                        this.handle.close(window, cx);
                        cx.emit(Created(id));
                    }
                    Err(err) => match refusal(&err) {
                        Some(problem) => this.problem = Some(problem),
                        None if this.picker.update(cx, |p, cx| p.refused(&err, cx)) => {}
                        None => failed("Couldn't make the workspace", &err, window, cx),
                    },
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Penpot `NewWorkspaceForm`: the Name field, then the Repositories, `space.4` apart.
    fn form(&self) -> impl IntoElement {
        let problem = self.problem.clone();
        div()
            .flex()
            .flex_col()
            .gap(space::S4)
            .child(
                div().debug_selector(|| "name field".into()).child(
                    Field::new()
                        .child(Label::new().child("Name"))
                        .child(Input::new(&self.name).aria_invalid(problem.is_some()))
                        .child(FieldDescription::new(NAME_HINT))
                        .when_some(problem, |field, why| field.child(FieldError::new(why))),
                ),
            )
            .child(self.picker.clone())
    }
}

/// What the daemon said is wrong with the name, if it refused it.
fn refusal(err: &anyhow::Error) -> Option<SharedString> {
    let err = err.downcast_ref::<ApiError>()?;
    (err.code.as_deref()? == "invalid_name").then(|| err.message.clone().into())
}

impl Render for NewWorkspaceDialog {
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
        Dialog::new("new-workspace")
            .handle(self.handle.clone())
            .content(
                DialogContent::new()
                    .header(
                        DialogHeader::new()
                            .title(DialogTitle::new("New workspace"))
                            .description(DialogDescription::new(DESCRIPTION)),
                    )
                    .child(self.form())
                    .footer(
                        DialogFooter::new()
                            .child(DialogClose::new(
                                Button::new("new-workspace-cancel")
                                    .variant(ButtonVariant::Outline)
                                    .label("Cancel"),
                            ))
                            .child(
                                Button::new("new-workspace-create")
                                    .label("Create workspace")
                                    .disabled(self.creating)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.create(window, cx);
                                    })),
                            ),
                    ),
            )
    }
}

/// What the form asks the daemon for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The name, trimmed.
    pub name: String,
    /// The Repositories picked to link, by `owner/name`.
    pub link: Vec<RepositoryName>,
}

/// The request the form's `name` and picked Repositories make, or what's wrong with the name.
pub fn request(name: &str, link: Vec<RepositoryName>) -> Result<Request, SharedString> {
    match name.trim() {
        "" => Err(NO_NAME.into()),
        name => Ok(Request {
            name: name.to_owned(),
            link,
        }),
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
    use crate::link_picker::tests::repository;
    use crate::overview::Snapshot;
    use crate::overview::fixtures::{id, links, pull_request};

    struct Harness {
        api: Arc<FakeApi>,
        dialog: Entity<NewWorkspaceDialog>,
        created: Rc<RefCell<Vec<WorkspaceId>>>,
        _created: Subscription,
    }

    impl Harness {
        /// A window holding the dialog, opened.
        fn open(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            Harness::open_on(Snapshot::default(), cx)
        }

        /// A window holding the dialog, opened over `snapshot`.
        fn open_on(
            snapshot: Snapshot,
            cx: &mut TestAppContext,
        ) -> (Harness, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            let api = Arc::new(FakeApi::with(snapshot));
            let mut dialog = None;
            let window = cx.add_window(|window, cx| {
                let api: Arc<dyn Api> = api.clone();
                let view = cx.new(|cx| NewWorkspaceDialog::new(api, window, cx));
                dialog = Some(view.clone());
                Root::new(view, window, cx)
            });
            let cx = VisualTestContext::from_window(*window, cx).into_mut();
            let dialog = dialog.unwrap();
            let created = Rc::new(RefCell::new(Vec::new()));
            let seen = created.clone();
            let _created = cx.update(|_, cx| {
                cx.subscribe(&dialog, move |_, Created(id), _| {
                    seen.borrow_mut().push(*id)
                })
            });
            cx.update(|window, _| window.activate_window());
            let offer = Offer::read(&api.snapshot_now(), None);
            cx.update(|window, cx| dialog.update(cx, |d, cx| d.open(offer, window, cx)));
            cx.run_until_parked();
            let harness = Harness {
                api,
                dialog,
                created,
                _created,
            };
            (harness, cx)
        }

        fn create(&self, cx: &mut VisualTestContext) {
            cx.update(|window, cx| self.dialog.update(cx, |d, cx| d.create(window, cx)));
            cx.run_until_parked();
        }

        fn is_open(&self, cx: &mut VisualTestContext) -> bool {
            self.dialog.read_with(cx, |d, _| d.is_open())
        }

        fn problem(&self, cx: &mut VisualTestContext) -> Option<SharedString> {
            self.dialog.read_with(cx, |d, _| d.problem().cloned())
        }

        fn made(&self) -> Vec<Request> {
            self.api.made_workspaces.lock().unwrap().clone()
        }

        fn draw(&self, cx: &mut VisualTestContext) {
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }

    mod dialog {
        use super::*;

        #[gpui::test]
        fn should_make_the_workspace_typed_and_close(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);

            cx.simulate_input("tabs-a11y");
            harness.create(cx);

            assert_eq!(
                harness.made(),
                [Request {
                    name: "tabs-a11y".into(),
                    link: vec![],
                }]
            );
            assert_eq!(*harness.created.borrow(), [id::<WorkspaceId>(101)]);
            assert!(!harness.is_open(cx));
        }

        #[gpui::test]
        fn should_make_it_on_enter(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);

            cx.simulate_input("tabs-a11y");
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();

            assert_eq!(harness.made().len(), 1);
            assert_eq!(*harness.created.borrow(), [id::<WorkspaceId>(101)]);
            assert!(!harness.is_open(cx));
        }

        #[gpui::test]
        fn should_close_without_making_it_on_escape(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);

            cx.simulate_input("tabs-a11y");
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();

            assert!(harness.made().is_empty());
            assert!(!harness.is_open(cx));
        }

        #[gpui::test]
        fn should_stay_open_with_the_name_problem_on_enter_in_a_blank_name(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(cx);
            harness.draw(cx);

            cx.simulate_keystrokes("enter");
            cx.run_until_parked();

            assert_eq!(harness.problem(cx), Some(NO_NAME.into()));
            assert!(harness.is_open(cx));
            assert_eq!(harness.made(), []);
        }

        #[gpui::test]
        fn should_stay_open_with_the_daemons_refusal_on_enter(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            harness.draw(cx);
            cx.simulate_input("../x");
            harness
                .api
                .refuse_next("invalid_name", "a name can't hold a /");

            cx.simulate_keystrokes("enter");
            cx.run_until_parked();

            assert_eq!(harness.problem(cx), Some("a name can't hold a /".into()));
            assert!(harness.is_open(cx));
        }

        #[gpui::test]
        fn should_stay_open_with_the_name_problem_and_ask_nothing(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);

            harness.create(cx);

            assert_eq!(harness.problem(cx), Some(NO_NAME.into()));
            assert!(harness.is_open(cx));
            assert_eq!(harness.made(), []);
            harness.draw(cx);
        }

        #[gpui::test]
        fn should_stay_open_on_another_failure(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            cx.simulate_input("ts");
            harness.api.fail_next("connection refused");

            harness.create(cx);

            assert_eq!(harness.problem(cx), None);
            assert!(harness.is_open(cx));
            harness.draw(cx);
        }

        #[gpui::test]
        fn should_ask_for_a_name_and_optional_repositories(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            harness.draw(cx);

            assert!(cx.debug_bounds("name field").is_some());
            assert!(cx.debug_bounds("link picker").is_some());
            assert!(cx.debug_bounds("folder field").is_none());
            assert!(!DESCRIPTION.contains("folder"), "{DESCRIPTION}");
            assert!(!NAME_HINT.contains("folder"), "{NAME_HINT}");
        }

        #[gpui::test]
        fn should_open_empty_again(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            cx.simulate_input("ts");
            harness.create(cx);

            cx.update(|window, cx| {
                harness
                    .dialog
                    .update(cx, |d, cx| d.open(Offer::default(), window, cx))
            });
            harness.create(cx);

            assert_eq!(harness.problem(cx), Some(NO_NAME.into()));
        }
    }

    mod request {
        use super::*;

        #[test]
        fn should_trim_the_name() {
            assert_eq!(
                request("  tabs-a11y ", vec![]),
                Ok(Request {
                    name: "tabs-a11y".into(),
                    link: vec![],
                })
            );
        }

        #[test]
        fn should_need_a_name() {
            assert_eq!(request(" ", vec![]), Err(NO_NAME.into()));
        }
    }

    mod repositories {
        use super::*;

        /// You have a PR in acme/tools; GitHub also shows octo/hello.
        fn with_prs() -> Snapshot {
            Snapshot {
                repositories: vec![links("acme/tools", &[])],
                prs: vec![pull_request(1, "acme/tools")],
                ..Snapshot::default()
            }
        }

        fn pick(selector: &'static str, harness: &Harness, cx: &mut VisualTestContext) {
            harness.draw(cx);
            let at = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} shown"));
            cx.simulate_click(at.center(), gpui::Modifiers::default());
            cx.run_until_parked();
            harness.draw(cx);
        }

        fn type_in_picker(text: &str, harness: &Harness, cx: &mut VisualTestContext) {
            let picker = harness.dialog.read_with(cx, |d, _| d.picker().clone());
            picker.update(cx, |p, cx| p.focus(cx));
            harness.draw(cx);
            cx.run_until_parked();
            harness.draw(cx);
            cx.simulate_input(text);
            cx.run_until_parked();
            harness.draw(cx);
        }

        #[gpui::test]
        fn should_link_what_was_picked_on_create(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open_on(with_prs(), cx);
            harness.api.on_github(repository("octo/hello"));
            cx.simulate_input("tabs-a11y");
            type_in_picker("tools", &harness, cx);
            pick("pick acme/tools", &harness, cx);
            type_in_picker("octo/hello", &harness, cx);
            pick("link typed", &harness, cx);

            harness.create(cx);

            assert_eq!(
                harness.made(),
                [Request {
                    name: "tabs-a11y".into(),
                    link: vec!["acme/tools".parse().unwrap(), "octo/hello".parse().unwrap()],
                }]
            );
            let made = id::<WorkspaceId>(101);
            let linking: Vec<String> = (harness.api.snapshot_now().repositories.iter())
                .filter(|r| r.workspaces.contains(&made))
                .map(|r| r.name.to_string())
                .collect();
            assert_eq!(linking, ["acme/tools", "octo/hello"]);
            assert!(!harness.is_open(cx));
        }

        #[gpui::test]
        fn should_show_a_repository_the_daemon_cannot_link_under_the_picker(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open_on(with_prs(), cx);
            cx.simulate_input("tabs-a11y");
            type_in_picker("tools", &harness, cx);
            pick("pick acme/tools", &harness, cx);
            harness.api.refuse_next(
                "unknown_repository",
                "GitHub can't see acme/tools. Check the name or your token's access.",
            );

            harness.create(cx);
            harness.draw(cx);

            assert!(harness.is_open(cx));
            assert_eq!(harness.problem(cx), None);
            assert!(cx.debug_bounds("picker error").is_some());
        }
    }
}
