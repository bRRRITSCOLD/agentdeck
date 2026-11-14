//! The right panel (spec §11), Penpot "Main window" `RightPanel`: the selected workspace's
//! tasks and PRs, grouped by status, each with a status picker. Done tasks hide behind
//! "Show N done". The top bar's panel button toggles it, through
//! [`ToggleRightPanel`](crate::keymap::ToggleRightPanel), which the window handles. The
//! Tasks & PRs `+` asks the window for the new task dialog on the workspace ([`NewTask`]).
//!
//! Below the tasks come the workspace's instructions, newest first, and a field that adds one
//! for every agent in the workspace (Enter sends it; the Instructions header's `+` focuses
//! it). Each instruction's `…` menu sends it to a live agent pane at once (spec §9).
//!
//! Interim (issue #84): these are the old note cards and field, reworded, until the
//! Instructions panel's own design lands.
//!
//! Like the Tasks view it shows the sidebar's [`Snapshot`], passed on by the window; an
//! instruction added here comes back through the daemon's `changed`.

use std::collections::HashSet;
use std::sync::Arc;

use agentdeck_core::{InstructionId, PaneId, TaskId, TaskStatus, WorkspaceId};
use brrritscold_core_gpui::{
    ActiveTheme as _, Button, ButtonSize, ButtonVariant, DropdownMenu, DropdownMenuContent,
    DropdownMenuItem, DropdownMenuLabel, Icon, IconName, Input, InputState, Separator,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, px,
};
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::instruction_list::{self, InstructionRow, SendTarget};
use crate::new_task::NewTask;
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
    /// The instruction to add (Penpot `NewNote` for now).
    new_instruction: Entity<InputState>,
    /// Whether the instruction typed is being sent; another Enter waits for it.
    sending: bool,
    _new_instruction: Subscription,
}

impl EventEmitter<NewTask> for RightPanel {}

impl RightPanel {
    /// A panel that changes statuses through `api`; it lists nothing until given a
    /// workspace and a snapshot.
    pub fn new(api: Arc<dyn Api>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let new_instruction = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Add an instruction for agents here…")
        });
        let _new_instruction =
            cx.subscribe_in(&new_instruction, window, |this, _, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.add_instruction(window, cx);
                }
            });
        RightPanel {
            api,
            snapshot: Snapshot::default(),
            workspace: None,
            show_done: false,
            collapsed: HashSet::from([TaskStatus::Todo]),
            new_instruction,
            sending: false,
            _new_instruction,
        }
    }

    /// Adds the instruction typed to the workspace, then empties the field; a failure keeps
    /// the text and shows a toast.
    pub fn add_instruction(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let body = self.new_instruction.read(cx).value().trim().to_owned();
        let Some(workspace) = self.workspace.filter(|_| !body.is_empty() && !self.sending) else {
            return;
        };
        self.sending = true;
        let adding = self.api.new_instruction(workspace, body, cx);
        cx.spawn_in(window, async move |this, cx| {
            let added = adding.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                match added {
                    Ok(()) => this
                        .new_instruction
                        .update(cx, |input, cx| input.set_value("", window, cx)),
                    Err(err) => failed("Couldn't add the instruction", &err, window, cx),
                }
            });
        })
        .detach();
    }

    /// Types `instruction` into `pane` at once: what a "Send to agent" item does. A failure
    /// shows a toast.
    pub fn send_instruction(
        &mut self,
        instruction: InstructionId,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(body) = (self.snapshot.instructions.iter())
            .find(|i| i.id == instruction)
            .map(|i| i.body.clone())
        else {
            return;
        };
        let sending = self.api.send_instruction(body, pane, cx);
        cx.spawn_in(window, async move |_, cx| {
            if let Err(err) = sending.await {
                let _ = cx.update(|window, cx| {
                    failed("Couldn't send the instruction", &err, window, cx);
                });
            }
        })
        .detach();
    }

    /// The instructions listed.
    pub fn instructions(&self) -> Vec<InstructionRow> {
        match self.workspace {
            Some(workspace) => {
                instruction_list::instructions(&self.snapshot, workspace, chrono::Utc::now())
            }
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
        // Then Separator, the Instructions header with its `+`, and each instruction.
        let s = cx.theme().semantic();
        list = list
            .child(Separator::new().my(space::S1))
            .child(
                section_header("Instructions", s).child(
                    div().debug_selector(|| "add instruction".into()).child(
                        Button::new("add-instruction")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::IconXs)
                            .icon(IconName::Plus)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.new_instruction
                                    .update(cx, |input, cx| input.focus(window, cx));
                            })),
                    ),
                ),
            )
            .children(self.instructions().iter().map(|instruction| {
                let targets = instruction_list::send_targets(&self.snapshot, instruction.id);
                instruction_item(instruction, targets, cx)
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
            // Penpot `NewNote`, for now.
            .child(div().flex_none().child(Input::new(&self.new_instruction)))
    }
}

/// Penpot's "Tasks & PRs header" and "Notes header" (titled Instructions for now): sm
/// medium, 32px, padded `space.1` / `space.2`, its children after a spacer.
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

/// Penpot `agentdeck / NoteItem`, for now: the text over a meta line (sticky note, when it
/// was added, the instruction's menu), in a `border` box with `radius-md`.
fn instruction_item(
    instruction: &InstructionRow,
    targets: Vec<SendTarget>,
    cx: &Context<RightPanel>,
) -> AnyElement {
    let s = cx.theme().semantic();
    let id = instruction.id;
    div()
        .debug_selector(move || format!("instruction {id}"))
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
                .child(instruction.body.clone()),
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
                .child(div().min_w_0().truncate().child(instruction.meta.clone()))
                .child(div().flex_1())
                .child(instruction_menu(id, targets, cx)),
        )
        .into_any_element()
}

