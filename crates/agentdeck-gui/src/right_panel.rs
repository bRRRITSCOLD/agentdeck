//! The right panel (spec §11), Penpot "Main window" `RightPanel`. The top bar's panel button
//! toggles it, through [`ToggleRightPanel`](crate::keymap::ToggleRightPanel), which the
//! window handles.
//!
//! First come the workspace's instructions, newest first, and a field that adds one for every
//! agent in the workspace (Enter sends it; the Instructions header's `+` focuses it). Each
//! instruction's `…` menu sends it to a live agent pane at once (spec §9), edits it in place
//! (Ctrl+Enter or Save saves, Escape or Cancel drops the edit) or deletes it. A long
//! instruction shows [`CLAMP`] lines, and Show more in its meta row shows the rest.
//!
//! Below a separator come the workspace's tasks and PRs, grouped by status, each with a status
//! picker. Done tasks hide behind "Show N done". The Tasks & PRs `+` asks the window for the
//! new task dialog on the workspace ([`NewTask`]). **Select**, before it, turns on Select mode
//! for closing PRs ([`ClosePrs`]), with the selection bar pinned to the panel's bottom.
//!
//! Like the Tasks view it shows the sidebar's [`Snapshot`], passed on by the window; an
//! instruction added here comes back through the daemon's `changed`.
//!
//! All of it is one GPUI [`list`] of `Item`s, which draws only the items in view: with 70 PRs,
//! drawing every row made each frame rebuild every row's status picker. The window draws it as
//! a cached view, so a frame drawn for a change elsewhere reuses it.

use std::collections::HashSet;
use std::sync::Arc;

use agentdeck_core::{InstructionId, PaneId, TaskId, TaskStatus, WorkspaceId};
use brrritscold_core_gpui::{
    ActiveTheme as _, Button, ButtonSize, ButtonVariant, DropdownMenu, DropdownMenuContent,
    DropdownMenuItem, DropdownMenuItemVariant, DropdownMenuLabel, DropdownMenuSeparator, Icon,
    IconName, Input, InputState, Separator, Textarea, TextareaState,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement, ListState, ParentElement as _, Render, SharedString,
    Styled as _, StyledText, Subscription, TextLayout, Window, div, list, px,
};
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::close_prs::{self, ClosePrs};
use crate::instruction_list::{self, InstructionRow, SendTarget};
use crate::keymap::{self, CancelEdit, Submit};
use crate::new_task::NewTask;
use crate::overview::Snapshot;
use crate::scrollbar;
use crate::task_list::{self, Group, Query, Row, Scope};
use crate::task_row;
use crate::task_sheet::{EditTask, OpenTask};
use crate::workspace::failed;

/// The panel's width. Raw in Penpot too: no sizing token is 340px.
pub const WIDTH: f32 = 340.;

/// How many lines of an instruction show until Show more.
pub const CLAMP: usize = 4;

/// The right panel.
pub struct RightPanel {
    api: Arc<dyn Api>,
    snapshot: Snapshot,
    workspace: Option<WorkspaceId>,
    show_done: bool,
    /// The statuses whose group is folded; Todo starts folded, as Penpot draws it.
    collapsed: HashSet<TaskStatus>,
    /// The instruction to add (Penpot `NewInstruction`).
    new_instruction: Entity<InputState>,
    /// Whether the instruction typed is being sent; another Enter waits for it.
    sending: bool,
    _new_instruction: Subscription,
    /// The instruction whose card is an editor, and the text being edited.
    editor: Option<(InstructionId, Entity<TextareaState>)>,
    /// Whether the edit is being saved; Save waits for it.
    saving: bool,
    _editor: Option<Subscription>,
    /// The instructions shown whole, from Show more.
    expanded: HashSet<InstructionId>,
    /// The instructions whose text runs past [`CLAMP`] lines, as last drawn.
    overflowing: HashSet<InstructionId>,
    /// The list's items, as last rendered.
    shown: Vec<Item>,
    /// The items of `shown` that take focus, with their focus, as the list was last told.
    focusable: Vec<(usize, FocusHandle)>,
    /// The list: where it is scrolled to, and its items' heights.
    list: ListState,
    /// Select mode, the selection and the confirm dialog.
    close: Entity<ClosePrs>,
    _close: Subscription,
    /// How many list items it has rendered, so a test can tell which items a frame drew.
    #[cfg(test)]
    pub(crate) items_rendered: std::cell::Cell<usize>,
}

