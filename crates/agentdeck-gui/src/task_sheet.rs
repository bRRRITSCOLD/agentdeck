//! The task sheet (spec §11), Penpot "Screens" → "Task sheet": a right-side `Sheet` on one task.
//! Its header names the task and who is on it; its body (Penpot `agentdeck/TaskSheetBody`)
//! holds the status picker and where the task lives, its description, its PR (its reference a
//! link to the PR on GitHub, Penpot "Task sheet · PR link"), and the comment thread, oldest
//! first; its footer (`agentdeck/CommentComposer`) writes a comment for the
//! task's agent. Enter is a newline there; Ctrl+Enter or **Comment** sends.
//!
//! While no agent works on the task, the status row ends in **Start agent**, which closes the
//! sheet and asks the window for New agent on the task's workspace with [`StartAgentOn`]. The
//! header's `…` menu (Penpot "Task sheet · send to agent") sends the task to a live agent pane
//! ([`send_task`]) and copies its id.
//!
//! The task is edited in place (spec §11, Editing a task; Penpot "Task sheet · edit title",
//! "… · title error", "… · edit description", "… · workspace select", "… · PR task"): a click on
//! the title turns it into an input (Enter or leaving it saves, Escape cancels), a click on the
//! description into a textarea (Ctrl+Enter or Save saves, Escape or Cancel cancels), and the
//! workspace picker moves the task as soon as one is picked. A PR task's title is its PR's,
//! so it isn't editable. A row's **Edit task** opens the sheet editing it ([`EditTask`]).
//!
//! Clicking a task's title in the Tasks view, the Inbox or the right panel asks the window
//! for it with [`OpenTask`]. Opening it reads the thread and marks the task's agent comments
//! seen, which clears the row's unseen mark. While open, a `changed comment` reads the thread again.
//! The task itself comes from the sidebar's [`Snapshot`], passed on by the window.

use std::rc::Rc;
use std::sync::Arc;

use agentdeck_client::types::{Comment, Task, TaskPatch};
use agentdeck_core::ws::{Entity as Changed, ServerMessage};
use agentdeck_core::{
    Agent, AgentState, CommentAuthor, TaskId, TaskKind, TaskPlace, TaskStatus, WorkspaceId,
};
use brrritscold_core_gpui::{
    ActiveTheme as _, Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, DialogHandle,
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, FieldError, Icon, IconName, Input,
    InputState, Select, SelectTrigger, Sheet, SheetContent, SheetDescription, SheetFooter,
    SheetTitle, Textarea, TextareaState, Tooltip, TooltipContent, TooltipContentSide,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use chrono::{DateTime, Utc};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, px,
};
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::connection::{Connection, DaemonEvent};
use crate::keymap::{self, CancelEdit, Submit};
use crate::new_agent::AGENTS;
use crate::new_task::{self, INBOX};
use crate::open_pr;
use crate::overview::{Snapshot, ago};
use crate::pane_header;
use crate::rereads::Rereads;
use crate::send_task::{self, Sending, TaskTarget};
use crate::task_list::{self, PrCell};
use crate::task_row;
use crate::workspace::failed;

/// A view asks the window for the task sheet on this task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenTask(pub TaskId);

/// A row's **Edit task** asks the window for the task sheet on this task, editing it: the
/// title of a plain task, the description of a PR task, whose title is its PR's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditTask(pub TaskId);

/// The sheet's Start agent asks the window for New agent on `workspace`, the task's, with
/// `task` picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartAgentOn {
    /// The task's workspace.
    pub workspace: WorkspaceId,
    /// The task.
    pub task: TaskId,
}

/// The status picker's width. Raw in Penpot too.
const STATUS_WIDTH: f32 = 120.;

/// What the sheet shows of the task, from the snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// The task's title: the sheet's title.
    pub title: SharedString,
    /// Who is on it: `Claude Code is working on it.`, or `No agent on it.`
    pub on_it: SharedString,
    /// Its status, which the picker shows.
    pub status: TaskStatus,
    /// Whether its title can be edited: a PR task's is its PR's.
    pub title_editable: bool,
    /// Where it lives, as the workspace picker shows it: `tabs-a11y`, or `Inbox`.
    pub place: SharedString,
    /// The workspace the picker has picked: the task's, while it is one the task could move to.
    /// `None` in the inbox and in an archived workspace, where the picker shows `place` instead.
    pub workspace: Option<WorkspaceId>,
    /// The workspaces the picker offers after the Inbox, oldest first.
    pub places: Vec<(WorkspaceId, SharedString)>,
    /// Its description; `None` when it has none.
    pub description: Option<SharedString>,
    /// Its PR, for a PR task the sync has filled in.
    pub pr: Option<PrCell>,
    /// The Start agent button at the end of the status row.
    pub start_agent: StartAgent,
    /// The panes the sheet menu's Send to agent offers; `None` for a done task, whose menu
    /// has no Send to agent.
    pub send_to: Option<Vec<TaskTarget>>,
    /// Whether it is in the Inbox, so a send moves it to the pane's workspace first.
    pub inbox: bool,
}

/// The sheet's Start agent button (Penpot `TaskSheetBody` → `StartAgent`), shown while no
/// live agent pane is linked to the task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartAgent {
    /// Not shown: an agent is working on the task.
    Hidden,
    /// Opens New agent on this workspace, the task's, with the task picked.
    On(WorkspaceId),
    /// Shown disabled, with this tooltip: the task has nowhere to start one.
    Disabled(SharedString),
}

/// `task`'s summary in `snapshot`, if it holds the task.
pub fn summary(snapshot: &Snapshot, task: TaskId) -> Option<Summary> {
    let held: &Task = snapshot.tasks.iter().find(|t| t.id == task)?;
    let working = (snapshot.panes.iter())
        .filter(|p| p.task == Some(task) && p.state != AgentState::Exited)
        .find_map(|p| p.agent);
    let on_it = match working {
        Some(agent) => format!("{} is working on it.", agent_name(agent)),
        None => "No agent on it.".to_owned(),
    };
    let workspace =
        (held.workspace_id).and_then(|ws| snapshot.workspaces.iter().find(|w| w.id == ws));
    let place = workspace.map_or("Inbox".to_owned(), |w| w.name.clone());
    let start_agent = match (working, workspace) {
        (Some(_), _) => StartAgent::Hidden,
        (None, None) => {
            StartAgent::Disabled("Move this task to a workspace to start an agent on it.".into())
        }
        (None, Some(w)) if w.archived_at.is_some() => {
            StartAgent::Disabled(format!("Unarchive {} to start an agent on it.", w.name).into())
        }
        (None, Some(w)) => StartAgent::On(w.id),
    };
    let row = task_list::rows(
        snapshot,
        &task_list::Query {
            scope: task_list::Scope::All,
            filter: "",
            show_done: true,
        },
    )
    .into_iter()
    .find(|r| r.id == task);
    Some(Summary {
        title: held.title.clone().into(),
        on_it: on_it.into(),
        status: held.status,
        title_editable: held.kind == TaskKind::Plain,
        place: place.into(),
        workspace: workspace.filter(|w| w.archived_at.is_none()).map(|w| w.id),
        places: new_task::places(snapshot),
        description: (!held.body.trim().is_empty()).then(|| held.body.clone().into()),
        pr: row.and_then(|r| r.pr),
        start_agent,
        send_to: send_task::targets(snapshot, task),
        inbox: held.workspace_id.is_none(),
    })
}

/// One comment, as the thread shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentRow {
    /// Who wrote it: `You`, or the agent with its glyph (`✳ Claude Code`).
    pub author: SharedString,
    /// How long ago: `2 h ago`.
    pub ago: SharedString,
    /// Its text.
    pub body: SharedString,
    /// For yours, whether it reached the agent: `Delivered to Claude Code` or `Waiting for
    /// the agent`; `None` for an agent's.
    pub delivery: Option<SharedString>,
}

/// `comments` as the thread shows them, in their order, at `now`.
pub fn comment_rows(
    snapshot: &Snapshot,
    comments: &[Comment],
    now: DateTime<Utc>,
) -> Vec<CommentRow> {
    comments
        .iter()
        .map(|comment| {
            let (author, delivery) = match comment.author {
                CommentAuthor::User => ("You".to_owned(), Some(delivery(snapshot, comment))),
                CommentAuthor::Agent => (
                    comment.author_agent.map_or("Shell".to_owned(), agent_label),
                    None,
                ),
            };
            let ago = (comment.created_at.parse::<DateTime<Utc>>())
                .map_or(String::new(), |created| ago(now - created));
            CommentRow {
                author: author.into(),
                ago: ago.into(),
                body: comment.body.clone().into(),
                delivery: delivery.map(SharedString::from),
            }
        })
        .collect()
}

/// Whether the user's `comment` reached an agent, and which.
fn delivery(snapshot: &Snapshot, comment: &Comment) -> String {
    if comment.delivered_at.is_none() {
        return "Waiting for the agent".to_owned();
    }
    let agent = (comment.delivered_to_pane)
        .and_then(|pane| snapshot.panes.iter().find(|p| p.pane == pane))
        .and_then(|p| p.agent);
    match agent {
        Some(agent) => format!("Delivered to {}", agent_name(agent)),
        None => "Delivered to the agent".to_owned(),
    }
}

/// The agent's name: `Claude Code`, or `Agent` for another CLI.
pub(crate) fn agent_name(agent: Agent) -> &'static str {
    (AGENTS.iter())
        .find(|(a, _)| *a == agent)
        .map_or("Agent", |(_, name)| name)
}

/// The agent's name after the glyph it puts in its pane's title, as pane headers show it:
/// `✳ Claude Code`. Codex puts none.
pub(crate) fn agent_label(agent: Agent) -> String {
    match agent {
        Agent::Claude => format!("✳ {}", agent_name(agent)),
        Agent::Codex | Agent::Other => agent_name(agent).to_owned(),
    }
}

