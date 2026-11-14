//! The right panel (spec §11), Penpot "Main window" `RightPanel`: the selected workspace's
//! tasks and PRs, grouped by status, each with a status picker. Done tasks hide behind
//! "Show N done". The top bar's panel button toggles it, through
//! [`ToggleRightPanel`](crate::keymap::ToggleRightPanel), which the window handles. The
//! Tasks & PRs `+` asks the window for the new task dialog on the workspace ([`NewTask`]).
//!
//! Below the tasks come the notes on the workspace and on its tasks, newest first, and a field
//! that leaves a note on the workspace for the next agent (Enter sends it; the notes header's
//! `+` focuses it). Each note's `…` menu sends it to a live agent pane at once (spec §9).
//!
//! Like the Tasks view it shows the sidebar's [`Snapshot`], passed on by the window; a note
//! left here comes back through the daemon's `changed`.

use std::collections::HashSet;
use std::sync::Arc;

use agentdeck_core::{NoteId, PaneId, TaskId, TaskStatus, WorkspaceId};
use brrritscold_core_gpui::{
    ActiveTheme as _, Badge, Button, ButtonSize, ButtonVariant, DropdownMenu, DropdownMenuContent,
    DropdownMenuItem, DropdownMenuLabel, Icon, IconName, Input, InputState, Separator,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, px,
};
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::new_task::NewTask;
use crate::note_list::{self, NoteRow, SendTarget};
use crate::overview::Snapshot;
use crate::task_list::{self, Group, Query, Scope};
use crate::task_row;
use crate::workspace::failed;

/// The panel's width. Raw in Penpot too: no sizing token is 340px.
pub const WIDTH: f32 = 340.;

/// The right panel.
pub struct RightPanel {
    api: Arc<dyn Api>,
    snapshot: Snapshot,
    workspace: Option<WorkspaceId>,
    show_done: bool,
    /// The statuses whose group is folded; Todo starts folded, as Penpot draws it.
    collapsed: HashSet<TaskStatus>,
    /// Penpot `NewNote`: the note to leave.
    new_note: Entity<InputState>,
    /// Whether the note typed is being sent; another Enter waits for it.
    sending: bool,
    _new_note: Subscription,
}

impl EventEmitter<NewTask> for RightPanel {}