/// Penpot "Send to agent": a ghost icon-xs `Ellipsis` opening a `DropdownMenu` with the label
/// "Send to agent", then an item per live agent pane `targets` names; with none, one
/// disabled "No agent is running here".
fn instruction_menu(
    instruction: InstructionId,
    targets: Vec<SendTarget>,
    cx: &Context<RightPanel>,
) -> AnyElement {
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
                    let _ = panel.update(cx, |panel, cx| {
                        panel.send_instruction(instruction, pane, window, cx)
                    });
                }),
            )
        })
    };
    div()
        .debug_selector(move || format!("instruction menu {instruction}"))
        .child(
            DropdownMenu::new(SharedString::from(format!(
                "instruction-menu-{instruction}"
            )))
            .trigger(
                Button::new(SharedString::from(format!(
                    "instruction-menu-trigger-{instruction}"
                )))
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

    /// Types `text` into the instruction field and presses Enter.
    fn add(panel: &Entity<RightPanel>, text: &str, cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            let input = panel.read(cx).new_instruction.clone();
            input.update(cx, |input, cx| {
                input.set_value(text.to_owned(), window, cx);
                input.focus(window, cx);
            });
        });
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
    }

    fn typed(panel: &Entity<RightPanel>, cx: &mut VisualTestContext) -> String {
        panel.read_with(cx, |p, cx| p.new_instruction.read(cx).value().to_string())
    }

    mod instructions {
        use agentdeck_core::InstructionId;

        use super::*;
        use crate::overview::fixtures::instruction;

        #[gpui::test]
        fn should_list_the_workspaces_instructions(cx: &mut TestAppContext) {
            let (panel, cx) = open(cx);
            panel.update(cx, |p, cx| {
                let mut snapshot = snapshot();
                snapshot.instructions = vec![instruction(30, id(1)), instruction(31, id(2))];
                p.show(id(1), snapshot, cx);
            });

            let listed: Vec<_> = panel.read_with(cx, |p, _| {
                p.instructions().into_iter().map(|i| i.id).collect()
            });
            assert_eq!(listed, [id(30)]);
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        /// [`open_with_api`], showing an instruction for workspace 1.
        fn with_an_instruction(
            cx: &mut TestAppContext,
        ) -> (Entity<RightPanel>, Arc<FakeApi>, &mut VisualTestContext) {
            let (panel, api, cx) = open_with_api(cx);
            panel.update(cx, |p, cx| {
                let mut snapshot = snapshot();
                snapshot.instructions = vec![instruction(30, id(1))];
                p.show(id(1), snapshot, cx);
            });
            (panel, api, cx)
        }

        #[gpui::test]
        fn should_end_each_instructions_meta_row_with_its_menu(cx: &mut TestAppContext) {
            let (_, _, cx) = with_an_instruction(cx);
            cx.update(|window, cx| window.draw(cx).clear(cx));

            let selector = format!("instruction menu {}", id::<InstructionId>(30));
            assert!(cx.debug_bounds(selector.leak()).is_some());
        }

        #[gpui::test]
        fn should_send_the_instruction_to_the_pane_picked(cx: &mut TestAppContext) {
            let (panel, api, cx) = with_an_instruction(cx);

            cx.update(|window, cx| {
                panel.update(cx, |p, cx| p.send_instruction(id(30), id(40), window, cx));
            });
            cx.run_until_parked();

            assert_eq!(
                *api.sent_instructions.lock().unwrap(),
                [("instruction".to_owned(), id(40))]
            );
        }

        #[gpui::test]
        fn should_toast_when_the_instruction_cant_be_sent(cx: &mut TestAppContext) {
            let (panel, api, cx) = with_an_instruction(cx);
            api.fail_next("pane not found");

            cx.update(|window, cx| {
                panel.update(cx, |p, cx| p.send_instruction(id(30), id(40), window, cx));
            });
            cx.run_until_parked();

            let shown = cx.update(|_, cx| crate::notice::shown(cx));
            assert_eq!(shown.last().unwrap().title, "Couldn't send the instruction");
        }

        #[gpui::test]
        fn should_add_the_instruction_typed_on_enter_and_empty_the_field(cx: &mut TestAppContext) {
            let (panel, api, cx) = open_with_api(cx);

            add(&panel, "  Push after 20 green runs ", cx);

            assert_eq!(
                *api.instructions_added.lock().unwrap(),
                [(id(1), "Push after 20 green runs".to_owned())]
            );
            assert_eq!(typed(&panel, cx), "");
        }

        #[gpui::test]
        fn should_add_nothing_for_a_blank_field(cx: &mut TestAppContext) {
            let (panel, api, cx) = open_with_api(cx);

            add(&panel, "   ", cx);

            assert!(api.instructions_added.lock().unwrap().is_empty());
        }

        #[gpui::test]
        fn should_keep_the_text_when_the_instruction_cant_be_added(cx: &mut TestAppContext) {
            let (panel, api, cx) = open_with_api(cx);
            api.fail_next("refused");

            add(&panel, "keep me", cx);

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