/// What the sheet edits in place.
enum Editor {
    /// The title, in an input in the header: Enter or leaving it saves, Escape cancels.
    Title(Entity<InputState>),
    /// The description, in a textarea: Ctrl+Enter or Save saves, Escape or Cancel cancels.
    Description(Entity<TextareaState>),
}

/// What the sheet is editing.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Editing {
    /// The title.
    Title,
    /// The description.
    Description,
}

/// The error under a title left blank (Penpot "Task sheet · title error").
pub const BLANK_TITLE: &str = "Give the task a title.";

/// The sheet, kept by the window.
pub struct TaskSheet {
    api: Arc<dyn Api>,
    handle: DialogHandle,
    /// The task shown, once one was opened.
    task: Option<TaskId>,
    snapshot: Snapshot,
    /// The task's thread, as last read.
    comments: Vec<Comment>,
    rereads: Rereads,
    /// The comment being written (Penpot `CommentComposer`).
    composer: Entity<TextareaState>,
    /// Whether the comment written is being sent; another waits for it.
    sending: bool,
    /// The title or description being edited, if any.
    editor: Option<Editor>,
    _editor: Option<Subscription>,
    /// Whether the title being edited was left blank; it shows the field's error.
    blank_title: bool,
    /// Whether an edit is being saved; another waits for it.
    saving: bool,
    _composer: Subscription,
    _events: Subscription,
}

impl EventEmitter<StartAgentOn> for TaskSheet {}