impl RightPanel {
    /// A panel that changes statuses through `api`; it lists nothing until given a
    /// workspace and a snapshot.
    pub fn new(api: Arc<dyn Api>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let new_note = cx
            .new(|cx| InputState::new(window, cx).placeholder("Leave a note for the next agent…"));
        let _new_note = cx.subscribe_in(&new_note, window, |this, _, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.leave_note(window, cx);
            }
        });
        RightPanel {
            api,
            snapshot: Snapshot::default(),
            workspace: None,
            show_done: false,
            collapsed: HashSet::from([TaskStatus::Todo]),
            new_note,
            sending: false,
            _new_note,
        }
    }

    /// Leaves the note typed on the workspace, then empties the field; a failure keeps the
    /// text and shows a toast.
    pub fn leave_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let body = self.new_note.read(cx).value().trim().to_owned();
        let Some(workspace) = self.workspace.filter(|_| !body.is_empty() && !self.sending) else {
            return;
        };
        self.sending = true;
        let leaving = self.api.new_note(workspace, body, cx);
        cx.spawn_in(window, async move |this, cx| {
            let left = leaving.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                match left {
                    Ok(()) => this
                        .new_note
                        .update(cx, |input, cx| input.set_value("", window, cx)),
                    Err(err) => failed("Couldn't leave the note", &err, window, cx),
                }
            });
        })
        .detach();
    }

    /// Sends `note` to `pane` at once and marks it read by that pane: what a "Send to agent"
    /// item does. A failure shows a toast.
    pub fn send_note(
        &mut self,
        note: NoteId,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(body) = (self.snapshot.notes.iter())
            .find(|n| n.id == note)
            .map(|n| n.body.clone())
        else {
            return;
        };
        let sending = self.api.send_note(note, body, pane, cx);
        cx.spawn_in(window, async move |_, cx| {
            if let Err(err) = sending.await {
                let _ = cx.update(|window, cx| {
                    failed("Couldn't send the note", &err, window, cx);
                });
            }
        })
        .detach();
    }

    /// The notes listed.
    pub fn notes(&self) -> Vec<NoteRow> {
        match self.workspace {
            Some(workspace) => note_list::notes(&self.snapshot, workspace, chrono::Utc::now()),
            None => Vec::new(),
        }
    }

    /// Lists `workspace`'s tasks from `snapshot`.
    pub fn show(&mut self, workspace: WorkspaceId, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.workspace = Some(workspace);
        self.snapshot = snapshot;
        cx.notify();
    }

    /// Folds `status`'s group, or unfolds it.
    pub fn toggle(&mut self, status: TaskStatus, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&status) {
            self.collapsed.insert(status);
        }
        cx.notify();
    }

    /// Shows or hides done tasks: what "Show N done" does.
    pub fn set_show_done(&mut self, show: bool, cx: &mut Context<Self>) {
        self.show_done = show;
        cx.notify();
    }

    /// Moves `task` to `status`, reporting a failure in a toast.
    pub fn set_status(
        &mut self,
        task: TaskId,
        status: TaskStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let setting = self.api.set_task_status(task, status, cx);
        cx.spawn_in(window, async move |_, cx| {
            if let Err(err) = setting.await {
                let _ = cx.update(|window, cx| {
                    failed("Couldn't change the status", &err, window, cx);
                });
            }
        })
        .detach();
    }

    /// The workspace's groups, done ones only when shown, before folding.
    pub fn groups(&self) -> Vec<Group> {
        let Some(workspace) = self.workspace else {
            return Vec::new();
        };
        let query = Query {
            scope: Scope::Workspace(workspace),
            filter: "",
            show_done: true,
        };
        task_list::groups(&self.snapshot, &query)
            .into_iter()
            .filter(|group| self.show_done || group.status != TaskStatus::Done)
            .collect()
    }

    /// How many of the workspace's tasks are done.
    fn done(&self) -> usize {
        let Some(workspace) = self.workspace else {
            return 0;
        };
        (self.snapshot.tasks.iter())
            .filter(|t| t.workspace_id == Some(workspace) && t.status == TaskStatus::Done)
            .count()
    }

    /// Whether `status`'s group is folded.
    pub fn is_collapsed(&self, status: TaskStatus) -> bool {
        self.collapsed.contains(&status)
    }
}

impl Render for RightPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div()
            .id("panel-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(space::S1)
            .overflow_y_scroll();
        for group in self.groups() {
            let status = group.status;
            let collapsed = self.is_collapsed(status);
            list = list.child(task_row::group_header(
                status.to_string(),
                task_row::status_label(status),
                group.rows.len(),
                collapsed,
                cx.listener(move |this, _, _, cx| this.toggle(status, cx)),
                cx,
            ));
            if collapsed {
                continue;
            }
            for row in &group.rows {
                let task = row.id;
                let picker = task_row::status_picker(
                    SharedString::from(format!("panel-status-{task}")),
                    row.status,
                    cx.listener(move |this, status: &TaskStatus, window, cx| {
                        this.set_status(task, *status, window, cx);
                    }),
                );
                list = list.child(task_row::panel(row, picker, cx));
            }
        }
        // Penpot `ShowDone`: a link button at xs, at the start of its row, after the groups.
        let done = self.done();
        let show_done = !self.show_done;
        if done > 0 {
            let label = if self.show_done {
                "Hide done".to_owned()
            } else {
                format!("Show {done} done")
            };
            list = list.child(
                div().flex().flex_none().px(space::S2).child(
                    div().debug_selector(|| "show done".into()).child(
                        Button::new("panel-show-done")
                            .variant(ButtonVariant::Link)
                            .size(ButtonSize::Xs)
                            .label(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_show_done(show_done, cx);
                            })),
                    ),
                ),
            );
        }
        // Then Separator, the notes header with its `+`, and each note.
        let s = cx.theme().semantic();
        list = list
            .child(Separator::new().my(space::S1))
            .child(
                section_header("Notes", s).child(
                    div().debug_selector(|| "add note".into()).child(
                        Button::new("add-note")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::IconXs)
                            .icon(IconName::Plus)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.new_note
                                    .update(cx, |input, cx| input.focus(window, cx));
                            })),
                    ),
                ),
            )
            .children(self.notes().iter().map(|note| {
                let targets = note_list::send_targets(&self.snapshot, note.id);
                note_item(note, targets, cx)
            }));
        // Penpot `RightPanel`: `card`, padded `space.3` down and `space.2` across, rows
        // `space.1` apart.
        div()
            .flex_none()
            .w(px(WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .gap(space::S1)
            .py(space::S3)
            .px(space::S2)
            .bg(s.card)
            .child(
                section_header("Tasks & PRs", s).child(
                    div().debug_selector(|| "add task".into()).child(
                        Button::new("add-task")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::IconXs)
                            .icon(IconName::Plus)
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.emit(NewTask(this.workspace));
                            })),
                    ),
                ),
            )
            .child(list)
            // Penpot `NewNote`.
            .child(div().flex_none().child(Input::new(&self.new_note)))
    }
}