impl EventEmitter<NewTask> for RightPanel {}
impl EventEmitter<OpenTask> for RightPanel {}
impl EventEmitter<EditTask> for RightPanel {}

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
        let close = cx.new(|_| ClosePrs::new(api.clone(), "panel"));
        let _close = cx.observe(&close, |_, _, cx| cx.notify());
        RightPanel {
            api,
            snapshot: Snapshot::default(),
            workspace: None,
            show_done: false,
            collapsed: HashSet::from([TaskStatus::Todo]),
            new_instruction,
            sending: false,
            _new_instruction,
            editor: None,
            saving: false,
            _editor: None,
            expanded: HashSet::new(),
            overflowing: HashSet::new(),
            shown: Vec::new(),
            focusable: Vec::new(),
            // Every item measured once, then each one that changes, so the scrollbar's thumb is
            // right from the first frame; a redraw renders only the items in view.
            list: task_row::list_state().measure_all(),
            close,
            _close,
            #[cfg(test)]
            items_rendered: std::cell::Cell::new(0),
        }
    }

    /// Select mode, the selection and the confirm dialog.
    #[cfg(test)]
    pub fn close_prs(&self) -> &Entity<ClosePrs> {
        &self.close
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

    /// Turns `instruction`'s card into an editor holding its text, focused: what its Edit
    /// item does. Another card being edited closes, unsaved.
    pub fn edit_instruction(
        &mut self,
        instruction: InstructionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(body) = (self.snapshot.instructions.iter())
            .find(|i| i.id == instruction)
            .map(|i| i.body.clone())
        else {
            return;
        };
        let editor = cx.new(|cx| {
            let mut editor = TextareaState::new(window, cx).auto_grow(2, 8);
            editor.set_value(body, window, cx);
            editor
        });
        // Save follows whether the text is blank, and the card grows with the text: the list
        // measures it again, in view or not.
        self._editor = Some(cx.subscribe(&editor, |this, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                let editing = (this.shown.iter()).position(|item| matches!(item, Item::Editor(_)));
                if let Some(ix) = editing {
                    this.list.remeasure_items(ix..ix + 1);
                }
                cx.notify();
            }
        }));
        // The menu hands focus back to its trigger as it closes, after this.
        let focus = editor.clone();
        window.defer(cx, move |window, cx| {
            focus.update(cx, |editor, cx| editor.focus(window, cx));
        });
        self.editor = Some((instruction, editor));
        cx.notify();
    }

    /// The instruction being edited, if any.
    pub fn editing(&self) -> Option<InstructionId> {
        self.editor.as_ref().map(|(instruction, _)| *instruction)
    }

    /// Whether Save can go ahead: the text isn't blank and no save is out.
    pub fn can_save(&self, cx: &App) -> bool {
        (self.editor.as_ref())
            .is_some_and(|(_, editor)| !self.saving && !editor.read(cx).value().trim().is_empty())
    }

    /// Saves the edited text, trimmed, then closes the editor; a failure keeps it open and
    /// toasts why. What Save and Ctrl+Enter do.
    pub fn save_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_save(cx) {
            return;
        }
        let Some((instruction, editor)) = self.editor.clone() else {
            return;
        };
        let body = editor.read(cx).value().trim().to_owned();
        self.saving = true;
        cx.notify();
        let saving = self.api.edit_instruction(instruction, body, cx);
        cx.spawn_in(window, async move |this, cx| {
            let saved = saving.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match saved {
                    Ok(()) => this.close_editor(instruction, cx),
                    Err(err) => failed("Couldn't save the instruction", &err, window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Closes the editor unsaved: what Cancel and Escape do.
    pub fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.editor = None;
        self._editor = None;
        cx.notify();
    }

    /// Closes the editor if it still edits `instruction`.
    fn close_editor(&mut self, instruction: InstructionId, cx: &mut Context<Self>) {
        if self.editing() == Some(instruction) {
            self.cancel_edit(cx);
        }
    }

    /// Deletes `instruction` at once, with no confirmation: what its Delete item does. It
    /// leaves the list through the daemon's `changed`; a failure toasts why.
    pub fn delete_instruction(
        &mut self,
        instruction: InstructionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let deleting = self.api.delete_instruction(instruction, cx);
        cx.spawn_in(window, async move |this, cx| {
            let deleted = deleting.await;
            let _ = this.update_in(cx, |this, window, cx| match deleted {
                Ok(()) => this.close_editor(instruction, cx),
                Err(err) => failed("Couldn't delete the instruction", &err, window, cx),
            });
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

    /// Lists `workspace`'s tasks and instructions from `snapshot`. An instruction being
    /// edited that the snapshot no longer holds closes its editor; one that left the list
    /// forgets whether it was shown whole.
    pub fn show(&mut self, workspace: WorkspaceId, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.workspace = Some(workspace);
        self.close
            .update(cx, |close, cx| close.saw(&snapshot.tasks, cx));
        self.snapshot = snapshot;
        let listed: HashSet<InstructionId> = self.instructions().iter().map(|i| i.id).collect();
        if self
            .editing()
            .is_some_and(|editing| !listed.contains(&editing))
        {
            self.cancel_edit(cx);
        }
        self.expanded.retain(|id| listed.contains(id));
        self.overflowing.retain(|id| listed.contains(id));
        cx.notify();
    }

    /// Whether `instruction` shows whole, from Show more.
    pub fn is_expanded(&self, instruction: InstructionId) -> bool {
        self.expanded.contains(&instruction)
    }

    /// Shows `instruction` whole, or clamps it again: what Show more and Show less do.
    pub fn toggle_instruction(&mut self, instruction: InstructionId, cx: &mut Context<Self>) {
        if !self.expanded.remove(&instruction) {
            self.expanded.insert(instruction);
        }
        cx.notify();
    }

    /// Records whether `instruction`'s text runs past [`CLAMP`] lines, as its last draw
    /// measured it; a change redraws, to show or hide its toggle.
    fn set_overflowing(
        &mut self,
        instruction: InstructionId,
        runs_past: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = if runs_past {
            self.overflowing.insert(instruction)
        } else {
            self.overflowing.remove(&instruction)
        };
        if changed {
            cx.notify();
        }
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

    /// The list's items, top to bottom, over `groups`, the groups shown.
    fn items(&self, groups: &[Group], cx: &App) -> Vec<Item> {
        let mut items = vec![Item::InstructionsHeader];
        items.extend(self.instructions().into_iter().map(|row| {
            if self.editing() == Some(row.id) {
                Item::Editor(row.id)
            } else {
                let clamp = Clamp {
                    expanded: self.is_expanded(row.id),
                    overflowing: self.overflowing.contains(&row.id),
                };
                Item::Instruction { row, clamp }
            }
        }));
        items.extend([Item::NewInstruction, Item::Separator, Item::TasksHeader]);
        let close = self.close.read(cx);
        let selecting = close.active();
        for group in groups {
            let collapsed = self.is_collapsed(group.status);
            items.push(Item::Group {
                status: group.status,
                count: group.rows.len(),
                collapsed,
                selectable: close_prs::selectable(&group.rows),
                selecting,
            });
            if !collapsed {
                items.extend(group.rows.iter().map(|row| Item::Task {
                    row: row.clone(),
                    selecting,
                    error: close.error(row.id).cloned(),
                }));
            }
        }
        let done = self.done();
        if done > 0 {
            items.push(Item::ShowDone {
                done,
                shown: self.show_done,
            });
        }
        items
    }

    /// Makes the list hold `items`, keeping its place. Only the items that changed are
    /// measured again. The instruction field and an open editor go to the list with their
    /// focus, so it keeps drawing them while focused, scrolled out of view or not.
    fn relist(&mut self, items: Vec<Item>, cx: &App) {
        let (old, new) = (self.shown.len(), items.len());
        if old == 0 {
            // An empty list keeps its top at item 0, which a splice at 0 would push past the
            // items it adds.
            self.list.reset(new);
        } else {
            if new > old {
                self.list.splice(old..old, new - old);
            } else if new < old {
                self.list.splice(new..old, 0);
            }
            let changed = |ix: &usize| self.shown[*ix] != items[*ix];
            let first = (0..old.min(new)).find(changed);
            let last = (0..old.min(new)).rfind(changed);
            if let (Some(first), Some(last)) = (first, last) {
                self.list.remeasure_items(first..last + 1);
            }
        }
        let focusable = self.focusable(&items, cx);
        if focusable != self.focusable {
            for (ix, _) in &self.focusable {
                if *ix < new {
                    self.list.splice_focusable(*ix..*ix + 1, [None]);
                }
            }
            for (ix, handle) in &focusable {
                self.list
                    .splice_focusable(*ix..*ix + 1, [Some(handle.clone())]);
            }
            self.focusable = focusable;
        }
        self.shown = items;
    }

    /// The items in `items` that take focus, the field and an open editor, with their focus.
    fn focusable(&self, items: &[Item], cx: &App) -> Vec<(usize, FocusHandle)> {
        (items.iter().enumerate())
            .filter_map(|(ix, item)| {
                let handle = match item {
                    Item::NewInstruction => self.new_instruction.focus_handle(cx),
                    Item::Editor(_) => self.editor.as_ref()?.1.focus_handle(cx),
                    _ => return None,
                };
                Some((ix, handle))
            })
            .collect()
    }

    /// The list's `ix`th item, `space.1` from the next: a list has no gap of its own.
    fn item(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(test)]
        self.items_rendered.set(self.items_rendered.get() + 1);
        let Some(item) = self.shown.get(ix).cloned() else {
            return div().into_any_element();
        };
        let s = cx.theme().semantic();
        let element = match item {
            // Instructions first: the header with its `+`, each instruction, then Penpot
            // `NewInstruction`.
            Item::InstructionsHeader => section_header("Instructions", s)
                .child(
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
                )
                .into_any_element(),
            Item::Instruction { row, clamp } => {
                let targets = instruction_list::send_targets(&self.snapshot, row.id);
                instruction_item(&row, clamp, targets, cx)
            }
            Item::Editor(instruction) => match &self.editor {
                Some((_, editor)) => instruction_editor(instruction, editor, self.can_save(cx), cx),
                None => div().into_any_element(),
            },
            Item::NewInstruction => div()
                .flex_none()
                .debug_selector(|| "new instruction".into())
                .child(Input::new(&self.new_instruction))
                .into_any_element(),
            // Then Separator, the Tasks & PRs header with its `+`, and the groups.
            Item::Separator => Separator::new().my(space::S1).into_any_element(),
            Item::TasksHeader => section_header("Tasks & PRs", s)
                .child(close_prs::select_toggle(&self.close, cx))
                .child(
                    div().debug_selector(|| "add task".into()).child(
                        Button::new("add-task")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::IconXs)
                            .icon(IconName::Plus)
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.emit(NewTask(this.workspace));
                            })),
                    ),
                )
                .into_any_element(),
            Item::Group {
                status,
                count,
                collapsed,
                selectable,
                ..
            } => {
                let select = close_prs::select_all(&self.close, status.as_str(), selectable, cx);
                task_row::group_header(
                    status.to_string(),
                    task_row::status_label(status),
                    count,
                    collapsed,
                    select,
                    cx.listener(move |this, _, _, cx| this.toggle(status, cx)),
                    cx,
                )
            }
            Item::Task { row, .. } => {
                let task = row.id;
                let picker = task_row::status_picker(
                    SharedString::from(format!("panel-status-{task}")),
                    row.status,
                    cx.listener(move |this, status: &TaskStatus, window, cx| {
                        this.set_status(task, *status, window, cx);
                    }),
                );
                let open = Box::new(cx.listener(move |_, _, _, cx| cx.emit(OpenTask(task))));
                let view = cx.weak_entity();
                let edit = move |_: &mut Window, cx: &mut gpui::App| {
                    let _ = view.update(cx, |_, cx| cx.emit(EditTask(task)));
                };
                let send = crate::send_task::targets(&self.snapshot, task);
                let parts = close_prs::row_parts(&self.close, &row, send, edit, cx);
                task_row::panel(&row, picker, parts, open, cx)
            }
            // Penpot `ShowDone`: a link button at xs, at the start of its row, after the
            // groups.
            Item::ShowDone { done, shown } => {
                let label = if shown {
                    "Hide done".to_owned()
                } else {
                    format!("Show {done} done")
                };
                div()
                    .flex()
                    .flex_none()
                    .px(space::S2)
                    .child(
                        div().debug_selector(|| "show done".into()).child(
                            Button::new("panel-show-done")
                                .variant(ButtonVariant::Link)
                                .size(ButtonSize::Xs)
                                .label(label)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_show_done(!shown, cx);
                                })),
                        ),
                    )
                    .into_any_element()
            }
        };
        // As wide as the list, as the rows of a column are: a list lays each item out alone.
        div()
            .w_full()
            .flex()
            .flex_col()
            .when(ix + 1 < self.shown.len(), |item| item.pb(space::S1))
            .child(element)
            .into_any_element()
    }
}