impl TaskSheet {
    /// A closed sheet that reads and writes comments through `api`, and follows `connection`'s
    /// `changed comment` while open.
    pub fn new(
        api: Arc<dyn Api>,
        connection: &Entity<Connection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Comment for the agent on this task…")
                .auto_grow(2, 6)
        });
        // Comment follows whether the text is blank.
        let _composer = cx.subscribe(&composer, |_, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                cx.notify();
            }
        });
        let _events = cx.subscribe(connection, |this, _, event, cx| {
            if let DaemonEvent::Message(
                ServerMessage::Changed {
                    entity: Changed::Comment,
                    ..
                }
                | ServerMessage::Resync,
            ) = event
                && this.is_open()
            {
                this.reread(cx);
            }
        });
        TaskSheet {
            api,
            handle: DialogHandle::new(false),
            task: None,
            snapshot: Snapshot::default(),
            comments: Vec::new(),
            rereads: Rereads::default(),
            composer,
            sending: false,
            editor: None,
            _editor: None,
            blank_title: false,
            saving: false,
            _composer,
            _events,
        }
    }

    /// Opens it on `task`, reading its thread and marking its agent comments seen. A comment
    /// half written for another task is dropped.
    pub fn open(
        &mut self,
        task: TaskId,
        snapshot: Snapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.task != Some(task) {
            self.comments.clear();
            self.composer
                .update(cx, |composer, cx| composer.set_value("", window, cx));
            self.cancel_edit(cx);
        }
        self.task = Some(task);
        self.snapshot = snapshot;
        self.handle.open(window, cx);
        self.reread(cx);
        self.mark_seen(task, cx);
        cx.notify();
    }

    /// Shows `snapshot`'s copy of the task.
    pub fn set_snapshot(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.snapshot = snapshot;
        cx.notify();
    }

    /// Whether it is open.
    pub fn is_open(&self) -> bool {
        self.handle.is_open()
    }

    /// The task shown.
    pub fn task(&self) -> Option<TaskId> {
        self.task
    }

    /// The thread, as shown now.
    pub fn thread(&self) -> Vec<CommentRow> {
        comment_rows(&self.snapshot, &self.comments, Utc::now())
    }

    /// Reads the thread again; one read at a time.
    fn reread(&mut self, cx: &mut Context<Self>) {
        let Some(task) = self.task else {
            return;
        };
        if !self.rereads.ask() {
            return;
        }
        let reading = self.api.comments(task, cx);
        cx.spawn(async move |this, cx| {
            let read = reading.await;
            let _ = this.update(cx, |this, cx| {
                // Read for a task since swapped for another: drop it.
                if this.task == Some(task)
                    && let Ok(comments) = read
                {
                    let unseen = (comments.iter())
                        .any(|c| c.author == CommentAuthor::Agent && c.seen_at.is_none());
                    this.comments = comments;
                    if unseen && this.is_open() {
                        this.mark_seen(task, cx);
                    }
                }
                if this.rereads.landed() {
                    this.reread(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Marks `task`'s agent comments seen. A failure only leaves the unseen mark on its row.
    fn mark_seen(&self, task: TaskId, cx: &mut Context<Self>) {
        self.api.mark_comments_seen(task, cx).detach();
    }

    /// Whether Comment can go ahead: the text isn't blank and nothing is being sent.
    pub fn can_comment(&self, cx: &App) -> bool {
        !self.sending && !self.composer.read(cx).value().trim().is_empty()
    }

    /// Sends the comment written, trimmed, then empties the composer; a failure keeps the
    /// text and toasts why. What Comment and Ctrl+Enter do.
    pub fn send_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self.task.filter(|_| self.can_comment(cx)) else {
            return;
        };
        let body = self.composer.read(cx).value().trim().to_owned();
        self.sending = true;
        cx.notify();
        let sending = self.api.new_comment(task, body, cx);
        cx.spawn_in(window, async move |this, cx| {
            let sent = sending.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                match sent {
                    Ok(()) => {
                        this.composer
                            .update(cx, |composer, cx| composer.set_value("", window, cx));
                        // The daemon's `changed comment` reads it back too; this doesn't wait.
                        this.reread(cx);
                    }
                    Err(err) => failed("Couldn't send the comment", &err, window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens it on `task` editing it, as a row's **Edit task** asks: its title, or for a PR
    /// task, whose title is its PR's, its description.
    pub fn open_editing(
        &mut self,
        task: TaskId,
        snapshot: Snapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open(task, snapshot, window, cx);
        match summary(&self.snapshot, task) {
            Some(summary) if summary.title_editable => self.edit_title(window, cx),
            Some(_) => self.edit_description(window, cx),
            None => {}
        }
    }

    /// What is being edited, if anything.
    #[cfg(test)]
    pub fn editing(&self) -> Option<Editing> {
        self.editor.as_ref().map(|editor| match editor {
            Editor::Title(_) => Editing::Title,
            Editor::Description(_) => Editing::Description,
        })
    }

    /// The task shown, as the snapshot holds it.
    fn held(&self) -> Option<&Task> {
        let task = self.task?;
        self.snapshot.tasks.iter().find(|t| t.id == task)
    }

    /// Turns the title into an input holding it, focused: what a click on the title does.
    /// Nothing for a PR task, whose title is its PR's.
    pub fn edit_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = (self.held())
            .filter(|t| t.kind == TaskKind::Plain)
            .map(|t| t.title.clone())
        else {
            return;
        };
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_value(title, window, cx);
            input
        });
        self._editor = Some(
            cx.subscribe_in(&input, window, |this, _, event, window, cx| {
                match event {
                    InputEvent::PressEnter { .. } | InputEvent::Blur => this.save_edit(window, cx),
                    // The error shows while the title stays blank; typing redraws it.
                    InputEvent::Change => cx.notify(),
                    InputEvent::Focus => {}
                }
            }),
        );
        self.start_editing(Editor::Title(input), window, cx);
    }

    /// Turns the description into a textarea holding it, focused: what a click on the
    /// description does.
    pub fn edit_description(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(body) = self.held().map(|t| t.body.clone()) else {
            return;
        };
        let textarea = cx.new(|cx| {
            let mut textarea = TextareaState::new(window, cx).auto_grow(2, 8);
            textarea.set_value(body, window, cx);
            textarea
        });
        self._editor = Some(cx.subscribe(&textarea, |_, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                cx.notify();
            }
        }));
        self.start_editing(Editor::Description(textarea), window, cx);
    }

    /// Shows `editor`, focused once the click or the menu that asked has let focus go.
    fn start_editing(&mut self, editor: Editor, window: &mut Window, cx: &mut Context<Self>) {
        self.blank_title = false;
        match &editor {
            Editor::Title(input) => {
                let input = input.clone();
                window.defer(cx, move |window, cx| {
                    input.update(cx, |input, cx| input.focus(window, cx));
                });
            }
            Editor::Description(textarea) => {
                let textarea = textarea.clone();
                window.defer(cx, move |window, cx| {
                    textarea.update(cx, |textarea, cx| textarea.focus(window, cx));
                });
            }
        }
        self.editor = Some(editor);
        cx.notify();
    }

    /// Whether the title being edited shows the field's error: it was saved blank and still
    /// is.
    pub fn shows_blank_title(&self, cx: &App) -> bool {
        match &self.editor {
            Some(Editor::Title(input)) => {
                self.blank_title && input.read(cx).value().trim().is_empty()
            }
            _ => false,
        }
    }

    /// Drops the edit, leaving the task as it was: what Escape and Cancel do.
    pub fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.editor = None;
        self._editor = None;
        self.blank_title = false;
        cx.notify();
    }

    /// Saves what is being edited, trimmed, then closes the editor: what Enter, leaving the
    /// title, Ctrl+Enter and Save do. A blank title shows the field's error and stays open;
    /// an unchanged one just closes. A failure keeps the editor open and toasts why.
    pub fn save_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let (Some(task), Some(held)) = (self.task, self.held()) else {
            return;
        };
        let (old_title, old_body) = (held.title.clone(), held.body.clone());
        let patch = match &self.editor {
            None => return,
            Some(Editor::Title(input)) => {
                let title = input.read(cx).value().trim().to_owned();
                if title.is_empty() {
                    self.blank_title = true;
                    cx.notify();
                    return;
                }
                (title != old_title).then(|| TaskPatch {
                    title: Some(title),
                    ..TaskPatch::default()
                })
            }
            Some(Editor::Description(textarea)) => {
                let body = textarea.read(cx).value().trim().to_owned();
                (body != old_body.trim()).then(|| TaskPatch {
                    body: Some(body),
                    ..TaskPatch::default()
                })
            }
        };
        let Some(patch) = patch else {
            self.cancel_edit(cx);
            return;
        };
        self.saving = true;
        cx.notify();
        let saving = self.api.edit_task(task, patch, cx);
        cx.spawn_in(window, async move |this, cx| {
            let saved = saving.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match saved {
                    // The daemon's `changed task` brings the new text through the snapshot.
                    Ok(()) if this.task == Some(task) => this.cancel_edit(cx),
                    Ok(()) => {}
                    Err(err) => failed(format!("Couldn't save {old_title}"), &err, window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Moves the task to `place` at once: what picking in the workspace picker does. Picking
    /// where it already is does nothing; a failure toasts why.
    pub fn move_to(&mut self, place: TaskPlace, window: &mut Window, cx: &mut Context<Self>) {
        let Some(held) = self.held() else {
            return;
        };
        if TaskPlace::from(held.workspace_id) == place {
            return;
        }
        let (task, title) = (held.id, held.title.clone());
        let patch = TaskPatch {
            workspace_id: Some(place),
            ..TaskPatch::default()
        };
        let moving = self.api.edit_task(task, patch, cx);
        cx.spawn_in(window, async move |_, cx| {
            if let Err(err) = moving.await {
                let _ = cx.update(|window, cx| {
                    failed(format!("Couldn't save {title}"), &err, window, cx);
                });
            }
        })
        .detach();
    }

    /// Moves the task to `status`, reporting a failure in a toast.
    pub fn set_status(&mut self, status: TaskStatus, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self.task else {
            return;
        };
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

    /// Penpot `agentdeck/TaskSheetBody`: the status row, the description, the PR line, the
    /// "Comments" heading and the thread, `space.4` apart, padded `space.4` across.
    fn body(&self, summary: &Summary, cx: &mut Context<Self>) -> AnyElement {
        let s = cx.theme().semantic();
        let status = Select::new("task-sheet-status")
            .trigger(SelectTrigger::new().w(px(STATUS_WIDTH)))
            .content(task_row::status_content())
            .value(Some(summary.status.as_str()))
            .on_value_change(cx.listener(|this, value: &SharedString, window, cx| {
                if let Ok(status) = value.parse() {
                    this.set_status(status, window, cx);
                }
            }));
        let muted = |text: SharedString| {
            div()
                .text_size(font_size::SM)
                .text_color(s.muted_foreground)
                .child(text)
        };
        let description = match &self.editor {
            Some(Editor::Description(textarea)) => self.description_editor(textarea, cx),
            _ => div()
                .id("task-sheet-description")
                .debug_selector(|| "task sheet description".into())
                .cursor_text()
                .on_click(cx.listener(|this, _, window, cx| this.edit_description(window, cx)))
                .child(muted(
                    (summary.description.clone()).unwrap_or_else(|| "Add a description…".into()),
                ))
                .into_any_element(),
        };
        let thread = self.thread();
        div()
            .id("task-sheet-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(space::S4)
            .px(space::S4)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::S3)
                    .child(status)
                    .child(self.workspace_picker(summary, cx))
                    .children(self.start_agent_button(&summary.start_agent, cx)),
            )
            .child(description)
            .when_some(summary.pr.as_ref(), |body, pr| body.child(pr_line(pr, cx)))
            .child(
                div()
                    .text_size(font_size::SM)
                    .font_weight(font_weight::MEDIUM)
                    .text_color(s.foreground)
                    .child("Comments"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::S3)
                    .when(thread.is_empty(), |list| {
                        list.child(muted("No comments yet.".into()))
                    })
                    .children(thread.into_iter().map(|row| comment(row, cx))),
            )
            .into_any_element()
    }

    /// Penpot `TaskSheetBody` → `StartAgent`: an outline sm **Start agent** pushed to the end
    /// of the status row; disabled with a tooltip saying why when the task has nowhere to
    /// start one; nothing while an agent works on it.
    fn start_agent_button(&self, start: &StartAgent, cx: &mut Context<Self>) -> Option<AnyElement> {
        let button = Button::new("task-sheet-start-agent")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Sm)
            .label("Start agent");
        let shown = match start {
            StartAgent::Hidden => return None,
            StartAgent::On(_) => button
                .on_click(cx.listener(|this, _, window, cx| this.start_agent(window, cx)))
                .into_any_element(),
            StartAgent::Disabled(why) => Tooltip::new("task-sheet-start-agent-why")
                .trigger(button.disabled(true))
                .content(TooltipContent::new(why.clone()))
                .into_any_element(),
        };
        Some(
            div()
                .ml_auto()
                .debug_selector(|| "start agent".into())
                .child(shown)
                .into_any_element(),
        )
    }

    /// Penpot `TaskSheetBody` → `Workspace`: a Select of the Inbox and the unarchived
    /// workspaces, filling the status row, that moves the task as soon as one is picked. In
    /// the inbox or an archived workspace it picks nothing and shows `place` (`Inbox`).
    fn workspace_picker(&self, summary: &Summary, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex_1()
            .min_w_0()
            .debug_selector(|| "task sheet workspace".into())
            .child(
                Select::new("task-sheet-workspace")
                    .trigger(
                        SelectTrigger::new()
                            .w_full()
                            .placeholder(summary.place.clone()),
                    )
                    .content(new_task::place_content(&summary.places))
                    .value(summary.workspace.map(|w| SharedString::from(w.to_string())))
                    .on_value_change(cx.listener(|this, value: &SharedString, window, cx| {
                        let place = match value.as_ref() {
                            INBOX => Some(TaskPlace::Inbox),
                            id => id.parse().ok().map(TaskPlace::Workspace),
                        };
                        if let Some(place) = place {
                            this.move_to(place, window, cx);
                        }
                    })),
            )
            .into_any_element()
    }

    /// Penpot `TaskSheetBody` → `DescriptionEdit`: the description in a textarea over Cancel
    /// (outline sm) and Save (sm), as an instruction card's editor. Ctrl+Enter saves and
    /// Escape cancels.
    fn description_editor(
        &self,
        textarea: &Entity<TextareaState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .debug_selector(|| "task sheet description editor".into())
            .key_context(keymap::TASK_EDITOR)
            .on_action(cx.listener(|this, _: &Submit, window, cx| this.save_edit(window, cx)))
            .on_action(cx.listener(|this, _: &CancelEdit, _, cx| this.cancel_edit(cx)))
            .flex()
            .flex_col()
            .gap(space::S2)
            .child(Textarea::new(textarea))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(space::S2)
                    .child(
                        div().debug_selector(|| "cancel description".into()).child(
                            Button::new("task-sheet-description-cancel")
                                .variant(ButtonVariant::Outline)
                                .size(ButtonSize::Sm)
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| this.cancel_edit(cx))),
                        ),
                    )
                    .child(
                        div().debug_selector(|| "save description".into()).child(
                            Button::new("task-sheet-description-save")
                                .size(ButtonSize::Sm)
                                .label("Save")
                                .disabled(self.saving)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.save_edit(window, cx);
                                })),
                        ),
                    ),
            )
            .into_any_element()
    }

    /// The sheet's `Header`, with core-gpui's `SheetHeader` classes (`flex flex-col gap-1.5
    /// p-4`) around parts it can't hold: the title, which a click edits (an input clear of the
    /// close button while it does, Penpot "Task sheet · edit title"), or for a PR task a
    /// tooltip saying it is GitHub's (Penpot "Task sheet · PR task"); then who is on it, or
    /// the field's error while the title is blank.
    fn header(&self, summary: &Summary, cx: &mut Context<Self>) -> AnyElement {
        let title = match &self.editor {
            Some(Editor::Title(input)) => div()
                .debug_selector(|| "task sheet title editor".into())
                .key_context(keymap::TASK_EDITOR)
                .on_action(cx.listener(|this, _: &Submit, window, cx| this.save_edit(window, cx)))
                .on_action(cx.listener(|this, _: &CancelEdit, _, cx| this.cancel_edit(cx)))
                .mr(space::S18)
                .child(Input::new(input).aria_invalid(self.shows_blank_title(cx)))
                .into_any_element(),
            _ if summary.title_editable => div()
                .id("task-sheet-title")
                .debug_selector(|| "task sheet title".into())
                .mr(space::S18)
                .cursor_text()
                .on_click(cx.listener(|this, _, window, cx| this.edit_title(window, cx)))
                .child(SheetTitle::new(summary.title.clone()))
                .into_any_element(),
            _ => Tooltip::new("task-sheet-title-github")
                .trigger(
                    div()
                        .debug_selector(|| "task sheet title".into())
                        .mr(space::S18)
                        .child(SheetTitle::new(summary.title.clone())),
                )
                .content(
                    TooltipContent::new("The title comes from GitHub.")
                        .side(TooltipContentSide::Bottom),
                )
                .into_any_element(),
        };
        let under = if self.shows_blank_title(cx) {
            FieldError::new(BLANK_TITLE).into_any_element()
        } else {
            SheetDescription::new(summary.on_it.clone()).into_any_element()
        };
        div()
            .debug_selector(|| "task sheet header".into())
            .relative()
            .flex()
            .flex_col()
            .gap(space::S1_5)
            .p(space::S4)
            .child(title)
            .child(under)
            .child(self.menu(summary))
            .into_any_element()
    }

    /// The sheet's `…` menu (Penpot "Task sheet · send to agent"): a ghost icon-sm `Ellipsis`
    /// just left of the close button, holding **Send to agent** unless the task is done, then
    /// **Copy task id**, as a row's menu does.
    fn menu(&self, summary: &Summary) -> AnyElement {
        let content = DropdownMenuContent::new();
        let content = match (self.task, summary.send_to.clone()) {
            (Some(task), Some(targets)) => {
                let api = self.api.clone();
                let sending = Sending {
                    task,
                    title: summary.title.clone(),
                    inbox: summary.inbox,
                };
                let (trigger, sub) = send_task::submenu(
                    targets,
                    Rc::new(move |target, window, cx| {
                        send_task::send(api.clone(), sending.clone(), target, window, cx);
                    }),
                );
                content.sub(trigger, sub)
            }
            _ => content,
        };
        let task = self.task;
        let content = content.item(DropdownMenuItem::new("Copy task id").on_click(
            move |_, window, cx| {
                if let Some(task) = task {
                    crate::notice::copy(task.to_string(), "Copied task id", window, cx);
                }
            },
        ));
        div()
            .debug_selector(|| "task sheet menu".into())
            .absolute()
            .top(space::S4)
            .right(space::S12)
            .child(
                DropdownMenu::new("task-sheet-menu")
                    .trigger(
                        Button::new("task-sheet-menu-trigger")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::IconSm)
                            .icon(IconName::Ellipsis),
                    )
                    .content(content),
            )
            .into_any_element()
    }

    /// Closes the sheet and asks the window for New agent on the task's workspace with the
    /// task picked: what Start agent does. Nothing while the task has nowhere to start one.
    pub fn start_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self.task else {
            return;
        };
        let Some(StartAgent::On(workspace)) =
            summary(&self.snapshot, task).map(|summary| summary.start_agent)
        else {
            return;
        };
        self.handle.close(window, cx);
        cx.emit(StartAgentOn { workspace, task });
        cx.notify();
    }

    /// Penpot `agentdeck/CommentComposer`: the Textarea over "Ctrl+Enter to send" and a sm
    /// Comment button at the end of its row, disabled while the text is blank.
    fn composer(&self, cx: &mut Context<Self>) -> AnyElement {
        let s = cx.theme().semantic();
        div()
            .key_context(keymap::COMMENT_COMPOSER)
            .on_action(cx.listener(|this, _: &Submit, window, cx| this.send_comment(window, cx)))
            .flex()
            .flex_col()
            .gap(space::S2)
            .child(Textarea::new(&self.composer))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::S2)
                    .child(
                        div()
                            .flex_1()
                            .text_size(font_size::XS)
                            .text_color(s.muted_foreground)
                            .child("Ctrl+Enter to send"),
                    )
                    .child(
                        div().debug_selector(|| "send comment".into()).child(
                            Button::new("task-sheet-comment")
                                .size(ButtonSize::Sm)
                                .label("Comment")
                                .disabled(!self.can_comment(cx))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.send_comment(window, cx);
                                })),
                        ),
                    ),
            )
            .into_any_element()
    }
}

