//! The new workspace dialog (spec §11), Penpot "Screens" → "New workspace": a name and an
//! optional existing folder. Without a folder the daemon makes a scratch one.
//!
//! It checks what it can before asking ([`request`]), shows the daemon's `invalid_name` or
//! `invalid_dir` under the field it names, and reports any other failure in a toast. Once the
//! workspace is made it closes and emits [`Created`].

use std::path::Path;
use std::sync::Arc;

use agentdeck_client::ApiError;
use agentdeck_core::WorkspaceId;
use brrritscold_core_gpui::{
    Button, ButtonVariant, Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter,
    DialogHandle, DialogHeader, DialogTitle, Field, FieldDescription, FieldError, Input,
    InputState, Label,
};
use brrritscold_core_gpui_tokens::primitive::space;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Window, div,
};
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::form_enter::form_enter;
use crate::workspace::failed;

/// The dialog made a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Created(pub WorkspaceId);

/// The dialog, kept by the window and opened from the sidebar's `+`.
pub struct NewWorkspaceDialog {
    api: Arc<dyn Api>,
    handle: DialogHandle,
    name: Entity<InputState>,
    dir: Entity<InputState>,
    /// Why the last try failed, against its field.
    problem: Option<Problem>,
    /// Whether a request is out; another waits for it.
    creating: bool,
    /// Whether the name field takes the keyboard once the dialog shows.
    focus_name: bool,
    _enter: [Subscription; 2],
}

impl EventEmitter<Created> for NewWorkspaceDialog {}

impl NewWorkspaceDialog {
    /// A closed dialog that makes workspaces through `api`.
    pub fn new(api: Arc<dyn Api>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. tabs-a11y"));
        let dir = cx.new(|cx| InputState::new(window, cx).placeholder("~/Code/…"));
        let on_enter = |cx: &mut Context<Self>, input: &Entity<InputState>, window: &mut Window| {
            cx.subscribe_in(input, window, |this, _, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.create(window, cx);
                }
            })
        };
        let _enter = [on_enter(cx, &name, window), on_enter(cx, &dir, window)];
        NewWorkspaceDialog {
            api,
            handle: DialogHandle::new(false),
            name,
            dir,
            problem: None,
            creating: false,
            focus_name: false,
            _enter,
        }
    }

    /// Opens it empty, the name field focused.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for input in [&self.name, &self.dir] {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.problem = None;
        self.handle.open(window, cx);
        self.focus_name = true;
        cx.notify();
    }

    /// Whether it is open.
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.handle.is_open()
    }

    /// Why the last try failed, if it did.
    #[cfg(test)]
    pub fn problem(&self) -> Option<&Problem> {
        self.problem.as_ref()
    }

    /// Asks the daemon for the workspace the form describes: what its button and Enter do.
    pub fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.creating {
            return;
        }
        let (name, dir) = (self.name.read(cx).value(), self.dir.read(cx).value());
        let request = match request(&name, &dir, std::env::home_dir().as_deref()) {
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
                        None => failed("Couldn't make the workspace", &err, window, cx),
                    },
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Penpot `NewWorkspaceForm`: two vertical fields, `space.4` apart.
    fn form(&self) -> impl IntoElement {
        let (name_problem, dir_problem) = match &self.problem {
            Some(Problem::Name(why)) => (Some(why.clone()), None),
            Some(Problem::Dir(why)) => (None, Some(why.clone())),
            None => (None, None),
        };
        let field = |label: &'static str,
                     input: &Entity<InputState>,
                     description: &'static str,
                     problem: Option<SharedString>| {
            Field::new()
                .child(Label::new().child(label))
                .child(form_enter(
                    Input::new(input).aria_invalid(problem.is_some()),
                ))
                .child(FieldDescription::new(description))
                .when_some(problem, |field, why| field.child(FieldError::new(why)))
        };
        div()
            .flex()
            .flex_col()
            .gap(space::S4)
            .child(field(
                "Name",
                &self.name,
                "Names the folder and the sidebar entry.",
                name_problem,
            ))
            .child(field(
                "Existing folder (optional)",
                &self.dir,
                "Leave it empty for a scratch folder under ~/.local/share/agentdeck/workspaces.",
                dir_problem,
            ))
    }
}

/// The field a daemon refusal names, if it names one.
fn refusal(err: &anyhow::Error) -> Option<Problem> {
    let err = err.downcast_ref::<ApiError>()?;
    let why = SharedString::from(err.message.clone());
    match err.code.as_deref()? {
        "invalid_name" => Some(Problem::Name(why)),
        "invalid_dir" => Some(Problem::Dir(why)),
        _ => None,
    }
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
                            .description(DialogDescription::new(
                                "A folder for any number of repos and agents.",
                            )),
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
    /// The folder to attach, absolute; `None` for a scratch folder.
    pub dir: Option<String>,
}

/// Why the form can't be sent, against the field it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The name field's.
    Name(SharedString),
    /// The folder field's.
    Dir(SharedString),
}

/// The request the form's `name` and `dir` make, `~` standing for `home`; or what's wrong
/// with them. The daemon checks that the folder exists.
pub fn request(name: &str, dir: &str, home: Option<&Path>) -> Result<Request, Problem> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Problem::Name("Give the workspace a name.".into()));
    }
    let dir = match dir.trim() {
        "" => None,
        dir => Some(absolute(dir, home)?),
    };
    Ok(Request {
        name: name.to_owned(),
        dir,
    })
}