/// One item of the panel's list (Penpot `RightPanel`), holding what its height depends on:
/// the list measures an item again when it changes.
#[derive(Debug, Clone, PartialEq)]
enum Item {
    /// Penpot "Instructions header", with its `+`.
    InstructionsHeader,
    /// An instruction's card.
    Instruction {
        row: InstructionRow,
        clamp: Clamp,
    },
    /// The card of the instruction being edited.
    Editor(InstructionId),
    /// Penpot `NewInstruction`.
    NewInstruction,
    Separator,
    /// Penpot "Tasks & PRs header", with Select and its `+`.
    TasksHeader,
    /// A status group's header.
    Group {
        status: TaskStatus,
        count: usize,
        collapsed: bool,
        /// Its rows with an open PR, which its checkbox picks.
        selectable: Vec<TaskId>,
        /// Select mode is on.
        selecting: bool,
    },
    /// A task's row.
    Task {
        row: Row,
        /// Select mode is on.
        selecting: bool,
        /// Why closing its PR failed, under its title.
        error: Option<SharedString>,
    },
    /// Penpot `ShowDone`: Show N done, or Hide done while `shown`.
    ShowDone {
        done: usize,
        shown: bool,
    },
}

impl Render for RightPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = cx.theme().semantic();
        let groups = self.groups();
        let rows: Vec<_> = groups.iter().flat_map(|g| g.rows.clone()).collect();
        let shown = close_prs::selectable(&rows);
        self.close.update(cx, |close, cx| close.retain(&shown, cx));
        let bar =
            close_prs::selection_bar(&self.close, close_prs::picked(&self.close, &rows, cx), cx);
        let items = self.items(&groups, cx);
        if items != self.shown {
            self.relist(items, cx);
        }
        // Only the items in view are drawn. The list reaches the panel's right edge, over its
        // `space.2` padding, and pads its content back in by the scrollbar's gutter (Penpot's
        // `space.3` on the right): the scrollbar sits in it, clear of the rows and their
        // buttons.
        let list = div()
            .debug_selector(|| "panel list".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .mr(-space::S2)
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, ix, _, cx| this.item(ix, cx)),
                )
                .flex_1()
                .min_h_0()
                .pr(scrollbar::GUTTER),
            );
        // Penpot `RightPanel`: `card`, padded `space.3` down and `space.2` across, rows
        // `space.1` apart; in Select mode the selection bar sits on its bottom edge.
        let selecting = bar.is_some();
        div()
            .flex_none()
            .w(px(WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .pt(space::S3)
            .when(!selecting, |panel| panel.pb(space::S3))
            .px(space::S2)
            .bg(s.card)
            .child(list)
            .child(scrollbar::vertical(&self.list, cx))
            .children(bar)
            // The confirm dialog's host: out of the flow.
            .child(div().absolute().child(self.close.clone()))
    }
}

/// Penpot's "Tasks & PRs header" and "Instructions header": sm medium, 32px, padded
/// `space.1` / `space.2`, its children after a spacer.
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

/// The card's frame: a `border` box with `radius-md`, padded `space.2` / `space.3`.
fn card(s: &brrritscold_core_gpui_tokens::Semantic) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .flex_none()
        .gap(space::S1_5)
        .px(space::S3)
        .py(space::S2)
        .border_1()
        .border_color(s.border)
        .rounded(s.radius_md)
}

/// Where an instruction's card stands on clamping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Clamp {
    /// Shown whole, from Show more.
    expanded: bool,
    /// Its text runs past [`CLAMP`] lines, as last drawn.
    overflowing: bool,
}

/// Penpot `agentdeck/InstructionItem`: the text over a meta line (its icon, when it was
/// added, Show more or Show less while the text runs past [`CLAMP`] lines, the
/// instruction's menu).
fn instruction_item(
    instruction: &InstructionRow,
    clamp: Clamp,
    targets: Vec<SendTarget>,
    cx: &Context<RightPanel>,
) -> AnyElement {
    let s = cx.theme().semantic();
    let id = instruction.id;
    card(s)
        .debug_selector(move || format!("instruction {id}"))
        .child(instruction_text(instruction, clamp, cx))
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
                .when(clamp.overflowing, |meta| {
                    meta.child(clamp_toggle(id, clamp, cx))
                })
                .child(instruction_menu(id, targets, cx)),
        )
        .into_any_element()
}

/// An instruction's text, sm: [`CLAMP`] lines, the last ending in "…", unless shown whole.
/// Each draw measures whether the whole text runs past [`CLAMP`] lines at the card's width,
/// which only layout knows, and tells the panel when that changed.
fn instruction_text(
    instruction: &InstructionRow,
    clamp: Clamp,
    cx: &Context<RightPanel>,
) -> AnyElement {
    let s = cx.theme().semantic();
    let id = instruction.id;
    let body = instruction.body.clone();
    let text = StyledText::new(body.clone());
    let layout = text.layout().clone();
    let panel = cx.entity().downgrade();
    div()
        .debug_selector(move || format!("instruction text {id}"))
        .text_size(font_size::SM)
        .text_color(s.foreground)
        .when(!clamp.expanded, |text| {
            text.line_clamp(CLAMP).text_ellipsis()
        })
        .child(text)
        .on_children_prepainted(move |_, window, cx| {
            let runs_past = runs_past_clamp(&layout, &body, !clamp.expanded);
            if runs_past != clamp.overflowing {
                // The panel can't redraw from inside this draw; it does once it's over.
                let panel = panel.clone();
                window.defer(cx, move |_, cx| {
                    let _ = panel.update(cx, |panel, cx| {
                        panel.set_overflowing(id, runs_past, cx);
                    });
                });
            }
        })
        .into_any_element()
}

/// Whether `body`, laid out in `layout`, runs past [`CLAMP`] lines. A `clamped` layout
/// holds at most that many, and holds other text than `body` (cut, with "…") only when it
/// does; a whole one is counted, wrapped lines and line breaks alike.
fn runs_past_clamp(layout: &TextLayout, body: &str, clamped: bool) -> bool {
    if clamped {
        layout.text() != body
    } else {
        let lines: usize = (layout.line_layouts().iter())
            .map(|line| line.wrap_boundaries.len() + 1)
            .sum();
        lines > CLAMP
    }
}