/// The PR line (Penpot `TaskSheetBody` → `PR`): its reference as a link to the PR, a draft
/// badge, its checks and its review.
fn pr_line(pr: &PrCell, cx: &App) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(space::S2)
        .child(pr_link(pr, cx))
        .when(pr.draft, |line| {
            line.child(Badge::new().variant(BadgeVariant::Outline).label("draft"))
        })
        .when_some(pr.checks, |line, state| line.child(task_row::checks(state)))
        .when_some(pr.review, |line, state| {
            line.child(task_row::review(state, cx))
        })
        .into_any_element()
}

/// Penpot `PR` → `Link`: the PR's reference (sm), `primary` and underlined on hover as
/// core-gpui's link button is, then a 12px `ExternalLink` `space.1` after it. A click opens
/// the PR's stored `url` on GitHub (spec §11, Opening PRs). Tests find it as
/// `task sheet pr link`.
fn pr_link(pr: &PrCell, cx: &App) -> AnyElement {
    let s = cx.theme().semantic();
    let url = pr.url.clone();
    div()
        .id("task-sheet-pr-link")
        .debug_selector(|| "task sheet pr link".into())
        .flex()
        .items_center()
        .gap(space::S1)
        .text_size(font_size::SM)
        .text_color(s.primary)
        .cursor_pointer()
        .hover(|style| style.underline())
        .on_click(move |_, window, cx| open_pr::open(&url, window, cx))
        .child(pr.reference.clone())
        .child(
            Icon::new(IconName::ExternalLink)
                .size(px(12.))
                .text_color(s.primary),
        )
        .into_any_element()
}

/// One comment: "Author · 2 h ago", its text (sm), and for yours the delivery line (xs,
/// muted), `space.1` apart.
fn comment(row: CommentRow, cx: &App) -> AnyElement {
    let s = cx.theme().semantic();
    div()
        .flex()
        .flex_col()
        .gap(space::S1)
        .child(
            div()
                .flex()
                .items_baseline()
                .child(
                    div()
                        .flex()
                        .text_size(font_size::SM)
                        .font_weight(font_weight::MEDIUM)
                        .text_color(s.foreground)
                        .children(pane_header::symbol_runs(&row.author)),
                )
                .child(
                    div()
                        .text_size(font_size::XS)
                        .text_color(s.muted_foreground)
                        .child(format!(" · {}", row.ago)),
                ),
        )
        .child(
            div()
                .text_size(font_size::SM)
                .text_color(s.foreground)
                .child(row.body),
        )
        .when_some(row.delivery, |c, delivery| {
            c.child(
                div()
                    .text_size(font_size::XS)
                    .text_color(s.muted_foreground)
                    .child(delivery),
            )
        })
        .into_any_element()
}

impl Render for TaskSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let summary = self.task.and_then(|task| summary(&self.snapshot, task));
        let mut content = SheetContent::new();
        if let Some(summary) = &summary {
            content = content
                .child(self.header(summary, cx))
                .child(self.body(summary, cx))
                .footer(SheetFooter::new().child(self.composer(cx)));
        }
        Sheet::new("task-sheet")
            .handle(self.handle.clone())
            .content(content)
    }
}

#[cfg(test)]
mod tests {
    use agentdeck_client::types::{PullRequest, Workspace};
    use agentdeck_core::{ChecksState, PaneId, PrState, ReviewState, TaskKind};

    use super::*;
    use crate::overview::PaneState;
    use crate::overview::fixtures::{comment, id, pane, task, workspace};

    const NOW: &str = "2026-11-13T12:00:00Z";

    fn now() -> DateTime<Utc> {
        NOW.parse().unwrap()
    }

    fn agent_pane(n: u32, ws: &Workspace, agent: Agent, task: u32) -> PaneState {
        PaneState {
            agent: Some(agent),
            task: Some(id(task)),
            ..pane(n, ws.id, AgentState::Working)
        }
    }

    /// Workspace 1 `tabs-a11y` holding task 10, "Tabs: arrow-key navigation".
    fn snapshot() -> Snapshot {
        let ws = workspace(1, "tabs-a11y");
        Snapshot {
            tasks: vec![Task {
                title: "Tabs: arrow-key navigation".into(),
                body: "Add Home/End and arrow-key navigation.".into(),
                status: TaskStatus::Doing,
                ..task(10, TaskKind::Plain, Some(ws.id))
            }],
            workspaces: vec![ws],
            ..Snapshot::default()
        }
    }

    mod summary {
        use super::*;

        #[test]
        fn should_say_which_agent_is_working_on_it() {
            let mut snapshot = snapshot();
            let ws = snapshot.workspaces[0].clone();
            snapshot.panes = vec![agent_pane(30, &ws, Agent::Claude, 10)];

            let summary = summary(&snapshot, id(10)).unwrap();

            assert_eq!(summary.on_it, "Claude Code is working on it.");
        }

        #[test]
        fn should_say_no_agent_is_on_it_without_a_live_agent_pane() {
            let mut snapshot = snapshot();
            let ws = snapshot.workspaces[0].clone();
            snapshot.panes = vec![
                PaneState {
                    state: AgentState::Exited,
                    ..agent_pane(30, &ws, Agent::Claude, 10)
                },
                PaneState {
                    task: Some(id(10)),
                    ..pane(31, ws.id, AgentState::Idle)
                },
                agent_pane(32, &ws, Agent::Codex, 11),
            ];

            assert_eq!(summary(&snapshot, id(10)).unwrap().on_it, "No agent on it.");
        }

        #[test]
        fn should_place_it_in_its_workspace_with_its_status_and_description() {
            let summary = summary(&snapshot(), id(10)).unwrap();

            assert_eq!(summary.title, "Tabs: arrow-key navigation");
            assert_eq!(summary.place, "tabs-a11y");
            assert_eq!(summary.workspace, Some(id(1)));
            assert!(summary.title_editable);
            assert_eq!(summary.status, TaskStatus::Doing);
            assert_eq!(
                summary.description.as_deref(),
                Some("Add Home/End and arrow-key navigation.")
            );
        }

        #[test]
        fn should_place_an_inbox_task_in_the_inbox_with_its_pr() {
            let snapshot = Snapshot {
                tasks: vec![Task {
                    pr: Some(PullRequest {
                        checks: Some(ChecksState::Success),
                        review: Some(ReviewState::Approved),
                        ..pr_info()
                    }),
                    ..task(11, TaskKind::Pr, None)
                }],
                ..Snapshot::default()
            };

            let summary = summary(&snapshot, id(11)).unwrap();

            assert_eq!(summary.place, "Inbox");
            assert_eq!(summary.workspace, None);
            assert!(!summary.title_editable, "a PR task's title is its PR's");
            assert_eq!(summary.description, None);
            let pr = summary.pr.expect("its PR");
            assert_eq!(pr.reference, "ts-packages #142");
            assert_eq!(pr.checks, Some(ChecksState::Success));
        }

