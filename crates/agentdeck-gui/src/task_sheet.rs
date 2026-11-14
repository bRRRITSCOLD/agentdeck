//! The task sheet (spec §11), Penpot "Screens" → "Task sheet": a right-side `Sheet` on one task.
//! Its header names the task and who is on it; its body (Penpot `agentdeck/TaskSheetBody`)
//! holds the status picker and where the task lives, its description, its PR, and the comment
//! thread, oldest first; its footer (`agentdeck/CommentComposer`) writes a comment for the
//! task's agent. Enter is a newline there; Ctrl+Enter or **Comment** sends.
//!
//! Clicking a task's title in the Tasks view, the Inbox or the right panel asks the window
//! for it with [`OpenTask`]. Opening it reads the thread and marks the task's agent comments
//! seen, which clears the row's dot. While open, a `changed comment` reads the thread again.
//! The task itself comes from the sidebar's [`Snapshot`], passed on by the window.

use std::sync::Arc;

use agentdeck_client::types::{Comment, Task};
use agentdeck_core::ws::{Entity as Changed, ServerMessage};
use agentdeck_core::{Agent, AgentState, CommentAuthor, TaskId, TaskStatus};
use brrritscold_core_gpui::{
    ActiveTheme as _, Badge, BadgeVariant, Button, ButtonSize, DialogHandle, Select, SelectTrigger,
    Sheet, SheetContent, SheetDescription, SheetFooter, SheetHeader, SheetTitle, Textarea,
    TextareaState,
};
use brrritscold_core_gpui_tokens::primitive::{font_size, font_weight, space};
use chrono::{DateTime, Utc};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, px,
};
use gpui_base::input::InputEvent;

use crate::api::Api;
use crate::connection::{Connection, DaemonEvent};
use crate::keymap::{self, Submit};
use crate::new_agent::AGENTS;
use crate::overview::{Snapshot, ago};
use crate::pane_header;
use crate::rereads::Rereads;
use crate::task_list::{self, PrCell};
use crate::task_row;
use crate::workspace::failed;

/// A view asks the window for the task sheet on this task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenTask(pub TaskId);

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
    /// Where it lives: `in tabs-a11y`, or `in the Inbox`.
    pub place: SharedString,
    /// Its description; `None` when it has none.
    pub description: Option<SharedString>,
    /// Its PR, for a PR task the sync has filled in.
    pub pr: Option<PrCell>,
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
    let place = held
        .workspace_id
        .and_then(|ws| snapshot.workspaces.iter().find(|w| w.id == ws))
        .map_or("in the Inbox".to_owned(), |w| format!("in {}", w.name));
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
        place: place.into(),
        description: (!held.body.trim().is_empty()).then(|| held.body.clone().into()),
        pr: row.and_then(|r| r.pr),
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
fn agent_name(agent: Agent) -> &'static str {
    (AGENTS.iter())
        .find(|(a, _)| *a == agent)
        .map_or("Agent", |(_, name)| name)
}

/// The agent's name after the glyph it puts in its pane's title, as pane headers show it:
/// `✳ Claude Code`. Codex puts none.
fn agent_label(agent: Agent) -> String {
    match agent {
        Agent::Claude => format!("✳ {}", agent_name(agent)),
        Agent::Codex | Agent::Other => agent_name(agent).to_owned(),
    }
}

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
    _composer: Subscription,
    _events: Subscription,
}

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
            if let DaemonEvent::Message(ServerMessage::Changed {
                entity: Changed::Comment,
                ..
            }) = event
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
    #[cfg(test)]
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

    /// Marks `task`'s agent comments seen. A failure only leaves the dot on its row.
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
                    .child(muted(summary.place.clone())),
            )
            .when_some(summary.description.clone(), |body, description| {
                body.child(muted(description))
            })
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

/// The PR line: its reference (sm, muted), a draft badge, its checks and its review.
fn pr_line(pr: &PrCell, cx: &App) -> AnyElement {
    let s = cx.theme().semantic();
    div()
        .flex()
        .items_center()
        .gap(space::S2)
        .child(
            div()
                .text_size(font_size::SM)
                .text_color(s.muted_foreground)
                .child(pr.reference.clone()),
        )
        .when(pr.draft, |line| {
            line.child(Badge::new().variant(BadgeVariant::Outline).label("draft"))
        })
        .when_some(pr.checks, |line, state| {
            line.child(task_row::checks(state, false, cx))
        })
        .when_some(pr.review, |line, state| {
            line.child(task_row::review(state, cx))
        })
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
                .header(
                    SheetHeader::new()
                        .title(SheetTitle::new(summary.title.clone()))
                        .description(SheetDescription::new(summary.on_it.clone())),
                )
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
            assert_eq!(summary.place, "in tabs-a11y");
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

            assert_eq!(summary.place, "in the Inbox");
            assert_eq!(summary.description, None);
            let pr = summary.pr.expect("its PR");
            assert_eq!(pr.reference, "ts-packages #142");
            assert_eq!(pr.checks, Some(ChecksState::Success));
        }

        #[test]
        fn should_be_none_for_a_task_it_doesnt_hold() {
            assert_eq!(summary(&snapshot(), id(99)), None);
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
    }
}