/// Penpot's "Tasks & PRs header" and "Notes header": sm medium, 32px, padded `space.1` /
/// `space.2`, its children after a spacer.
fn section_header(title: &'static str, s: &brrritscold_core_gpui_tokens::Semantic) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(space::S2)
        .h(space::S8)
        .py(space::S1)
        .px(space::S2)
        .text_size(font_size::SM)
        .font_weight(font_weight::MEDIUM)
        .text_color(s.foreground)
        .child(title)
        .child(div().flex_1())
}

/// Penpot `agentdeck / NoteItem`: the text over a meta line (sticky note, what it is on and
/// when or by whom it was read, an `unread` badge, the note's menu), in a `border` box with
/// `radius-md`.
fn note_item(note: &NoteRow, targets: Vec<SendTarget>, cx: &Context<RightPanel>) -> AnyElement {
    let s = cx.theme().semantic();
    let id = note.id;
    div()
        .debug_selector(move || format!("note {id}"))
        .flex()
        .flex_col()
        .flex_none()
        .gap(space::S1_5)
        .px(space::S3)
        .py(space::S2)
        .border_1()
        .border_color(s.border)
        .rounded(s.radius_md)
        .child(
            div()
                .text_size(font_size::SM)
                .text_color(s.foreground)
                .child(note.body.clone()),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(space::S1_5)
                .text_size(font_size::XS)
                .text_color(s.muted_foreground)
                .child(
                    Icon::new(IconName::StickyNote)
                        .size(space::S3)
                        .text_color(s.foreground),
                )
                .child(div().min_w_0().truncate().child(note.meta.clone()))
                .child(div().flex_1())
                .when(note.unread, |meta| meta.child(Badge::new().label("unread")))
                .child(note_menu(id, targets, cx)),
        )
        .into_any_element()
}