        #[test]
        fn should_offer_the_unarchived_workspaces_oldest_first() {
            let mut snapshot = snapshot();
            snapshot.workspaces.push(Workspace {
                archived_at: Some(NOW.into()),
                ..workspace(2, "old")
            });
            snapshot.workspaces.push(workspace(3, "gpui-terminal"));

            let summary = summary(&snapshot, id(10)).unwrap();

            assert_eq!(
                summary.places,
                [(id(1), "tabs-a11y".into()), (id(3), "gpui-terminal".into())]
            );
        }

        #[test]
        fn should_pick_nothing_and_name_an_archived_workspace() {
            let mut snapshot = snapshot();
            snapshot.workspaces[0].archived_at = Some(NOW.into());

            let summary = summary(&snapshot, id(10)).unwrap();

            assert_eq!(summary.place, "tabs-a11y");
            assert_eq!(summary.workspace, None);
            assert!(summary.places.is_empty());
        }

        #[test]
        fn should_be_none_for_a_task_it_doesnt_hold() {
            assert_eq!(summary(&snapshot(), id(99)), None);
        }

        mod start_agent {
            use super::*;

            #[test]
            fn should_start_on_its_workspace_with_no_agent_on_it() {
                let summary = summary(&snapshot(), id(10)).unwrap();

                assert_eq!(summary.start_agent, StartAgent::On(id(1)));
            }

            #[test]
            fn should_be_hidden_while_an_agent_works_on_it() {
                let mut snapshot = snapshot();
                let ws = snapshot.workspaces[0].clone();
                snapshot.panes = vec![agent_pane(30, &ws, Agent::Claude, 10)];

                let summary = summary(&snapshot, id(10)).unwrap();

                assert_eq!(summary.start_agent, StartAgent::Hidden);
            }

            #[test]
            fn should_be_disabled_for_an_inbox_task_saying_to_move_it() {
                let snapshot = Snapshot {
                    tasks: vec![task(11, TaskKind::Plain, None)],
                    ..snapshot()
                };

                let summary = summary(&snapshot, id(11)).unwrap();

                assert_eq!(
                    summary.start_agent,
                    StartAgent::Disabled(
                        "Move this task to a workspace to start an agent on it.".into()
                    )
                );
            }

            #[test]
            fn should_be_disabled_in_an_archived_workspace_saying_to_unarchive_it() {
                let mut snapshot = snapshot();
                snapshot.workspaces[0].archived_at = Some(NOW.into());

                let summary = summary(&snapshot, id(10)).unwrap();

                assert_eq!(
                    summary.start_agent,
                    StartAgent::Disabled("Unarchive tabs-a11y to start an agent on it.".into())
                );
            }
        }
    }

    fn pr_info() -> PullRequest {
        PullRequest {
            checks: None,
            draft: false,
            head_ref: "feat/tabs-keyboard".into(),
            number: 142,
            repo: "bRRRITSCOLD/ts-packages".into(),
            review: None,
            state: PrState::Open,
            synced_at: NOW.into(),
            updated_at: NOW.into(),
            url: "https://github.com/bRRRITSCOLD/ts-packages/pull/142".into(),
        }
    }

    mod comment_rows {
        use super::*;

        fn agents_comment(n: u32, agent: Option<Agent>) -> Comment {
            Comment {
                author: CommentAuthor::Agent,
                author_agent: agent,
                author_pane: Some(id(30)),
                seen_at: None,
                ..comment(n, id(10))
            }
        }

        fn row(snapshot: &Snapshot, comment: Comment) -> CommentRow {
            comment_rows(snapshot, &[comment], now()).remove(0)
        }

        #[test]
        fn should_name_you_and_say_your_comment_is_waiting() {
            let row = row(&snapshot(), comment(20, id(10)));

            assert_eq!(row.author, "You");
            assert_eq!(row.body, "comment 20");
            assert_eq!(row.delivery.as_deref(), Some("Waiting for the agent"));
        }

        #[test]
        fn should_say_which_agent_your_comment_was_delivered_to() {
            let mut snapshot = snapshot();
            let ws = snapshot.workspaces[0].clone();
            snapshot.panes = vec![agent_pane(30, &ws, Agent::Claude, 10)];
            let delivered = Comment {
                delivered_at: Some(NOW.into()),
                delivered_to_pane: Some(id::<PaneId>(30)),
                ..comment(20, id(10))
            };

            assert_eq!(
                row(&snapshot, delivered).delivery.as_deref(),
                Some("Delivered to Claude Code")
            );
        }

        #[test]
        fn should_say_delivered_to_the_agent_once_its_pane_is_gone() {
            let delivered = Comment {
                delivered_at: Some(NOW.into()),
                delivered_to_pane: Some(id::<PaneId>(30)),
                ..comment(20, id(10))
            };

            assert_eq!(
                row(&snapshot(), delivered).delivery.as_deref(),
                Some("Delivered to the agent")
            );
        }

        #[test]
        fn should_name_the_agent_with_its_glyph_and_no_delivery_line() {
            let row = row(&snapshot(), agents_comment(21, Some(Agent::Claude)));

            assert_eq!(row.author, "✳ Claude Code");
            assert_eq!(row.delivery, None);
        }

        #[test]
        fn should_name_a_shell_panes_comment_shell() {
            assert_eq!(row(&snapshot(), agents_comment(21, None)).author, "Shell");
        }

        #[test]
        fn should_say_how_long_ago_it_was_written() {
            let written = Comment {
                created_at: "2026-11-13T10:00:00Z".into(),
                ..comment(20, id(10))
            };

            assert_eq!(row(&snapshot(), written).ago, "2 h ago");
        }
    }

    mod view {
        use std::rc::Rc;

        use agentdeck_client::ws::{Event, Remote};
        use brrritscold_core_gpui::{Root, Theme, ThemePreference};
        use gpui::{Modifiers, TestAppContext, VisualTestContext};

        use super::*;
        use crate::api::fake::FakeApi;
        use crate::connection::fake::FakeConnector;
        use crate::connection::{Connector, Status};

        struct Harness {
            api: Arc<FakeApi>,
            remote: Remote,
            _connection: Entity<Connection>,
            sheet: Entity<TaskSheet>,
        }

        impl Harness {
            /// A window holding the sheet over a daemon with [`snapshot`]'s task 10 and
            /// `comments`, closed.
            fn new(
                comments: Vec<Comment>,
                cx: &mut TestAppContext,
            ) -> (Harness, &mut VisualTestContext) {
                cx.update(brrritscold_core_gpui::init);
                cx.update(crate::keymap::init);
                let api = Arc::new(FakeApi::with(snapshot()));
                api.set_comments(comments);
                let connector = Rc::new(FakeConnector::default());
                let remote = connector.then_connect_to(api.clone());
                let connector: Rc<dyn Connector> = connector;
                let connection = cx.update(|cx| cx.new(|cx| Connection::new(connector, cx)));
                cx.run_until_parked();
                let Status::Connected(session) =
                    connection.read_with(cx, |c, _| c.status().clone())
                else {
                    panic!("the fake connects");
                };
                let mut sheet = None;
                let window = cx.add_window(|window, cx| {
                    let view = cx.new(|cx| TaskSheet::new(session.api, &connection, window, cx));
                    sheet = Some(view.clone());
                    Root::new(view, window, cx)
                });
                let cx = VisualTestContext::from_window(*window, cx).into_mut();
                let harness = Harness {
                    api,
                    remote,
                    _connection: connection,
                    sheet: sheet.unwrap(),
                };
                harness.settle(cx);
                (harness, cx)
            }

            /// [`Harness::new`], opened on task 10.
            fn open(
                comments: Vec<Comment>,
                cx: &mut TestAppContext,
            ) -> (Harness, &mut VisualTestContext) {
                let (harness, cx) = Harness::new(comments, cx);
                harness.open_on(10, cx);
                (harness, cx)
            }

            fn open_on(&self, task: u32, cx: &mut VisualTestContext) {
                cx.update(|window, cx| {
                    self.sheet
                        .update(cx, |s, cx| s.open(id(task), snapshot(), window, cx));
                });
                self.settle(cx);
            }

            fn settle(&self, cx: &mut VisualTestContext) {
                cx.run_until_parked();
                cx.update(|window, cx| window.draw(cx).clear(cx));
                cx.run_until_parked();
                cx.update(|window, cx| window.draw(cx).clear(cx));
            }

            fn changed_comment(&self, cx: &mut VisualTestContext) {
                self.remote
                    .events
                    .try_send(Event::Message(ServerMessage::Changed {
                        entity: Changed::Comment,
                        id: "x".into(),
                    }))
                    .unwrap();
                self.settle(cx);
            }

            fn bodies(&self, cx: &mut VisualTestContext) -> Vec<String> {
                self.sheet.read_with(cx, |s, _| {
                    s.thread().into_iter().map(|r| r.body.to_string()).collect()
                })
            }

            fn write(&self, text: &str, cx: &mut VisualTestContext) {
                cx.update(|window, cx| {
                    let composer = self.sheet.read(cx).composer.clone();
                    composer.update(cx, |composer, cx| {
                        composer.set_value(text.to_owned(), window, cx);
                        composer.focus(window, cx);
                    });
                });
                self.settle(cx);
            }

            fn written(&self, cx: &mut VisualTestContext) -> String {
                self.sheet
                    .read_with(cx, |s, cx| s.composer.read(cx).value().to_string())
            }

            fn added(&self) -> Vec<(TaskId, String)> {
                self.api.comments_added.lock().unwrap().clone()
            }
        }