/// `dir` with a leading `~` read as `home`, if it then is absolute.
fn absolute(dir: &str, home: Option<&Path>) -> Result<String, Problem> {
    let expanded = match dir.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            let home = home.ok_or_else(|| Problem::Dir("No home folder to read ~ as.".into()))?;
            format!("{}{rest}", home.display())
        }
        _ => dir.to_owned(),
    };
    if Path::new(&expanded).is_absolute() {
        Ok(expanded)
    } else {
        Err(Problem::Dir("Give the full path, from / or ~.".into()))
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
    use crate::overview::fixtures::id;

    struct Harness {
        api: Arc<FakeApi>,
        dialog: Entity<NewWorkspaceDialog>,
        created: Rc<RefCell<Vec<WorkspaceId>>>,
        _created: Subscription,
    }

    impl Harness {
        /// A window holding the dialog, opened.
        fn open(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            let api = Arc::new(FakeApi::default());
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
            cx.update(|window, cx| dialog.update(cx, |d, cx| d.open(window, cx)));
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

        fn set_dir(&self, dir: &str, cx: &mut VisualTestContext) {
            cx.update(|window, cx| {
                let input = self.dialog.read(cx).dir.clone();
                input.update(cx, |input, cx| input.set_value(dir.to_owned(), window, cx));
            });
        }

        fn is_open(&self, cx: &mut VisualTestContext) -> bool {
            self.dialog.read_with(cx, |d, _| d.is_open())
        }

        fn problem(&self, cx: &mut VisualTestContext) -> Option<Problem> {
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
                    dir: None
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
        fn should_stay_open_with_the_name_problem_on_enter_in_a_blank_name(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(cx);
            harness.draw(cx);

            cx.simulate_keystrokes("enter");
            cx.run_until_parked();

            assert!(matches!(harness.problem(cx), Some(Problem::Name(_))));
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

            assert_eq!(
                harness.problem(cx),
                Some(Problem::Name("a name can't hold a /".into()))
            );
            assert!(harness.is_open(cx));
        }

        #[gpui::test]
        fn should_stay_open_on_enter_in_the_folder_without_a_name(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            harness.draw(cx);
            cx.update(|window, cx| {
                let dir = harness.dialog.read(cx).dir.clone();
                dir.update(cx, |dir, cx| dir.focus(window, cx));
            });
            cx.simulate_input("/srv/ts");

            cx.simulate_keystrokes("enter");
            cx.run_until_parked();

            assert!(matches!(harness.problem(cx), Some(Problem::Name(_))));
            assert!(harness.is_open(cx));
        }

        #[gpui::test]
        fn should_attach_the_folder_given(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            cx.simulate_input("ts");
            harness.set_dir("/srv/ts", cx);

            harness.create(cx);

            assert_eq!(harness.made()[0].dir.as_deref(), Some("/srv/ts"));
        }

        #[gpui::test]
        fn should_stay_open_with_the_name_problem_and_ask_nothing(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);

            harness.create(cx);

            assert!(matches!(harness.problem(cx), Some(Problem::Name(_))));
            assert!(harness.is_open(cx));
            assert_eq!(harness.made(), []);
            harness.draw(cx);
        }

        #[gpui::test]
        fn should_put_the_daemons_folder_refusal_under_the_folder(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            cx.simulate_input("ts");
            harness.set_dir("/nope", cx);
            harness
                .api
                .refuse_next("invalid_dir", "/nope is not a directory");

            harness.create(cx);

            assert_eq!(
                harness.problem(cx),
                Some(Problem::Dir("/nope is not a directory".into()))
            );
            assert!(harness.is_open(cx));
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
        fn should_open_empty_again(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            cx.simulate_input("ts");
            harness.create(cx);

            cx.update(|window, cx| harness.dialog.update(cx, |d, cx| d.open(window, cx)));
            harness.create(cx);

            assert!(matches!(harness.problem(cx), Some(Problem::Name(_))));
        }
    }

    mod request {
        use super::*;

        fn home() -> Option<&'static Path> {
            Some(Path::new("/home/ada"))
        }

        #[test]
        fn should_ask_for_a_scratch_folder_when_none_is_given() {
            assert_eq!(
                request("tabs-a11y", "  ", home()),
                Ok(Request {
                    name: "tabs-a11y".into(),
                    dir: None
                })
            );
        }

        #[test]
        fn should_trim_the_name_and_the_folder() {
            assert_eq!(
                request("  tabs-a11y ", " /srv/code ", home()),
                Ok(Request {
                    name: "tabs-a11y".into(),
                    dir: Some("/srv/code".into())
                })
            );
        }

        #[test]
        fn should_read_a_tilde_as_the_home_folder() {
            let dir = |input| request("x", input, home()).map(|r| r.dir);

            assert_eq!(dir("~"), Ok(Some("/home/ada".into())));
            assert_eq!(dir("~/Code/ts"), Ok(Some("/home/ada/Code/ts".into())));
        }

        #[test]
        fn should_need_a_name() {
            assert!(matches!(request(" ", "", home()), Err(Problem::Name(_))));
        }

        #[test]
        fn should_refuse_a_relative_folder() {
            assert!(matches!(
                request("x", "Code/ts", home()),
                Err(Problem::Dir(_))
            ));
        }

        #[test]
        fn should_refuse_a_tilde_without_a_home_folder() {
            assert!(matches!(request("x", "~/Code", None), Err(Problem::Dir(_))));
        }
    }
}