/// Show more, or Show less once shown whole: a link xs (Penpot `InstructionItem` → `More`).
fn clamp_toggle(instruction: InstructionId, clamp: Clamp, cx: &Context<RightPanel>) -> AnyElement {
    let label = if clamp.expanded {
        "Show less"
    } else {
        "Show more"
    };
    div()
        .debug_selector(move || format!("instruction more {instruction}"))
        .flex_none()
        .child(
            Button::new(SharedString::from(format!(
                "instruction-more-{instruction}"
            )))
            .variant(ButtonVariant::Link)
            .size(ButtonSize::Xs)
            .label(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_instruction(instruction, cx);
            })),
        )
        .into_any_element()
}

/// The card's `…` menu: a ghost icon-xs `Ellipsis` opening a `DropdownMenu`. Penpot "Send to
/// agent" first: the label, then an item per live agent pane `targets` names; with none, one
/// disabled "No agent is running here". Then Edit, and Delete in `destructive` after a
/// separator.
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
    let edit = panel.clone();
    let content = content
        .separator(DropdownMenuSeparator::new())
        .item(
            DropdownMenuItem::new("Edit").on_click(move |_, window, cx| {
                let _ = edit.update(cx, |panel, cx| {
                    panel.edit_instruction(instruction, window, cx)
                });
            }),
        )
        .separator(DropdownMenuSeparator::new())
        .item(
            DropdownMenuItem::new("Delete")
                .variant(DropdownMenuItemVariant::Destructive)
                .on_click(move |_, window, cx| {
                    let _ = panel.update(cx, |panel, cx| {
                        panel.delete_instruction(instruction, window, cx)
                    });
                }),
        );
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