        #[gpui::test]
        fn should_open_on_the_task_with_its_thread_oldest_first(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(
                vec![
                    comment(20, id(10)),
                    comment(21, id(10)),
                    comment(22, id(11)),
                ],
                cx,
            );

            assert!(harness.sheet.read_with(cx, |s, _| s.is_open()));
            assert_eq!(harness.sheet.read_with(cx, |s, _| s.task()), Some(id(10)));
            assert_eq!(harness.bodies(cx), ["comment 20", "comment 21"]);
        }

        #[gpui::test]
        fn should_mark_the_tasks_agent_comments_seen_on_opening(cx: &mut TestAppContext) {
            let (harness, _) = Harness::open(Vec::new(), cx);

            assert_eq!(*harness.api.seen.lock().unwrap(), [id::<TaskId>(10)]);
        }

        #[gpui::test]
        fn should_read_the_thread_again_on_a_changed_comment(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(vec![comment(20, id(10))], cx);
            harness
                .api
                .set_comments(vec![comment(20, id(10)), comment(21, id(10))]);

            harness.changed_comment(cx);

            assert_eq!(harness.bodies(cx), ["comment 20", "comment 21"]);
        }

        #[gpui::test]
        fn should_mark_an_agent_comment_arriving_while_open_seen(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(Vec::new(), cx);
            harness.api.set_comments(vec![Comment {
                author: CommentAuthor::Agent,
                author_agent: Some(Agent::Claude),
                seen_at: None,
                ..comment(21, id(10))
            }]);

            harness.changed_comment(cx);

            assert_eq!(harness.api.seen.lock().unwrap().len(), 2);
        }

        #[gpui::test]
        fn should_read_nothing_on_a_changed_comment_while_closed(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::new(Vec::new(), cx);

            harness.changed_comment(cx);

            assert_eq!(harness.api.comment_reads(), 0);
        }

        #[gpui::test]
        fn should_send_the_trimmed_comment_on_ctrl_enter_and_empty_the_composer(
            cx: &mut TestAppContext,
        ) {
            let (harness, cx) = Harness::open(Vec::new(), cx);
            harness.write("  Also flip the arrows\nfor right-to-left text. ", cx);

            cx.simulate_keystrokes("ctrl-enter");
            harness.settle(cx);

            assert_eq!(
                harness.added(),
                [(
                    id(10),
                    "Also flip the arrows\nfor right-to-left text.".to_owned()
                )]
            );
            assert_eq!(harness.written(cx), "");
            assert_eq!(
                harness.bodies(cx),
                ["Also flip the arrows\nfor right-to-left text."]
            );
        }

        #[gpui::test]
        fn should_insert_a_newline_on_enter(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(Vec::new(), cx);
            harness.write("line", cx);

            cx.simulate_keystrokes("enter");
            harness.settle(cx);

            assert_eq!(harness.added(), []);
            assert!(harness.written(cx).contains('\n'));
        }

        #[gpui::test]
        fn should_send_from_the_comment_button(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(Vec::new(), cx);
            harness.write("Arrow keys work?", cx);

            let button = cx.debug_bounds("send comment").expect("Comment shows");
            cx.simulate_click(button.center(), Modifiers::default());
            harness.settle(cx);

            assert_eq!(harness.added(), [(id(10), "Arrow keys work?".to_owned())]);
        }

        #[gpui::test]
        fn should_keep_comment_disabled_while_the_text_is_blank(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(Vec::new(), cx);
            assert!(!cx.update(|_, cx| harness.sheet.read(cx).can_comment(cx)));

            harness.write("  \n ", cx);
            assert!(!cx.update(|_, cx| harness.sheet.read(cx).can_comment(cx)));
            cx.simulate_keystrokes("ctrl-enter");
            harness.settle(cx);
            let button = cx.debug_bounds("send comment").expect("Comment shows");
            cx.simulate_click(button.center(), Modifiers::default());
            harness.settle(cx);

            assert_eq!(harness.added(), []);
        }

        #[gpui::test]
        fn should_keep_the_text_and_toast_when_the_comment_cant_be_sent(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(Vec::new(), cx);
            harness.write("keep me", cx);
            harness.api.fail_next("refused");

            cx.simulate_keystrokes("ctrl-enter");
            harness.settle(cx);

            assert_eq!(harness.written(cx), "keep me");
            let shown = cx.update(|_, cx| crate::notice::shown(cx));
            assert_eq!(shown.last().unwrap().title, "Couldn't send the comment");
        }

        #[gpui::test]
        fn should_drop_a_half_written_comment_when_opened_on_another_task(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(Vec::new(), cx);
            harness.write("for task 10", cx);

            harness.open_on(11, cx);

            assert_eq!(harness.written(cx), "");
        }

        #[gpui::test]
        fn should_change_the_tasks_status(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(Vec::new(), cx);

            cx.update(|window, cx| {
                harness
                    .sheet
                    .update(cx, |s, cx| s.set_status(TaskStatus::Review, window, cx));
            });
            harness.settle(cx);

            let task = harness.api.snapshot_now().tasks[0].clone();
            assert_eq!(task.status, TaskStatus::Review);
        }

        #[gpui::test]
        fn should_close_on_escape(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(Vec::new(), cx);

            cx.simulate_keystrokes("escape");
            harness.settle(cx);

            assert!(!harness.sheet.read_with(cx, |s, _| s.is_open()));
        }

        #[gpui::test]
        fn should_draw_in_both_themes(cx: &mut TestAppContext) {
            let delivered = Comment {
                delivered_at: Some(NOW.into()),
                ..comment(20, id(10))
            };
            let agents = Comment {
                author: CommentAuthor::Agent,
                author_agent: Some(Agent::Claude),
                ..comment(21, id(10))
            };
            let (harness, cx) = Harness::open(vec![delivered, agents, comment(22, id(10))], cx);

            for preference in [ThemePreference::Light, ThemePreference::Dark] {
                cx.update(|_, cx| Theme::set_preference(preference, cx));
                harness.settle(cx);
            }
        }

        fn toasts(cx: &mut VisualTestContext) -> Vec<String> {
            cx.update(|_, cx| crate::notice::shown(cx))
                .into_iter()
                .map(|notice| notice.title.to_string())
                .collect()
        }

        fn editing(harness: &Harness, cx: &mut VisualTestContext) -> Option<Editing> {
            harness.sheet.read_with(cx, |s, _| s.editing())
        }

        fn edits(harness: &Harness) -> Vec<(TaskId, TaskPatch)> {
            harness.api.edited_tasks.lock().unwrap().clone()
        }

        fn click(selector: &'static str, harness: &Harness, cx: &mut VisualTestContext) {
            let bounds = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} shows"));
            cx.simulate_click(bounds.center(), Modifiers::default());
            harness.settle(cx);
        }

        mod sheet_menu {
            use super::*;

            /// [`snapshot`] with Claude Code idle in `tabs-a11y`'s `agents` tab, on no task.
            fn with_agent() -> Snapshot {
                let mut snapshot = snapshot();
                snapshot.panes.push(PaneState {
                    agent: Some(Agent::Claude),
                    tab: "agents".into(),
                    ..pane(30, id(1), AgentState::Idle)
                });
                snapshot
            }

            /// The sheet open on task 10 of `snapshot`.
            fn open_with(
                snapshot: Snapshot,
                cx: &mut TestAppContext,
            ) -> (Harness, &mut VisualTestContext) {
                let (harness, cx) = Harness::new(Vec::new(), cx);
                cx.update(|window, cx| {
                    harness
                        .sheet
                        .update(cx, |s, cx| s.open(id(10), snapshot, window, cx));
                });
                harness.settle(cx);
                (harness, cx)
            }

            /// Opens the sheet's `…` menu and types `keys`.
            fn pick(harness: &Harness, keys: &str, cx: &mut VisualTestContext) {
                click("task sheet menu", harness, cx);
                cx.simulate_keystrokes(keys);
                harness.settle(cx);
            }

            #[gpui::test]
            fn should_send_the_task_to_the_agent_picked(cx: &mut TestAppContext) {
                let (harness, cx) = open_with(with_agent(), cx);

                // Send to agent, its submenu, its first pane.
                pick(&harness, "down enter", cx);
                cx.simulate_keystrokes("enter");
                harness.settle(cx);

                assert_eq!(
                    *harness.api.linked.lock().unwrap(),
                    [(id(30), Some(id(10)))]
                );
            }

            #[gpui::test]
            fn should_copy_the_task_id_after_send_to_agent(cx: &mut TestAppContext) {
                let (harness, cx) = open_with(with_agent(), cx);

                pick(&harness, "down down enter", cx);

                let copied = cx.read_from_clipboard().and_then(|item| item.text());
                assert_eq!(copied, Some(id::<TaskId>(10).to_string()));
            }

            #[gpui::test]
            fn should_offer_only_copy_task_id_on_a_done_task(cx: &mut TestAppContext) {
                let mut done = with_agent();
                done.tasks[0].status = TaskStatus::Done;
                let (harness, cx) = open_with(done, cx);

                pick(&harness, "down enter", cx);

                let copied = cx.read_from_clipboard().and_then(|item| item.text());
                assert_eq!(copied, Some(id::<TaskId>(10).to_string()));
                assert_eq!(*harness.api.linked.lock().unwrap(), []);
            }

            #[gpui::test]
            fn should_sit_left_of_the_close_button_with_the_title_clear_of_it(
                cx: &mut TestAppContext,
            ) {
                let (_harness, cx) = open_with(with_agent(), cx);

                let menu = cx.debug_bounds("task sheet menu").expect("the menu shows");
                let title = cx
                    .debug_bounds("task sheet title")
                    .expect("the title shows");
                let sheet = (cx.debug_bounds("task sheet header")).expect("the header shows");
                assert_eq!(menu.size, gpui::size(px(32.), px(32.)));
                // `top-4`, `right-12`: just left of the close button's `right-4`.
                assert_eq!(menu.top() - sheet.top(), px(16.));
                assert_eq!(sheet.right() - menu.right(), px(48.));
                assert!(title.right() <= menu.left(), "{title:?} clear of {menu:?}");
            }
        }