/// Penpot "Send to agent": a ghost icon-xs `Ellipsis` opening a `DropdownMenu` with the label
/// "Send to agent", then an item per live agent pane `targets` names; with none, one
/// disabled "No agent is running here".
fn note_menu(note: NoteId, targets: Vec<SendTarget>, cx: &Context<RightPanel>) -> AnyElement {
    let panel = cx.entity().downgrade();
    let content = DropdownMenuContent::new().label(DropdownMenuLabel::new("Send to agent"));
    let content = if targets.is_empty() {
        content.item(DropdownMenuItem::new("No agent is running here").disabled(true))
    } else {
        targets.into_iter().fold(content, |content, target| {
            let panel = panel.clone();
            let pane = target.pane;
            content.item(
                DropdownMenuItem::new(target.label).on_click(move |_, window, cx| {
                    let _ = panel.update(cx, |panel, cx| panel.send_note(note, pane, window, cx));
                }),
            )
        })
    };
    div()
        .debug_selector(move || format!("note menu {note}"))
        .child(
            DropdownMenu::new(SharedString::from(format!("note-menu-{note}")))
                .trigger(
                    Button::new(SharedString::from(format!("note-menu-trigger-{note}")))
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::IconXs)
                        .icon(IconName::Ellipsis),
                )
                .content(content),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use agentdeck_client::types::Task;
    use agentdeck_core::TaskKind;
    use brrritscold_core_gpui::{Root, Theme, ThemePreference};
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::new_task::NewTask;
    use crate::overview::fixtures::{id, task, workspace};

    fn with_status(n: u32, status: TaskStatus, ws: WorkspaceId) -> Task {
        Task {
            status,
            ..task(n, TaskKind::Plain, Some(ws))
        }
    }

    /// Workspace 1 with a doing, a todo and a done task; workspace 2 with one of its own.
    fn snapshot() -> Snapshot {
        let (a, b) = (workspace(1, "a"), workspace(2, "b"));
        Snapshot {
            tasks: vec![
                with_status(1, TaskStatus::Doing, a.id),
                with_status(2, TaskStatus::Todo, a.id),
                with_status(3, TaskStatus::Done, a.id),
                with_status(4, TaskStatus::Doing, b.id),
            ],
            workspaces: vec![a, b],
            ..Snapshot::default()
        }
    }

    fn open(cx: &mut TestAppContext) -> (Entity<RightPanel>, &mut VisualTestContext) {
        let (panel, _, cx) = open_with_api(cx);
        (panel, cx)
    }

    fn open_with_api(
        cx: &mut TestAppContext,
    ) -> (Entity<RightPanel>, Arc<FakeApi>, &mut VisualTestContext) {
        cx.update(brrritscold_core_gpui::init);
        let fake = Arc::new(FakeApi::default());
        let api: Arc<dyn Api> = fake.clone();
        let mut panel = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = RightPanel::new(api, window, cx);
                view.show(id(1), snapshot(), cx);
                view
            });
            panel = Some(view.clone());
            Root::new(view, window, cx)
        });
        let cx = VisualTestContext::from_window(*window, cx).into_mut();
        cx.run_until_parked();
        (panel.unwrap(), fake, cx)
    }

    /// Types `text` into the note field and presses Enter.
    fn leave(panel: &Entity<RightPanel>, text: &str, cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            let input = panel.read(cx).new_note.clone();
            input.update(cx, |input, cx| {
                input.set_value(text.to_owned(), window, cx);
                input.focus(window, cx);
            });
        });
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
    }

    fn typed(panel: &Entity<RightPanel>, cx: &mut VisualTestContext) -> String {
        panel.read_with(cx, |p, cx| p.new_note.read(cx).value().to_string())
    }

    mod notes {
        use agentdeck_core::NoteId;

        use super::*;
        use crate::overview::fixtures::workspace_note;

        #[gpui::test]
        fn should_list_the_workspaces_notes(cx: &mut TestAppContext) {
            let (panel, cx) = open(cx);
            panel.update(cx, |p, cx| {
                let mut snapshot = snapshot();
                snapshot.notes = vec![workspace_note(30, id(1)), workspace_note(31, id(2))];
                p.show(id(1), snapshot, cx);
            });

            let notes: Vec<_> =
                panel.read_with(cx, |p, _| p.notes().into_iter().map(|n| n.id).collect());
            assert_eq!(notes, [id(30)]);
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        /// [`open_with_api`], showing a note on workspace 1.
        fn with_a_note(
            cx: &mut TestAppContext,
        ) -> (Entity<RightPanel>, Arc<FakeApi>, &mut VisualTestContext) {
            let (panel, api, cx) = open_with_api(cx);
            panel.update(cx, |p, cx| {
                let mut snapshot = snapshot();
                snapshot.notes = vec![workspace_note(30, id(1))];
                p.show(id(1), snapshot, cx);
            });
            (panel, api, cx)
        }

        #[gpui::test]
        fn should_end_each_notes_meta_row_with_its_menu(cx: &mut TestAppContext) {
            let (_, _, cx) = with_a_note(cx);
            cx.update(|window, cx| window.draw(cx).clear(cx));

            let selector = format!("note menu {}", id::<NoteId>(30));
            assert!(cx.debug_bounds(selector.leak()).is_some());
        }

        #[gpui::test]
        fn should_send_the_note_to_the_pane_picked(cx: &mut TestAppContext) {
            let (panel, api, cx) = with_a_note(cx);

            cx.update(|window, cx| {
                panel.update(cx, |p, cx| p.send_note(id(30), id(40), window, cx));
            });
            cx.run_until_parked();

            assert_eq!(
                *api.sent_notes.lock().unwrap(),
                [(id(30), "note".to_owned(), id(40))]
            );
        }

        #[gpui::test]
        fn should_toast_when_the_note_cant_be_sent(cx: &mut TestAppContext) {
            let (panel, api, cx) = with_a_note(cx);
            api.fail_next("pane not found");

            cx.update(|window, cx| {
                panel.update(cx, |p, cx| p.send_note(id(30), id(40), window, cx));
            });
            cx.run_until_parked();

            let shown = cx.update(|_, cx| crate::notice::shown(cx));
            assert_eq!(shown.last().unwrap().title, "Couldn't send the note");
        }

        #[gpui::test]
        fn should_leave_the_note_typed_on_enter_and_empty_the_field(cx: &mut TestAppContext) {
            let (panel, api, cx) = open_with_api(cx);

            leave(&panel, "  Push after 20 green runs ", cx);

            assert_eq!(
                *api.notes_left.lock().unwrap(),
                [(id(1), "Push after 20 green runs".to_owned())]
            );
            assert_eq!(typed(&panel, cx), "");
        }

        #[gpui::test]
        fn should_leave_nothing_for_a_blank_field(cx: &mut TestAppContext) {
            let (panel, api, cx) = open_with_api(cx);

            leave(&panel, "   ", cx);

            assert!(api.notes_left.lock().unwrap().is_empty());
        }

        #[gpui::test]
        fn should_keep_the_text_when_the_note_cant_be_left(cx: &mut TestAppContext) {
            let (panel, api, cx) = open_with_api(cx);
            api.fail_next("refused");

            leave(&panel, "keep me", cx);

            assert_eq!(typed(&panel, cx), "keep me");
        }
    }

    fn statuses(panel: &Entity<RightPanel>, cx: &mut VisualTestContext) -> Vec<TaskStatus> {
        panel.read_with(cx, |p, _| p.groups().iter().map(|g| g.status).collect())
    }

    #[gpui::test]
    fn should_list_the_workspaces_open_tasks_with_todo_folded(cx: &mut TestAppContext) {
        let (panel, cx) = open(cx);

        assert_eq!(statuses(&panel, cx), [TaskStatus::Doing, TaskStatus::Todo]);
        let rows: Vec<TaskId> = panel.read_with(cx, |p, _| {
            (p.groups().iter())
                .flat_map(|g| g.rows.iter().map(|r| r.id))
                .collect()
        });
        assert_eq!(rows, [id(1), id(2)]);
        assert!(panel.read_with(cx, |p, _| p.is_collapsed(TaskStatus::Todo)));
    }

    #[gpui::test]
    fn should_ask_for_the_new_task_dialog_on_its_workspace_from_the_plus(cx: &mut TestAppContext) {
        let (panel, cx) = open(cx);
        let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = asked.clone();
        let _asks = cx.update(|_, cx| {
            cx.subscribe(&panel, move |_, event: &NewTask, _| {
                seen.borrow_mut().push(*event)
            })
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let plus = cx.debug_bounds("add task").expect("the Tasks & PRs +");
        cx.simulate_click(plus.center(), Modifiers::default());
        cx.run_until_parked();

        assert_eq!(*asked.borrow(), [NewTask(Some(id(1)))]);
    }

    #[gpui::test]
    fn should_show_done_tasks_from_the_link(cx: &mut TestAppContext) {
        let (panel, cx) = open(cx);
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let link = cx.debug_bounds("show done").expect("the show done link");
        cx.simulate_click(link.center(), Modifiers::default());
        cx.run_until_parked();

        assert_eq!(statuses(&panel, cx).last(), Some(&TaskStatus::Done));
    }

    #[gpui::test]
    fn should_draw_in_both_themes(cx: &mut TestAppContext) {
        let (panel, cx) = open(cx);
        panel.update(cx, |p, cx| p.toggle(TaskStatus::Todo, cx));

        for preference in [ThemePreference::Light, ThemePreference::Dark] {
            cx.update(|_, cx| Theme::set_preference(preference, cx));
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }
}