/// An instruction's card while it is edited: its text in a Textarea over Cancel and Save,
/// sm, at the end of their row. Save is disabled while the text is blank.
fn instruction_editor(
    instruction: InstructionId,
    editor: &Entity<TextareaState>,
    can_save: bool,
    cx: &Context<RightPanel>,
) -> AnyElement {
    let s = cx.theme().semantic();
    card(s)
        .debug_selector(move || format!("instruction editor {instruction}"))
        .gap(space::S2)
        .key_context(keymap::INSTRUCTION_EDITOR)
        .on_action(cx.listener(|this, _: &Submit, window, cx| this.save_edit(window, cx)))
        .on_action(cx.listener(|this, _: &CancelEdit, _, cx| this.cancel_edit(cx)))
        .child(Textarea::new(editor))
        .child(
            div()
                .flex()
                .justify_end()
                .gap(space::S2)
                .child(
                    div().debug_selector(|| "cancel edit".into()).child(
                        Button::new("instruction-edit-cancel")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm)
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_edit(cx))),
                    ),
                )
                .child(
                    div().debug_selector(|| "save instruction".into()).child(
                        Button::new("instruction-edit-save")
                            .size(ButtonSize::Sm)
                            .label("Save")
                            .disabled(!can_save)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save_edit(window, cx);
                            })),
                    ),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use agentdeck_client::types::Task;
    use agentdeck_core::{AgentState, TaskKind};
    use brrritscold_core_gpui::{Root, Theme, ThemePreference};
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::api::fake::FakeApi;
    use crate::new_task::NewTask;
    use crate::overview::PaneState;
    use crate::overview::fixtures::{id, pane, task, workspace};

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
        cx.update(crate::keymap::init);
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

        /// Draws, then lets every task run.
        fn settle(cx: &mut VisualTestContext) {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        /// Opens instruction 30's `…` menu and picks its `n`th enabled item by keyboard.
        fn pick(n: usize, cx: &mut VisualTestContext) {
            settle(cx);
            let selector = format!("instruction menu {}", id::<InstructionId>(30));
            let trigger = cx.debug_bounds(selector.leak()).expect("the menu shows");
            cx.simulate_click(trigger.center(), Modifiers::default());
            settle(cx);
            cx.simulate_keystrokes(&format!("{}enter", "down ".repeat(n)));
            settle(cx);
        }

        /// The text in the open editor.
        fn edited(panel: &Entity<RightPanel>, cx: &mut VisualTestContext) -> String {
            panel.read_with(cx, |p, cx| {
                let (_, editor) = p.editor.as_ref().expect("an editor is open");
                editor.read(cx).value().to_string()
            })
        }

        /// Replaces the open editor's text with `text`.
        fn retype(panel: &Entity<RightPanel>, text: &str, cx: &mut VisualTestContext) {
            cx.update(|window, cx| {
                let (_, editor) = panel.read(cx).editor.clone().expect("an editor is open");
                editor.update(cx, |editor, cx| {
                    editor.set_value(text.to_owned(), window, cx)
                });
            });
            settle(cx);
        }

        fn editing(
            panel: &Entity<RightPanel>,
            cx: &mut VisualTestContext,
        ) -> Option<InstructionId> {
            panel.read_with(cx, |p, _| p.editing())
        }

        mod long {
            use agentdeck_client::types::Instruction;

            use super::*;

            /// About a dozen lines at the card's width.
            const LONG: &str = "Before you push, run cargo ci and fix whatever it reports, not \
                only the tests you touched. Keep each commit to one change and write its message \
                in the imperative. When a test fails twice in a row for the same reason, stop and \
                comment on the task with what you tried. Work in your own worktree, never in the \
                main checkout, and delete it once the PR merges.";

            /// The one-line instruction every case lists too, to measure a line by.
            const RULER: u32 = 99;

            fn with_body(n: u32, body: &str) -> Instruction {
                Instruction {
                    body: body.into(),
                    ..instruction(n, id(1))
                }
            }

            /// Shows workspace 1 with `instructions` and the ruler, then draws until the
            /// measures settle.
            fn show(
                panel: &Entity<RightPanel>,
                mut instructions: Vec<Instruction>,
                cx: &mut VisualTestContext,
            ) {
                instructions.push(with_body(RULER, "short"));
                panel.update(cx, |p, cx| {
                    let mut snapshot = snapshot();
                    snapshot.instructions = instructions;
                    p.show(id(1), snapshot, cx);
                });
                settle(cx);
            }

            fn text_height(n: u32, cx: &mut VisualTestContext) -> gpui::Pixels {
                let selector = format!("instruction text {}", id::<InstructionId>(n));
                cx.debug_bounds(selector.leak())
                    .expect("its text")
                    .size
                    .height
            }

            /// One line's height: the ruler's text.
            fn line(cx: &mut VisualTestContext) -> gpui::Pixels {
                text_height(RULER, cx)
            }

            fn toggle(n: u32, cx: &mut VisualTestContext) -> Option<gpui::Bounds<gpui::Pixels>> {
                let selector = format!("instruction more {}", id::<InstructionId>(n));
                cx.debug_bounds(selector.leak())
            }

            fn click_toggle(n: u32, cx: &mut VisualTestContext) {
                let button = toggle(n, cx).expect("Show more or Show less");
                cx.simulate_click(button.center(), Modifiers::default());
                settle(cx);
            }

            fn expanded(panel: &Entity<RightPanel>, n: u32, cx: &mut VisualTestContext) -> bool {
                panel.read_with(cx, |p, _| p.is_expanded(id(n)))
            }

            #[gpui::test]
            fn should_clamp_a_long_instruction_to_four_lines_with_show_more(
                cx: &mut TestAppContext,
            ) {
                let (panel, _, cx) = open_with_api(cx);

                show(&panel, vec![with_body(30, LONG)], cx);

                assert_eq!(text_height(30, cx), line(cx) * 4.);
                assert!(toggle(30, cx).is_some());
                assert!(!expanded(&panel, 30, cx));
                assert!(toggle(RULER, cx).is_none());
            }

            #[gpui::test]
            fn should_offer_no_toggle_for_text_that_fits(cx: &mut TestAppContext) {
                let (panel, _, cx) = open_with_api(cx);

                show(
                    &panel,
                    vec![with_body(30, "Push only after the play test passes.")],
                    cx,
                );

                assert!(text_height(30, cx) <= line(cx) * 2.);
                assert!(toggle(30, cx).is_none());
            }

            #[gpui::test]
            fn should_count_line_breaks_as_lines(cx: &mut TestAppContext) {
                let (panel, _, cx) = open_with_api(cx);

                show(
                    &panel,
                    vec![
                        with_body(30, "one\ntwo\nthree\nfour"),
                        with_body(31, "1\n2\n3\n4\n5"),
                    ],
                    cx,
                );

                assert_eq!(text_height(30, cx), line(cx) * 4.);
                assert!(toggle(30, cx).is_none());
                assert_eq!(text_height(31, cx), line(cx) * 4.);
                assert!(toggle(31, cx).is_some());
            }

            #[gpui::test]
            fn should_show_the_whole_text_from_show_more_and_clamp_it_from_show_less(
                cx: &mut TestAppContext,
            ) {
                let (panel, _, cx) = open_with_api(cx);
                show(&panel, vec![with_body(30, LONG)], cx);

                click_toggle(30, cx);

                assert!(expanded(&panel, 30, cx));
                assert!(text_height(30, cx) > line(cx) * 4.);
                assert!(toggle(30, cx).is_some(), "Show less");

                click_toggle(30, cx);

                assert!(!expanded(&panel, 30, cx));
                assert_eq!(text_height(30, cx), line(cx) * 4.);
            }

            #[gpui::test]
            fn should_keep_each_instructions_choice_across_rereads(cx: &mut TestAppContext) {
                let (panel, _, cx) = open_with_api(cx);
                show(&panel, vec![with_body(30, LONG), with_body(31, LONG)], cx);
                click_toggle(30, cx);

                show(&panel, vec![with_body(30, LONG), with_body(31, LONG)], cx);

                assert!(expanded(&panel, 30, cx));
                assert!(text_height(30, cx) > line(cx) * 4.);
                assert!(!expanded(&panel, 31, cx));
                assert!(toggle(31, cx).is_some());
            }

            #[gpui::test]
            fn should_forget_the_choice_once_the_instruction_leaves(cx: &mut TestAppContext) {
                let (panel, _, cx) = open_with_api(cx);
                show(&panel, vec![with_body(30, LONG)], cx);
                click_toggle(30, cx);

                show(&panel, vec![], cx);
                show(&panel, vec![with_body(30, LONG)], cx);

                assert!(!expanded(&panel, 30, cx));
                assert_eq!(text_height(30, cx), line(cx) * 4.);
            }

            #[gpui::test]
            fn should_drop_the_toggle_once_an_edit_makes_the_text_fit(cx: &mut TestAppContext) {
                let (panel, _, cx) = open_with_api(cx);
                show(&panel, vec![with_body(30, LONG)], cx);
                click_toggle(30, cx);

                show(&panel, vec![with_body(30, "Now it's short.")], cx);

                assert!(toggle(30, cx).is_none());
            }
        }

        mod edit {
            use super::*;

            #[gpui::test]
            fn should_open_an_editor_holding_its_text_from_its_menu(cx: &mut TestAppContext) {
                let (panel, _, cx) = with_an_instruction(cx);

                pick(1, cx);

                assert_eq!(editing(&panel, cx), Some(id(30)));
                assert_eq!(edited(&panel, cx), "instruction");
            }

            #[gpui::test]
            fn should_save_the_trimmed_text_on_ctrl_enter(cx: &mut TestAppContext) {
                let (panel, api, cx) = with_an_instruction(cx);
                pick(1, cx);
                retype(&panel, "  Push after 20 green runs\nand not before ", cx);

                cx.simulate_keystrokes("ctrl-enter");
                settle(cx);

                assert_eq!(
                    *api.edited_instructions.lock().unwrap(),
                    [(
                        id(30),
                        "Push after 20 green runs\nand not before".to_owned()
                    )]
                );
                assert_eq!(editing(&panel, cx), None);
            }

            #[gpui::test]
            fn should_insert_a_newline_on_enter(cx: &mut TestAppContext) {
                let (panel, api, cx) = with_an_instruction(cx);
                pick(1, cx);

                cx.simulate_keystrokes("enter");
                settle(cx);

                assert!(api.edited_instructions.lock().unwrap().is_empty());
                assert!(edited(&panel, cx).contains('\n'));
            }

            #[gpui::test]
            fn should_save_from_the_save_button(cx: &mut TestAppContext) {
                let (panel, api, cx) = with_an_instruction(cx);
                pick(1, cx);
                retype(&panel, "new", cx);

                let save = cx.debug_bounds("save instruction").expect("Save shows");
                cx.simulate_click(save.center(), Modifiers::default());
                settle(cx);

                assert_eq!(
                    *api.edited_instructions.lock().unwrap(),
                    [(id(30), "new".to_owned())]
                );
            }

            #[gpui::test]
            fn should_keep_save_disabled_while_the_text_is_blank(cx: &mut TestAppContext) {
                let (panel, api, cx) = with_an_instruction(cx);
                pick(1, cx);
                assert!(cx.update(|_, cx| panel.read(cx).can_save(cx)));

                retype(&panel, "  \n ", cx);
                assert!(!cx.update(|_, cx| panel.read(cx).can_save(cx)));
                cx.simulate_keystrokes("ctrl-enter");
                settle(cx);
                let save = cx.debug_bounds("save instruction").expect("Save shows");
                cx.simulate_click(save.center(), Modifiers::default());
                settle(cx);

                assert!(api.edited_instructions.lock().unwrap().is_empty());
                assert_eq!(editing(&panel, cx), Some(id(30)));
            }

            #[gpui::test]
            fn should_cancel_on_escape(cx: &mut TestAppContext) {
                let (panel, api, cx) = with_an_instruction(cx);
                pick(1, cx);
                retype(&panel, "never saved", cx);

                cx.simulate_keystrokes("escape");
                settle(cx);

                assert_eq!(editing(&panel, cx), None);
                assert!(api.edited_instructions.lock().unwrap().is_empty());
            }

            #[gpui::test]
            fn should_cancel_from_the_cancel_button(cx: &mut TestAppContext) {
                let (panel, api, cx) = with_an_instruction(cx);
                pick(1, cx);

                let cancel = cx.debug_bounds("cancel edit").expect("Cancel shows");
                cx.simulate_click(cancel.center(), Modifiers::default());
                settle(cx);

                assert_eq!(editing(&panel, cx), None);
                assert!(api.edited_instructions.lock().unwrap().is_empty());
            }

            #[gpui::test]
            fn should_keep_editing_and_toast_when_the_edit_cant_be_saved(cx: &mut TestAppContext) {
                let (panel, api, cx) = with_an_instruction(cx);
                pick(1, cx);
                retype(&panel, "keep me", cx);
                api.fail_next("refused");

                cx.update(|window, cx| panel.update(cx, |p, cx| p.save_edit(window, cx)));
                settle(cx);

                assert_eq!(editing(&panel, cx), Some(id(30)));
                assert_eq!(edited(&panel, cx), "keep me");
                let shown = cx.update(|_, cx| crate::notice::shown(cx));
                assert_eq!(shown.last().unwrap().title, "Couldn't save the instruction");
            }

            #[gpui::test]
            fn should_stop_editing_once_the_instruction_is_gone(cx: &mut TestAppContext) {
                let (panel, _, cx) = with_an_instruction(cx);
                pick(1, cx);

                panel.update(cx, |p, cx| p.show(id(1), snapshot(), cx));

                assert_eq!(editing(&panel, cx), None);
            }
        }

        mod delete {
            use super::*;

            #[gpui::test]
            fn should_delete_the_instruction_from_its_menu(cx: &mut TestAppContext) {
                let (_, api, cx) = with_an_instruction(cx);

                pick(2, cx);

                assert_eq!(*api.deleted_instructions.lock().unwrap(), [id(30)]);
            }

            #[gpui::test]
            fn should_follow_the_agents_to_send_to_with_edit_then_delete(cx: &mut TestAppContext) {
                let (panel, api, cx) = with_an_instruction(cx);
                panel.update(cx, |p, cx| {
                    let mut snapshot = p.snapshot.clone();
                    snapshot.panes = vec![crate::overview::PaneState {
                        agent: Some(agentdeck_core::Agent::Claude),
                        ..crate::overview::fixtures::pane(
                            40,
                            id(1),
                            agentdeck_core::AgentState::Idle,
                        )
                    }];
                    p.show(id(1), snapshot, cx);
                });

                pick(3, cx);

                assert_eq!(*api.deleted_instructions.lock().unwrap(), [id(30)]);
                assert!(api.sent_instructions.lock().unwrap().is_empty());
            }

            #[gpui::test]
            fn should_toast_when_the_instruction_cant_be_deleted(cx: &mut TestAppContext) {
                let (panel, api, cx) = with_an_instruction(cx);
                api.fail_next("refused");

                cx.update(|window, cx| {
                    panel.update(cx, |p, cx| p.delete_instruction(id(30), window, cx));
                });
                settle(cx);

                let shown = cx.update(|_, cx| crate::notice::shown(cx));
                assert_eq!(
                    shown.last().unwrap().title,
                    "Couldn't delete the instruction"
                );
            }
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

    #[gpui::test]
    fn should_list_instructions_before_tasks(cx: &mut TestAppContext) {
        let (panel, cx) = open(cx);
        panel.update(cx, |p, cx| {
            let mut snapshot = snapshot();
            snapshot.instructions = vec![crate::overview::fixtures::instruction(30, id(1))];
            p.show(id(1), snapshot, cx);
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let card: &str = format!("instruction {}", id::<agentdeck_core::InstructionId>(30)).leak();
        let title: &str = format!("task title {}", id::<TaskId>(1)).leak();
        let order = [
            "add instruction",
            card,
            "new instruction",
            "add task",
            title,
        ];
        let tops: Vec<_> = (order.into_iter())
            .map(|selector| {
                (cx.debug_bounds(selector))
                    .unwrap_or_else(|| panic!("{selector} shows"))
                    .top()
            })
            .collect();
        assert!(tops.is_sorted(), "top to bottom: {tops:?}");
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
    fn should_ask_to_edit_a_task_from_its_rows_menu(cx: &mut TestAppContext) {
        let (panel, cx) = open(cx);
        let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = asked.clone();
        let _asks = cx.update(|_, cx| {
            cx.subscribe(&panel, move |_, event: &EditTask, _| {
                seen.borrow_mut().push(*event)
            })
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let menu = format!("panel menu {}", id::<TaskId>(1));
        let bounds = cx.debug_bounds(menu.leak()).expect("the row's menu shows");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();

        assert_eq!(*asked.borrow(), [EditTask(id(1))]);
    }

    /// Opens task 1's row menu, then its Send to agent submenu, and picks its first item.
    fn send_task_1(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let menu = format!("panel menu {}", id::<TaskId>(1));
        let bounds = cx.debug_bounds(menu.leak()).expect("the row's menu shows");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("down down enter");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
    }

    #[gpui::test]
    fn should_send_a_task_to_the_agent_picked_from_its_rows_menu(cx: &mut TestAppContext) {
        let (panel, api, cx) = open_with_api(cx);
        let mut with_agent = snapshot();
        with_agent.panes.push(PaneState {
            agent: Some(agentdeck_core::Agent::Claude),
            ..pane(30, id(1), AgentState::Idle)
        });
        panel.update(cx, |panel, cx| panel.show(id(1), with_agent, cx));

        send_task_1(cx);

        assert_eq!(*api.linked.lock().unwrap(), [(id(30), Some(id(1)))]);
    }

    #[gpui::test]
    fn should_send_nothing_while_no_agent_runs_in_the_workspace(cx: &mut TestAppContext) {
        let (_panel, api, cx) = open_with_api(cx);

        send_task_1(cx);

        assert_eq!(*api.linked.lock().unwrap(), []);
    }

    #[gpui::test]
    fn should_copy_a_tasks_id_from_its_rows_menu(cx: &mut TestAppContext) {
        let (_panel, cx) = open(cx);
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let menu = format!("panel menu {}", id::<TaskId>(1));
        let bounds = cx.debug_bounds(menu.leak()).expect("the row's menu shows");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("down down down enter");
        cx.run_until_parked();

        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied, Some(id::<TaskId>(1).to_string()));
        let shown = cx.update(|_, cx| crate::notice::shown(cx));
        assert_eq!(
            shown.last().map(|toast| toast.title.clone()),
            Some("Copied task id".into())
        );
    }

    #[gpui::test]
    fn should_ask_for_the_task_sheet_when_a_title_is_clicked(cx: &mut TestAppContext) {
        let (panel, cx) = open(cx);
        let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = asked.clone();
        let _asks = cx.update(|_, cx| {
            cx.subscribe(&panel, move |_, event: &OpenTask, _| {
                seen.borrow_mut().push(*event)
            })
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let title = format!("task title {}", id::<TaskId>(1));
        let bounds = cx.debug_bounds(title.leak()).expect("the title shows");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();

        assert_eq!(*asked.borrow(), [OpenTask(id(1))]);
    }

    #[gpui::test]
    fn should_mark_a_row_whose_agent_comments_are_unseen_before_its_agent(cx: &mut TestAppContext) {
        let (panel, cx) = open(cx);
        panel.update(cx, |p, cx| {
            let mut snapshot = snapshot();
            snapshot.tasks[0].unseen_agent_comments = 1;
            snapshot.panes.push(PaneState {
                task: Some(id(1)),
                ..pane(10, snapshot.workspaces[0].id, AgentState::Idle)
            });
            p.show(id(1), snapshot, cx);
            p.toggle(TaskStatus::Todo, cx);
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let mark =
            (cx.debug_bounds(format!("unseen {}", id::<TaskId>(1)).leak())).expect("task 1's mark");
        let agent = (cx.debug_bounds(format!("panel agent {}", id::<TaskId>(1)).leak()))
            .expect("task 1's agent mark");
        assert_eq!(mark.size, gpui::size(px(14.), px(14.)));
        assert!(mark.right() <= agent.left(), "{mark:?} before {agent:?}");
        assert!(
            agent.left() - mark.right() <= space::S2,
            "{mark:?} next to {agent:?}"
        );
        assert!(
            cx.debug_bounds(format!("task title {}", id::<TaskId>(2)).leak())
                .is_some()
        );
        assert!(
            cx.debug_bounds(format!("unseen {}", id::<TaskId>(2)).leak())
                .is_none()
        );
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

    mod scrollbar {
        use super::*;
        use crate::scrollbar::testing::thumb;

        fn draw(cx: &mut VisualTestContext) {
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        #[gpui::test]
        fn should_show_a_scrollbar_when_the_list_overflows(cx: &mut TestAppContext) {
            let (panel, cx) = open(cx);
            let ws = workspace(1, "a");
            let crowded = Snapshot {
                tasks: (1..=100)
                    .map(|n| with_status(n, TaskStatus::Doing, ws.id))
                    .collect(),
                workspaces: vec![ws],
                ..Snapshot::default()
            };
            panel.update(cx, |view, cx| view.show(id(1), crowded, cx));
            draw(cx);

            let list = cx.debug_bounds("panel list").expect("the list");
            assert!(thumb(list, cx).is_some());
        }

        #[gpui::test]
        fn should_hide_the_scrollbar_when_everything_fits(cx: &mut TestAppContext) {
            let (_panel, cx) = open(cx);
            draw(cx);

            let list = cx.debug_bounds("panel list").expect("the list");
            assert_eq!(thumb(list, cx), None);
        }

        #[gpui::test]
        fn should_keep_the_thumb_clear_of_the_rows_and_their_buttons(cx: &mut TestAppContext) {
            let (panel, cx) = open(cx);
            let ws = workspace(1, "a");
            let crowded = Snapshot {
                tasks: (1..=100)
                    .map(|n| with_status(n, TaskStatus::Doing, ws.id))
                    .collect(),
                instructions: vec![crate::overview::fixtures::instruction(200, ws.id)],
                workspaces: vec![ws],
                ..Snapshot::default()
            };
            panel.update(cx, |view, cx| view.show(id(1), crowded, cx));
            draw(cx);
            let list = cx.debug_bounds("panel list").expect("the list");
            let thumb = thumb(list, cx).expect("a thumb");

            // The rows in view: the list draws no others.
            let mut parts: Vec<String> = (1..=100)
                .map(|n| format!("panel task {}", id::<TaskId>(n)))
                .filter(|row| cx.debug_bounds(row.clone().leak()).is_some())
                .collect();
            assert!(parts.len() >= 3, "{} rows in view", parts.len());
            parts.extend(
                [
                    "add instruction",
                    "new instruction",
                    "add task",
                    "group doing",
                ]
                .map(String::from),
            );
            parts.push(format!(
                "instruction {}",
                id::<agentdeck_core::InstructionId>(200)
            ));
            for part in parts {
                let bounds = cx.debug_bounds(part.clone().leak()).expect(&part);
                assert!(
                    bounds.right() < thumb.left(),
                    "{part} {bounds:?} reaches {thumb:?}"
                );
            }
        }
    }

    mod closing_prs {
        use agentdeck_core::PrState;

        use super::*;
        use crate::overview::fixtures::pr_task;

        /// Workspace 1 with an open PR 5 and a plain task 6, both in review, and a todo task.
        fn with_prs(panel: &Entity<RightPanel>, cx: &mut VisualTestContext) {
            let mut snapshot = snapshot();
            let ws = Some(id(1));
            snapshot
                .tasks
                .push(pr_task(5, "acme/widgets", PrState::Open, ws));
            snapshot.tasks.push(Task {
                status: TaskStatus::Review,
                ..task(6, TaskKind::Plain, ws)
            });
            panel.update(cx, |panel, cx| panel.show(id(1), snapshot, cx));
        }

        fn click(selector: &'static str, cx: &mut VisualTestContext) {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let bounds = cx.debug_bounds(selector).expect(selector);
            cx.simulate_click(bounds.center(), Modifiers::default());
            cx.run_until_parked();
        }

        #[gpui::test]
        fn should_count_the_picked_prs_closing_from_the_snapshots_it_is_shown(
            cx: &mut TestAppContext,
        ) {
            let (panel, api, cx) = open_with_api(cx);
            with_prs(&panel, cx);
            click("panel select", cx);
            click("panel select all review", cx);
            click("panel close prs", cx);
            let release = api.hold_close();
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
            let closing = |cx: &mut VisualTestContext| {
                panel.read_with(cx, |panel, cx| panel.close_prs().read(cx).closing())
            };
            assert_eq!(closing(cx), Some((0, 1)));

            let mut snapshot = snapshot();
            let mut pr = pr_task(5, "acme/widgets", PrState::Closed, Some(id(1)));
            pr.status = TaskStatus::Done;
            snapshot.tasks.push(pr);
            panel.update(cx, |panel, cx| panel.show(id(1), snapshot, cx));
            cx.update(|window, cx| window.draw(cx).clear(cx));
            assert_eq!(closing(cx), Some((1, 1)));

            release.send(()).unwrap();
            cx.run_until_parked();
            assert_eq!(closing(cx), None);
        }

        #[gpui::test]
        fn should_pin_the_selection_bar_to_the_panels_bottom(cx: &mut TestAppContext) {
            let (panel, cx) = open(cx);
            with_prs(&panel, cx);

            click("panel select", cx);
            cx.update(|window, cx| window.draw(cx).clear(cx));

            let bar = cx.debug_bounds("panel selection bar").expect("the bar");
            let viewport = cx.update(|window, _| window.viewport_size());
            assert_eq!(bar.bottom(), viewport.height);
        }

        #[gpui::test]
        fn should_check_only_the_groups_and_rows_with_an_open_pr(cx: &mut TestAppContext) {
            let (panel, cx) = open(cx);
            with_prs(&panel, cx);

            click("panel select", cx);

            let task: TaskId = id(5);
            assert!(cx.debug_bounds("panel select all review").is_some());
            assert!(cx.debug_bounds("panel select all doing").is_none());
            assert!(
                cx.debug_bounds(format!("panel select {task}").leak())
                    .is_some()
            );
            let plain: TaskId = id(6);
            assert!(
                cx.debug_bounds(format!("panel select {plain}").leak())
                    .is_none()
            );
            assert!(
                cx.debug_bounds(format!("panel menu {task}").leak())
                    .is_some()
            );
            assert!(
                cx.debug_bounds(format!("panel menu {plain}").leak())
                    .is_some(),
                "every row has a menu, for Edit task"
            );
        }

        #[gpui::test]
        fn should_pick_from_the_group_checkbox_without_folding_the_group(cx: &mut TestAppContext) {
            let (panel, cx) = open(cx);
            with_prs(&panel, cx);
            click("panel select", cx);

            click("panel select all review", cx);

            let picked = panel.read_with(cx, |panel, cx| {
                panel.close_prs().read(cx).selection().ids().to_vec()
            });
            assert_eq!(picked, [id(5)]);
            assert!(!panel.read_with(cx, |panel, _| panel.is_collapsed(TaskStatus::Review)));
        }
    }

    mod long_list {
        use agentdeck_client::types::Instruction;
        use agentdeck_core::InstructionId;
        use gpui::{ScrollDelta, ScrollEvent, point};

        use super::*;
        use crate::overview::fixtures::instruction;
        use crate::scrollbar::testing::thumb;
        use crate::state_dot::tests::{activate, redraw};

        /// Workspace 1 with tasks 1 to 100 in Doing, an agent at work on task 1, and
        /// instruction 200.
        fn crowded() -> Snapshot {
            let ws = workspace(1, "a");
            Snapshot {
                tasks: (1..=100)
                    .map(|n| with_status(n, TaskStatus::Doing, ws.id))
                    .collect(),
                panes: vec![PaneState {
                    task: Some(id(1)),
                    ..pane(300, ws.id, AgentState::Working)
                }],
                instructions: vec![instruction(200, ws.id)],
                workspaces: vec![ws],
                ..Snapshot::default()
            }
        }

        fn open_crowded(cx: &mut TestAppContext) -> (Entity<RightPanel>, &mut VisualTestContext) {
            let (panel, cx) = open(cx);
            panel.update(cx, |p, cx| p.show(id(1), crowded(), cx));
            settle(cx);
            (panel, cx)
        }

        fn settle(cx: &mut VisualTestContext) {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }

        fn shows(selector: String, cx: &mut VisualTestContext) -> bool {
            cx.debug_bounds(selector.leak()).is_some()
        }

        fn task_shows(n: u32, cx: &mut VisualTestContext) -> bool {
            shows(format!("panel task {}", id::<TaskId>(n)), cx)
        }

        fn rendered(panel: &Entity<RightPanel>, cx: &mut VisualTestContext) -> usize {
            panel.read_with(cx, |p, _| p.items_rendered.get())
        }

        /// The most items the list's height holds, counting what it draws past either end:
        /// as many as the shortest item, a 32px header, would fill.
        fn in_view(cx: &mut VisualTestContext) -> usize {
            let list = cx.debug_bounds("panel list").expect("the list");
            ((list.size.height + task_row::OVERDRAW * 2.) / space::S8).ceil() as usize + 2
        }

        /// Wheels the list down by `by`.
        fn wheel(by: f32, cx: &mut VisualTestContext) {
            let list = cx.debug_bounds("panel list").expect("the list");
            cx.simulate_event(ScrollEvent {
                position: list.center(),
                delta: ScrollDelta::Pixels(point(px(0.), px(-by))),
                ..ScrollEvent::default()
            });
            settle(cx);
        }

        fn wheel_to_the_end(cx: &mut VisualTestContext) {
            wheel(20_000., cx);
        }

        fn wheel_to_the_top(cx: &mut VisualTestContext) {
            wheel(-20_000., cx);
        }

        #[gpui::test]
        fn should_draw_only_the_rows_in_view(cx: &mut TestAppContext) {
            let (_, cx) = open_crowded(cx);

            assert!(task_shows(1, cx));
            assert!(!task_shows(100, cx));
        }

        #[gpui::test]
        fn should_render_only_the_items_in_view_on_a_redraw(cx: &mut TestAppContext) {
            let (panel, cx) = open_crowded(cx);
            let (before, in_view) = (rendered(&panel, cx), in_view(cx));
            assert!(in_view < 100, "the window holds {in_view} items");

            redraw(&panel, 10, cx);

            let per_redraw = (rendered(&panel, cx) - before) / 10;
            assert!(
                per_redraw <= in_view,
                "{per_redraw} items a redraw, {in_view} in view"
            );
        }

        #[gpui::test]
        fn should_stretch_every_item_across_the_list(cx: &mut TestAppContext) {
            let (panel, cx) = open_crowded(cx);
            let mut short = crowded();
            short.tasks[1].title = "x".into();
            panel.update(cx, |p, cx| p.show(id(1), short, cx));
            settle(cx);

            let width = |selector: String, cx: &mut VisualTestContext| {
                cx.debug_bounds(selector.clone().leak())
                    .expect(&selector)
                    .size
                    .width
            };
            let field = width("new instruction".into(), cx);
            let card = width(format!("instruction {}", id::<InstructionId>(200)), cx);
            let long = width(format!("panel task {}", id::<TaskId>(1)), cx);
            let short = width(format!("panel task {}", id::<TaskId>(2)), cx);
            assert_eq!([card, long, short], [field; 3]);
        }

        #[gpui::test]
        fn should_draw_the_rows_a_scroll_brings_into_view(cx: &mut TestAppContext) {
            let (_, cx) = open_crowded(cx);

            wheel_to_the_end(cx);

            assert!(task_shows(100, cx));
            assert!(!task_shows(1, cx));
            assert!(!shows("add instruction".into(), cx));
        }

        #[gpui::test]
        fn should_open_a_row_scrolled_into_view(cx: &mut TestAppContext) {
            let (panel, cx) = open_crowded(cx);
            let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let seen = asked.clone();
            let _asks = cx.update(|_, cx| {
                cx.subscribe(&panel, move |_, event: &OpenTask, _| {
                    seen.borrow_mut().push(*event)
                })
            });
            wheel_to_the_end(cx);

            let title = format!("task title {}", id::<TaskId>(100));
            let bounds = cx.debug_bounds(title.leak()).expect("the title shows");
            cx.simulate_click(bounds.center(), Modifiers::default());
            cx.run_until_parked();

            assert_eq!(*asked.borrow(), [OpenTask(id(100))]);
        }

        #[gpui::test]
        fn should_keep_its_place_when_the_snapshot_changes(cx: &mut TestAppContext) {
            let (panel, cx) = open_crowded(cx);
            wheel_to_the_end(cx);

            let mut changed = crowded();
            changed.tasks[50].title = "renamed".into();
            panel.update(cx, |p, cx| p.show(id(1), changed, cx));
            settle(cx);

            assert!(task_shows(100, cx));
            assert!(!task_shows(1, cx));
        }

        #[gpui::test]
        fn should_reach_the_rows_a_snapshot_adds(cx: &mut TestAppContext) {
            let (panel, cx) = open_crowded(cx);

            let mut more = crowded();
            more.tasks.extend(
                (101..=120).map(|n| with_status(n, TaskStatus::Doing, more.workspaces[0].id)),
            );
            panel.update(cx, |p, cx| p.show(id(1), more, cx));
            settle(cx);
            wheel_to_the_end(cx);

            assert!(task_shows(120, cx));
        }

        /// A body of about a dozen lines at the card's width.
        fn long_body() -> String {
            "Run cargo ci before you push and fix whatever it reports. ".repeat(8)
        }

        fn with_long_instruction() -> Snapshot {
            let mut snapshot = crowded();
            snapshot.instructions = vec![Instruction {
                body: long_body(),
                ..instruction(200, id(1))
            }];
            snapshot
        }

        fn top_of(selector: &'static str, cx: &mut VisualTestContext) -> gpui::Pixels {
            cx.debug_bounds(selector).expect(selector).top()
        }

        #[gpui::test]
        fn should_move_the_items_below_down_when_an_instruction_shows_whole(
            cx: &mut TestAppContext,
        ) {
            let (panel, cx) = open_crowded(cx);
            panel.update(cx, |p, cx| p.show(id(1), with_long_instruction(), cx));
            settle(cx);
            let before = top_of("add task", cx);

            let more = format!("instruction more {}", id::<InstructionId>(200));
            let button = cx.debug_bounds(more.leak()).expect("Show more");
            cx.simulate_click(button.center(), Modifiers::default());
            settle(cx);

            assert!(top_of("add task", cx) > before);
        }

        #[gpui::test]
        fn should_measure_an_instruction_again_once_its_text_changes_out_of_view(
            cx: &mut TestAppContext,
        ) {
            let (panel, cx) = open_crowded(cx);
            let list = cx.debug_bounds("panel list").expect("the list");
            wheel_to_the_end(cx);
            let short = thumb(list, cx).expect("a thumb").size.height;

            panel.update(cx, |p, cx| p.show(id(1), with_long_instruction(), cx));
            settle(cx);

            let longer = thumb(list, cx).expect("a thumb").size.height;
            assert!(longer < short, "{longer:?} after {short:?}: the list grew");
        }

        #[gpui::test]
        fn should_keep_typing_into_the_new_instruction_field_scrolled_out_of_view(
            cx: &mut TestAppContext,
        ) {
            let (panel, cx) = open_crowded(cx);
            activate(cx);
            cx.update(|window, cx| {
                let input = panel.read(cx).new_instruction.clone();
                input.update(cx, |input, cx| input.focus(window, cx));
            });
            settle(cx);

            wheel_to_the_end(cx);
            cx.simulate_input("still here");
            settle(cx);

            assert_eq!(typed(&panel, cx), "still here");
            wheel_to_the_top(cx);
            assert!(shows("new instruction".into(), cx));
        }

        #[gpui::test]
        fn should_keep_typing_into_an_instruction_editor_scrolled_out_of_view(
            cx: &mut TestAppContext,
        ) {
            let (panel, cx) = open_crowded(cx);
            activate(cx);
            cx.update(|window, cx| {
                panel.update(cx, |p, cx| p.edit_instruction(id(200), window, cx));
            });
            settle(cx);

            wheel_to_the_end(cx);
            cx.simulate_input("!");
            settle(cx);

            let text = panel.read_with(cx, |p, cx| {
                let (_, editor) = p.editor.as_ref().expect("an editor is open");
                editor.read(cx).value().to_string()
            });
            assert!(text.contains('!'), "{text:?}");
        }
    }

    mod spin {
        use super::*;
        use crate::state_dot::tests::{activate, redraws_in_one_second};

        /// Opens the panel on workspace 1, whose doing task an agent is working on.
        fn working(cx: &mut TestAppContext) -> (Entity<RightPanel>, &mut VisualTestContext) {
            let (panel, cx) = open(cx);
            panel.update(cx, |p, cx| {
                let mut snapshot = snapshot();
                snapshot.panes.push(PaneState {
                    task: Some(id(1)),
                    ..pane(10, snapshot.workspaces[0].id, AgentState::Working)
                });
                p.show(id(1), snapshot, cx);
            });
            activate(cx);
            (panel, cx)
        }

        #[gpui::test]
        fn should_hold_a_working_agents_mark_still_while_the_window_is_active(
            cx: &mut TestAppContext,
        ) {
            let (panel, cx) = working(cx);
            assert_eq!(redraws_in_one_second(&panel, cx), 0);
        }
    }
}