        mod edit_title {
            use super::*;

            /// The sheet on task 10 with its title clicked, the input holding `text`.
            fn typed<'a>(
                text: &str,
                cx: &'a mut TestAppContext,
            ) -> (Harness, &'a mut VisualTestContext) {
                let (harness, cx) = Harness::open(Vec::new(), cx);
                type_title(&harness, text, cx);
                (harness, cx)
            }

            /// Clicks the title and types `text` over it.
            fn type_title(harness: &Harness, text: &str, cx: &mut VisualTestContext) {
                click("task sheet title", harness, cx);
                cx.update(|window, cx| {
                    let Some(Editor::Title(input)) = &harness.sheet.read(cx).editor else {
                        panic!("the title is being edited");
                    };
                    let input = input.clone();
                    input.update(cx, |input, cx| input.set_value(text.to_owned(), window, cx));
                });
                harness.settle(cx);
            }

            fn title(patch: &str) -> TaskPatch {
                TaskPatch {
                    title: Some(patch.into()),
                    ..TaskPatch::default()
                }
            }

            #[gpui::test]
            fn should_turn_the_title_into_an_input_on_a_click(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(Vec::new(), cx);

                click("task sheet title", &harness, cx);

                assert_eq!(editing(&harness, cx), Some(Editing::Title));
                assert!(cx.debug_bounds("task sheet title editor").is_some());
            }

            #[gpui::test]
            fn should_save_the_trimmed_title_on_enter(cx: &mut TestAppContext) {
                let (harness, cx) = typed("  Tabs: Home and End keys ", cx);

                cx.simulate_keystrokes("enter");
                harness.settle(cx);

                assert_eq!(
                    edits(&harness),
                    [(id(10), title("Tabs: Home and End keys"))]
                );
                assert_eq!(editing(&harness, cx), None);
            }

            #[gpui::test]
            fn should_save_it_when_the_input_loses_focus(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(Vec::new(), cx);
                // Focus events reach only an active window's views.
                cx.update(|window, _| window.activate_window());
                type_title(&harness, "Tabs: Home and End keys", cx);

                cx.update(|window, cx| window.blur(cx));
                harness.settle(cx);

                assert_eq!(
                    edits(&harness),
                    [(id(10), title("Tabs: Home and End keys"))]
                );
            }

            #[gpui::test]
            fn should_drop_the_edit_on_escape_and_keep_the_sheet_open(cx: &mut TestAppContext) {
                let (harness, cx) = typed("Something else", cx);

                cx.simulate_keystrokes("escape");
                harness.settle(cx);

                assert_eq!(editing(&harness, cx), None);
                assert!(edits(&harness).is_empty());
                assert!(harness.sheet.read_with(cx, |s, _| s.is_open()));
            }

            #[gpui::test]
            fn should_show_the_fields_error_for_a_blank_title_and_keep_editing(
                cx: &mut TestAppContext,
            ) {
                let (harness, cx) = typed("   ", cx);

                cx.simulate_keystrokes("enter");
                harness.settle(cx);

                assert!(edits(&harness).is_empty());
                assert_eq!(editing(&harness, cx), Some(Editing::Title));
                assert!(harness.sheet.read_with(cx, |s, cx| s.shows_blank_title(cx)));
                assert!(cx.debug_bounds("task sheet title editor").is_some());
            }

            #[gpui::test]
            fn should_close_without_saving_an_unchanged_title(cx: &mut TestAppContext) {
                let (harness, cx) = typed("Tabs: arrow-key navigation", cx);

                cx.simulate_keystrokes("enter");
                harness.settle(cx);

                assert!(edits(&harness).is_empty());
                assert_eq!(editing(&harness, cx), None);
            }

            #[gpui::test]
            fn should_keep_editing_and_toast_when_it_cant_be_saved(cx: &mut TestAppContext) {
                let (harness, cx) = typed("Tabs: Home and End keys", cx);
                harness.api.fail_next("refused");

                cx.simulate_keystrokes("enter");
                harness.settle(cx);

                assert_eq!(editing(&harness, cx), Some(Editing::Title));
                assert_eq!(
                    toasts(cx).last().map(String::as_str),
                    Some("Couldn't save Tabs: arrow-key navigation")
                );
            }

            #[gpui::test]
            fn should_not_edit_a_pr_tasks_title(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::new(Vec::new(), cx);
                let mut snapshot = snapshot();
                snapshot.tasks.push(Task {
                    pr: Some(pr_info()),
                    ..task(11, TaskKind::Pr, None)
                });
                cx.update(|window, cx| {
                    harness
                        .sheet
                        .update(cx, |s, cx| s.open(id(11), snapshot, window, cx));
                });
                harness.settle(cx);

                click("task sheet title", &harness, cx);

                assert_eq!(editing(&harness, cx), None);
            }
        }

        mod edit_description {
            use super::*;

            /// The sheet on task 10 with its description clicked, the textarea holding `text`.
            fn typed<'a>(
                text: &str,
                cx: &'a mut TestAppContext,
            ) -> (Harness, &'a mut VisualTestContext) {
                let (harness, cx) = Harness::open(Vec::new(), cx);
                click("task sheet description", &harness, cx);
                cx.update(|window, cx| {
                    let Some(Editor::Description(textarea)) = &harness.sheet.read(cx).editor else {
                        panic!("the description is being edited");
                    };
                    let textarea = textarea.clone();
                    textarea.update(cx, |textarea, cx| {
                        textarea.set_value(text.to_owned(), window, cx);
                    });
                });
                harness.settle(cx);
                (harness, cx)
            }

            fn body(patch: &str) -> TaskPatch {
                TaskPatch {
                    body: Some(patch.into()),
                    ..TaskPatch::default()
                }
            }

            #[gpui::test]
            fn should_turn_the_description_into_a_textarea_holding_it(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(Vec::new(), cx);

                click("task sheet description", &harness, cx);

                let held = harness.sheet.read_with(cx, |s, cx| match &s.editor {
                    Some(Editor::Description(textarea)) => textarea.read(cx).value().to_string(),
                    _ => panic!("the description is being edited"),
                });
                assert_eq!(held, "Add Home/End and arrow-key navigation.");
            }

            #[gpui::test]
            fn should_save_it_on_ctrl_enter(cx: &mut TestAppContext) {
                let (harness, cx) = typed("Home/End first.\nThen PageUp. ", cx);

                cx.simulate_keystrokes("ctrl-enter");
                harness.settle(cx);

                assert_eq!(
                    edits(&harness),
                    [(id(10), body("Home/End first.\nThen PageUp."))]
                );
                assert_eq!(editing(&harness, cx), None);
            }

            #[gpui::test]
            fn should_insert_a_newline_on_enter(cx: &mut TestAppContext) {
                let (harness, cx) = typed("a", cx);

                cx.simulate_keystrokes("enter");
                harness.settle(cx);

                assert!(edits(&harness).is_empty());
                assert_eq!(editing(&harness, cx), Some(Editing::Description));
            }

            #[gpui::test]
            fn should_save_it_from_the_save_button(cx: &mut TestAppContext) {
                let (harness, cx) = typed("Home/End first.", cx);

                click("save description", &harness, cx);

                assert_eq!(edits(&harness), [(id(10), body("Home/End first."))]);
            }

            #[gpui::test]
            fn should_clear_it_when_saved_empty(cx: &mut TestAppContext) {
                let (harness, cx) = typed("  ", cx);

                cx.simulate_keystrokes("ctrl-enter");
                harness.settle(cx);

                assert_eq!(edits(&harness), [(id(10), body(""))]);
            }

            #[gpui::test]
            fn should_drop_the_edit_on_escape_or_cancel(cx: &mut TestAppContext) {
                let (harness, cx) = typed("never mind", cx);
                cx.simulate_keystrokes("escape");
                harness.settle(cx);
                assert_eq!(editing(&harness, cx), None);
                assert!(harness.sheet.read_with(cx, |s, _| s.is_open()));

                click("task sheet description", &harness, cx);
                click("cancel description", &harness, cx);

                assert_eq!(editing(&harness, cx), None);
                assert!(edits(&harness).is_empty());
            }

            #[gpui::test]
            fn should_offer_to_add_one_to_a_task_with_none(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::new(Vec::new(), cx);
                let mut snapshot = snapshot();
                snapshot.tasks[0].body = String::new();
                cx.update(|window, cx| {
                    harness
                        .sheet
                        .update(cx, |s, cx| s.open(id(10), snapshot, window, cx));
                });
                harness.settle(cx);

                click("task sheet description", &harness, cx);

                assert_eq!(editing(&harness, cx), Some(Editing::Description));
            }
        }

        mod move_to {
            use super::*;

            fn moved(place: TaskPlace) -> TaskPatch {
                TaskPatch {
                    workspace_id: Some(place),
                    ..TaskPatch::default()
                }
            }

            #[gpui::test]
            fn should_move_the_task_to_the_workspace_picked(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(Vec::new(), cx);

                cx.update(|window, cx| {
                    harness.sheet.update(cx, |s, cx| {
                        s.move_to(TaskPlace::Workspace(id(3)), window, cx);
                    });
                });
                harness.settle(cx);

                assert_eq!(
                    edits(&harness),
                    [(id(10), moved(TaskPlace::Workspace(id(3))))]
                );
            }

            #[gpui::test]
            fn should_move_it_to_the_inbox(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(Vec::new(), cx);

                cx.update(|window, cx| {
                    harness
                        .sheet
                        .update(cx, |s, cx| s.move_to(TaskPlace::Inbox, window, cx));
                });
                harness.settle(cx);

                assert_eq!(edits(&harness), [(id(10), moved(TaskPlace::Inbox))]);
                assert_eq!(harness.api.snapshot_now().tasks[0].workspace_id, None);
            }

            #[gpui::test]
            fn should_do_nothing_when_picking_where_it_is(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(Vec::new(), cx);

                cx.update(|window, cx| {
                    harness.sheet.update(cx, |s, cx| {
                        s.move_to(TaskPlace::Workspace(id(1)), window, cx);
                    });
                });
                harness.settle(cx);

                assert!(edits(&harness).is_empty());
            }

            #[gpui::test]
            fn should_toast_when_it_cant_move(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::open(Vec::new(), cx);
                harness
                    .api
                    .refuse_next("unknown_workspace", "workspace x is archived");

                cx.update(|window, cx| {
                    harness
                        .sheet
                        .update(cx, |s, cx| s.move_to(TaskPlace::Inbox, window, cx));
                });
                harness.settle(cx);

                assert_eq!(
                    toasts(cx).last().map(String::as_str),
                    Some("Couldn't save Tabs: arrow-key navigation")
                );
            }
        }

        mod open_editing {
            use super::*;

            #[gpui::test]
            fn should_edit_a_plain_tasks_title(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::new(Vec::new(), cx);

                cx.update(|window, cx| {
                    harness
                        .sheet
                        .update(cx, |s, cx| s.open_editing(id(10), snapshot(), window, cx));
                });
                harness.settle(cx);

                assert!(harness.sheet.read_with(cx, |s, _| s.is_open()));
                assert_eq!(editing(&harness, cx), Some(Editing::Title));
            }

            #[gpui::test]
            fn should_edit_a_pr_tasks_description(cx: &mut TestAppContext) {
                let (harness, cx) = Harness::new(Vec::new(), cx);
                let mut snapshot = snapshot();
                snapshot.tasks.push(Task {
                    pr: Some(pr_info()),
                    ..task(11, TaskKind::Pr, None)
                });

                cx.update(|window, cx| {
                    harness
                        .sheet
                        .update(cx, |s, cx| s.open_editing(id(11), snapshot, window, cx));
                });
                harness.settle(cx);

                assert_eq!(editing(&harness, cx), Some(Editing::Description));
            }
        }

        mod start_agent {
            use std::cell::RefCell;

            use super::*;

            /// The sheet opened on `task` as `snapshot` holds it, and what it asked the
            /// window for.
            fn open_over(
                snapshot: Snapshot,
                task: u32,
                cx: &mut TestAppContext,
            ) -> (
                Harness,
                Rc<RefCell<Vec<StartAgentOn>>>,
                Subscription,
                &mut VisualTestContext,
            ) {
                let (harness, cx) = Harness::new(Vec::new(), cx);
                let asked = Rc::new(RefCell::new(Vec::new()));
                let seen = asked.clone();
                let subscription = cx.update(|_, cx| {
                    cx.subscribe(&harness.sheet, move |_, ask: &StartAgentOn, _| {
                        seen.borrow_mut().push(*ask);
                    })
                });
                cx.update(|window, cx| {
                    harness
                        .sheet
                        .update(cx, |s, cx| s.open(id(task), snapshot, window, cx));
                });
                harness.settle(cx);
                (harness, asked, subscription, cx)
            }

            fn click(harness: &Harness, cx: &mut VisualTestContext) {
                let button = cx.debug_bounds("start agent").expect("Start agent shows");
                cx.simulate_click(button.center(), Modifiers::default());
                harness.settle(cx);
            }

            #[gpui::test]
            fn should_show_with_no_agent_on_the_task(cx: &mut TestAppContext) {
                let (_harness, _, _s, cx) = open_over(snapshot(), 10, cx);

                assert!(cx.debug_bounds("start agent").is_some());
            }

            #[gpui::test]
            fn should_hide_while_an_agent_works_on_the_task(cx: &mut TestAppContext) {
                let mut snapshot = snapshot();
                let ws = snapshot.workspaces[0].clone();
                snapshot.panes = vec![agent_pane(30, &ws, Agent::Claude, 10)];

                let (_harness, _, _s, cx) = open_over(snapshot, 10, cx);

                assert!(cx.debug_bounds("start agent").is_none());
            }

            #[gpui::test]
            fn should_ask_for_new_agent_on_the_tasks_workspace_and_close(cx: &mut TestAppContext) {
                let (harness, asked, _s, cx) = open_over(snapshot(), 10, cx);

                click(&harness, cx);

                assert_eq!(
                    *asked.borrow(),
                    [StartAgentOn {
                        workspace: id(1),
                        task: id(10),
                    }]
                );
                assert!(!harness.sheet.read_with(cx, |s, _| s.is_open()));
            }

            #[gpui::test]
            fn should_do_nothing_while_disabled_for_an_inbox_task(cx: &mut TestAppContext) {
                let snapshot = Snapshot {
                    tasks: vec![task(11, TaskKind::Plain, None)],
                    ..snapshot()
                };
                let (harness, asked, _s, cx) = open_over(snapshot, 11, cx);

                click(&harness, cx);

                assert_eq!(*asked.borrow(), []);
                assert!(harness.sheet.read_with(cx, |s, _| s.is_open()));
            }

            #[gpui::test]
            fn should_do_nothing_while_disabled_in_an_archived_workspace(cx: &mut TestAppContext) {
                let mut snapshot = snapshot();
                snapshot.workspaces[0].archived_at = Some(NOW.into());
                let (harness, asked, _s, cx) = open_over(snapshot, 10, cx);

                click(&harness, cx);

                assert_eq!(*asked.borrow(), []);
                assert!(harness.sheet.read_with(cx, |s, _| s.is_open()));
            }
        }

        mod pr_link {
            use super::*;
            use crate::overview::fixtures::pr_task;
            use crate::terminal::open::fake::{self, FakeSpawn};

            /// The sheet opened on task 10 as a PR whose stored `url` is `url`, with a fake
            /// opener.
            fn open_with<'a>(
                url: &str,
                cx: &'a mut TestAppContext,
            ) -> (Harness, Arc<FakeSpawn>, &'a mut VisualTestContext) {
                let (harness, cx) = Harness::new(Vec::new(), cx);
                let opener = cx.update(|_, cx| fake::install(cx));
                let mut snapshot = snapshot();
                let ws = snapshot.workspaces[0].id;
                let mut pr_task = pr_task(10, "acme/widgets", PrState::Open, Some(ws));
                if let Some(pr) = pr_task.pr.as_mut() {
                    pr.url = url.into();
                }
                snapshot.tasks = vec![pr_task];
                cx.update(|window, cx| {
                    (harness.sheet).update(cx, |s, cx| s.open(id(10), snapshot, window, cx));
                });
                harness.settle(cx);
                (harness, opener, cx)
            }

            fn click(harness: &Harness, cx: &mut VisualTestContext) {
                let link = cx
                    .debug_bounds("task sheet pr link")
                    .expect("the PR link shows");
                cx.simulate_click(link.center(), Modifiers::default());
                harness.settle(cx);
            }

            #[gpui::test]
            fn should_open_the_prs_stored_url(cx: &mut TestAppContext) {
                let url = "https://github.com/acme/widgets/pull/10";
                let (harness, opener, cx) = open_with(url, cx);

                click(&harness, cx);

                assert_eq!(
                    *opener.started.lock().unwrap(),
                    [["xdg-open".to_owned(), url.to_owned()]]
                );
            }

            #[gpui::test]
            fn should_open_nothing_for_a_url_off_github(cx: &mut TestAppContext) {
                let (harness, opener, cx) = open_with("https://evil.io/acme/widgets/pull/10", cx);

                click(&harness, cx);

                assert!(opener.started.lock().unwrap().is_empty());
            }

            #[gpui::test]
            fn should_toast_when_the_handler_cant_start(cx: &mut TestAppContext) {
                let (harness, opener, cx) =
                    open_with("https://github.com/acme/widgets/pull/10", cx);
                *opener.failure.lock().unwrap() = Some("No such file or directory".into());

                click(&harness, cx);

                let shown = cx.update(|_, cx| crate::notice::shown(cx));
                assert_eq!(
                    shown.last().map(|n| n.title.as_ref()),
                    Some("Couldn't open the PR")
                );
            }

            #[gpui::test]
            fn should_show_no_link_for_a_plain_task(cx: &mut TestAppContext) {
                let (_harness, cx) = Harness::open(Vec::new(), cx);

                assert!(cx.debug_bounds("task sheet pr link").is_none());
            }
        }

        mod spin {
            use super::*;
            use crate::overview::fixtures::pr_task;
            use crate::state_dot::tests::{activate, redraws_in_one_second};

            /// The sheet opened on task 10 as a PR whose checks are running.
            fn running_checks(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
                let (harness, cx) = Harness::new(Vec::new(), cx);
                let mut snapshot = snapshot();
                let ws = snapshot.workspaces[0].id;
                let mut pr_task = pr_task(10, "acme/widgets", PrState::Open, Some(ws));
                if let Some(pr) = pr_task.pr.as_mut() {
                    pr.checks = Some(ChecksState::Pending);
                }
                snapshot.tasks = vec![pr_task];
                cx.update(|window, cx| {
                    (harness.sheet).update(cx, |s, cx| s.open(id(10), snapshot, window, cx));
                });
                harness.settle(cx);
                activate(cx);
                (harness, cx)
            }

            #[gpui::test]
            fn should_hold_running_checks_still_while_the_window_is_active(
                cx: &mut TestAppContext,
            ) {
                let (harness, cx) = running_checks(cx);
                assert_eq!(redraws_in_one_second(&harness.sheet, cx), 0);
            }
        }
    }
}
