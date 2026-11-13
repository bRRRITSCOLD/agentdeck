//! One attached pane (spec §5): attaches on a channel of its own, feeds the snapshot and live
//! output into a [`Screen`], asks for the size that fits, types into the PTY, and detaches
//! when dropped.
//!
//! While its window is the active one it claims `primary`, so its size wins over other
//! clients' (spec §5): when it goes live, and again whenever the window is activated.
//!
//! Selection, after GNOME Terminal: drag to select cells, double-click for a word,
//! triple-click for a line, shift-click to extend. `secondary-shift-c` copies it and
//! `secondary-shift-v` pastes the clipboard. On Linux a finished selection is also the primary
//! selection, which a middle click pastes. The selection lives in this client's model only;
//! the pane never hears of it.
//!
//! The mouse, after xterm: a program that asks for mouse reports (DECSET 1000 / 1002 / 1003)
//! gets the buttons, drags, moves and wheel instead, and shift takes them back for selecting
//! and the scrollback. Otherwise the wheel scrolls the scrollback, or on the alternate screen
//! sends an arrow key per line (DECSET 1007).

use std::ops::Range;

use agentdeck_client::ws;
use agentdeck_core::PaneId;
use agentdeck_core::ws::{Channel, ClientMessage, ServerMessage};
use brrritscold_core_gpui::ActiveTheme as _;
use brrritscold_core_gpui_tokens::primitive::space;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, Entity, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyDownEvent,
    Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _, Pixels, Point,
    Render, ScrollEvent, SharedString, Styled as _, Subscription, UTF16Selection, Window, canvas,
    div,
};

use super::element::{Layout, Metrics, Pointer, terminal};
use super::input::{self, Button, Mouse};
use super::palette::Palette;
use super::screen::{Frame, GridSize, Screen, SelectionKind};
use crate::connection::{Connection, DaemonEvent};
use crate::keymap::{self, Copy, Paste};

/// Where the attachment stands.
enum Attachment {
    /// Sent `attach`; waiting for `attached`.
    Attaching,
    /// Got `attached`; the next output is the snapshot.
    AwaitingSnapshot(Screen),
    /// Showing the pane.
    Live(Screen),
    /// The daemon refused it; why.
    Refused(SharedString),
}

/// What a held button that went down here is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    /// The left button, selecting.
    Selecting,
    /// A button the program hears of.
    Reporting(Button),
}

/// What a terminal tells whoever shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum TerminalEvent {
    /// The program set the pane's title; also sent on attach with the title it had.
    Title(SharedString),
}

/// A pane's terminal.
pub struct TerminalView {
    pane: PaneId,
    channel: Channel,
    sender: ws::Sender,
    attachment: Attachment,
    /// Where the grid was last laid out, and the grid that fits.
    layout: Option<Layout>,
    /// The button pressed here and not yet let go.
    held: Option<Held>,
    /// The cell the mouse was last reported at, so a move is reported once per cell.
    reported: Option<(usize, usize)>,
    /// What an input method is composing, shown at the cursor until it commits.
    marked: Option<String>,
    focus: FocusHandle,
    _subscriptions: [Subscription; 2],
}

impl TerminalView {
    /// Attaches `pane` over `connection`'s socket, through `sender`.
    pub fn new(
        pane: PaneId,
        connection: &Entity<Connection>,
        sender: ws::Sender,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let channel = connection.update(cx, |connection, _| connection.open_channel());
        let events = cx.subscribe_in(connection, window, |this, _, event, window, cx| {
            this.on_event(event, window, cx);
        });
        let activation =
            cx.observe_window_activation(window, |this, window, _| this.claim_primary(window));
        let focus = cx.focus_handle();
        let view = TerminalView {
            pane,
            channel,
            sender,
            attachment: Attachment::Attaching,
            layout: None,
            held: None,
            reported: None,
            marked: None,
            focus,
            _subscriptions: [events, activation],
        };
        view.attach();
        view
    }

    fn attach(&self) {
        let _ = self.sender.send(ClientMessage::Attach {
            pane: self.pane,
            channel: self.channel,
        });
    }

    /// Makes this the pane's primary client, if it is live and its window is the active one.
    fn claim_primary(&self, window: &Window) {
        if matches!(self.attachment, Attachment::Live(_)) && window.is_window_active() {
            let _ = self.sender.send(ClientMessage::Primary {
                channel: self.channel,
            });
        }
    }

    fn on_event(&mut self, event: &DaemonEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ours = self.channel;
        match event {
            DaemonEvent::Message(ServerMessage::Attached {
                channel,
                cols,
                rows,
                title,
                ..
            }) if *channel == ours => {
                let size = GridSize {
                    cols: *cols,
                    rows: *rows,
                };
                self.attachment = Attachment::AwaitingSnapshot(Screen::new(size));
                cx.emit(TerminalEvent::Title(title.clone().into()));
            }
            DaemonEvent::Message(ServerMessage::Title { channel, title }) if *channel == ours => {
                cx.emit(TerminalEvent::Title(title.clone().into()));
                return;
            }
            DaemonEvent::Output(channel, bytes) if *channel == ours => {
                match std::mem::replace(&mut self.attachment, Attachment::Attaching) {
                    // The snapshot was written at the daemon's size; only once it is in may
                    // the pane be resized to fit.
                    Attachment::AwaitingSnapshot(mut screen) => {
                        screen.feed(bytes);
                        self.attachment = Attachment::Live(screen);
                        self.fit();
                        self.claim_primary(window);
                    }
                    Attachment::Live(mut screen) => {
                        screen.feed(bytes);
                        self.attachment = Attachment::Live(screen);
                    }
                    other => self.attachment = other,
                }
            }
            DaemonEvent::Message(ServerMessage::Resize {
                channel,
                cols,
                rows,
            }) if *channel == ours => {
                if let Attachment::Live(screen) = &mut self.attachment {
                    screen.resize(GridSize {
                        cols: *cols,
                        rows: *rows,
                    });
                }
            }
            DaemonEvent::Message(ServerMessage::Lagged { channel }) if *channel == ours => {
                self.attachment = Attachment::Attaching;
                self.attach();
            }
            DaemonEvent::Message(ServerMessage::Error {
                channel: Some(channel),
                message,
                ..
            }) if *channel == ours => {
                self.attachment = Attachment::Refused(message.clone().into());
            }
            _ => return,
        }
        cx.notify();
    }

    /// Records where the grid sits, and resizes the pane to the grid that fits once live.
    fn set_layout(&mut self, layout: Layout, cx: &mut Context<Self>) {
        let regrid = self.layout.map(|old| old.grid) != Some(layout.grid);
        self.layout = Some(layout);
        if regrid && self.fit() {
            cx.notify();
        }
    }

    /// Resizes a live pane to the grid that fits; whether its size changed.
    fn fit(&mut self) -> bool {
        let (Attachment::Live(screen), Some(Layout { grid, .. })) =
            (&mut self.attachment, self.layout)
        else {
            return false;
        };
        if screen.size() == grid {
            return false;
        }
        screen.resize(grid);
        let _ = self.sender.send(ClientMessage::Resize {
            channel: self.channel,
            cols: grid.cols,
            rows: grid.rows,
        });
        true
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Attachment::Live(screen) = &mut self.attachment else {
            return;
        };
        let Some(bytes) = input::bytes(&event.keystroke, screen.app_cursor()) else {
            return;
        };
        screen.scroll_to_bottom();
        let _ = self.sender.input(self.channel, &bytes);
        cx.stop_propagation();
        cx.notify();
    }

    fn on_scroll(&mut self, event: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        let line = Metrics::measure(window).cell.height;
        let delta = event.delta.pixel_delta(line);
        // GPUI's Linux backends turn a shifted wheel sideways; it still means up or down here.
        let pixels = if event.modifiers.shift && delta.y == Pixels::ZERO {
            delta.x
        } else {
            delta.y
        };
        let lines = (pixels / line).round() as i32;
        if lines == 0 {
            return;
        }
        let wheel = if lines > 0 {
            Mouse::WheelUp
        } else {
            Mouse::WheelDown
        };
        let count = lines.unsigned_abs() as usize;
        if self.report(wheel, event.position, &event.modifiers, count) {
            return;
        }
        let Attachment::Live(screen) = &mut self.attachment else {
            return;
        };
        if screen.alternate_scroll() && !event.modifiers.shift {
            let bytes = input::scroll_keys(lines, screen.app_cursor());
            let _ = self.sender.input(self.channel, &bytes);
        } else if !screen.alt_screen() {
            screen.scroll(lines);
            cx.notify();
        }
    }

    /// Sends `event` at `position` to the program `count` times, if it asked to hear of the
    /// mouse and shift isn't held; whether the program has the mouse. A move is sent once per
    /// cell.
    fn report(
        &mut self,
        event: Mouse,
        position: Point<Pixels>,
        modifiers: &Modifiers,
        count: usize,
    ) -> bool {
        let (Attachment::Live(screen), Some(layout)) = (&self.attachment, self.layout) else {
            return false;
        };
        let Some(protocol) = screen.mouse_protocol().filter(|_| !modifiers.shift) else {
            return false;
        };
        let at = layout.hit(position);
        let cell = (at.line, at.col);
        if matches!(event, Mouse::Move(_)) && self.reported == Some(cell) {
            return true;
        }
        if let Some(report) = input::mouse(event, at.line, at.col, modifiers, protocol) {
            self.reported = Some(cell);
            let _ = self.sender.input(self.channel, &report.repeat(count));
        }
        true
    }

    /// A press of `button` the program hears of; whether it did.
    fn report_press(&mut self, button: Button, event: &MouseDownEvent) -> bool {
        let reported = self.report(Mouse::Press(button), event.position, &event.modifiers, 1);
        if reported {
            self.held = Some(Held::Reporting(button));
        }
        reported
    }

    fn on_left_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        if self.report_press(Button::Left, event) {
            return;
        }
        let (Attachment::Live(screen), Some(layout)) = (&mut self.attachment, self.layout) else {
            return;
        };
        let at = layout.hit(event.position);
        if event.modifiers.shift && screen.has_selection() {
            screen.extend_selection(at);
        } else {
            screen.select(SelectionKind::from_clicks(event.click_count), at);
        }
        self.held = Some(Held::Selecting);
        cx.notify();
    }

    fn on_middle_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.report_press(Button::Middle, event) {
            return;
        }
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        if let Some(text) = cx.read_from_primary().and_then(|item| item.text()) {
            self.paste(&text, cx);
        }
        #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
        let _ = cx;
    }

    fn on_right_down(&mut self, event: &MouseDownEvent, _: &mut Window, _: &mut Context<Self>) {
        self.report_press(Button::Right, event);
    }

    /// A move over the terminal with no button held, which only DECSET 1003 hears of.
    fn on_hover(&mut self, event: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        if event.pressed_button.is_none() {
            self.report(Mouse::Move(None), event.position, &event.modifiers, 1);
        }
    }

    fn on_pointer(&mut self, pointer: Pointer, cx: &mut Context<Self>) {
        match (self.held, pointer) {
            (Some(Held::Reporting(button)), Pointer::Dragged(event)) => {
                self.report(
                    Mouse::Move(Some(button)),
                    event.position,
                    &event.modifiers,
                    1,
                );
            }
            (Some(Held::Reporting(button)), Pointer::Released(event))
                if heard(event.button) == Some(button) =>
            {
                self.held = None;
                self.report(Mouse::Release(button), event.position, &event.modifiers, 1);
            }
            (Some(Held::Selecting), Pointer::Dragged(event)) => {
                if let (Attachment::Live(screen), Some(layout)) =
                    (&mut self.attachment, self.layout)
                {
                    screen.extend_selection(layout.hit(event.position));
                    cx.notify();
                }
            }
            (Some(Held::Selecting), Pointer::Released(event))
                if event.button == MouseButton::Left =>
            {
                self.held = None;
                #[cfg(any(target_os = "linux", target_os = "freebsd"))]
                if let Attachment::Live(screen) = &self.attachment
                    && let Some(text) = screen.selected_text()
                {
                    cx.write_to_primary(ClipboardItem::new_string(text));
                }
            }
            _ => {}
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Attachment::Live(screen) = &self.attachment
            && let Some(text) = screen.selected_text()
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste_clipboard(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.paste(&text, cx);
        }
    }

    /// Types `text` into the pane as a paste, and shows the bottom where it lands.
    fn paste(&mut self, text: &str, cx: &mut Context<Self>) {
        let Attachment::Live(screen) = &mut self.attachment else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let bytes = input::paste(text, screen.bracketed_paste());
        screen.scroll_to_bottom();
        let _ = self.sender.input(self.channel, &bytes);
        cx.notify();
    }

    /// What a live terminal shows, with any composition at the cursor.
    fn frame(&self, palette: &Palette, focused: bool) -> Frame {
        let Attachment::Live(screen) = &self.attachment else {
            return blank();
        };
        let mut frame = screen.frame(palette, focused);
        frame.preedit = self
            .marked
            .as_deref()
            .and_then(|text| screen.preedit(text, palette));
        frame
    }

    /// Types text an input method or a dead key committed.
    fn commit(&mut self, text: &str, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
        let Attachment::Live(screen) = &mut self.attachment else {
            return;
        };
        if !text.is_empty() {
            screen.scroll_to_bottom();
            let _ = self.sender.input(self.channel, text.as_bytes());
        }
    }

    /// The composition's length in UTF-16 code units, as input methods count.
    fn marked_len(&self) -> usize {
        self.marked
            .as_deref()
            .map_or(0, |text| text.encode_utf16().count())
    }

    /// The visible text, for tests.
    #[cfg(test)]
    pub fn text(&self) -> Option<Vec<String>> {
        match &self.attachment {
            Attachment::Live(screen) => Some(screen.text()),
            _ => None,
        }
    }

    /// Why the daemon refused it, for tests.
    #[cfg(test)]
    pub fn refused(&self) -> Option<&str> {
        match &self.attachment {
            Attachment::Refused(reason) => Some(reason),
            _ => None,
        }
    }
}

impl EventEmitter<TerminalEvent> for TerminalView {}

/// Input methods and dead keys, after GNOME Terminal: a composition shows at the cursor and
/// reaches the pane only when committed. The terminal has no text to edit, so the ranges an
/// input method passes are ignored.
impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        // The caret sits at the end of any composition.
        let end = self.marked_len();
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|_| 0..self.marked_len())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit(text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = (!text.is_empty()).then(|| text.to_owned());
        cx.notify();
    }

    /// The cursor's cell, so the candidate window opens beside it.
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (Attachment::Live(screen), Some(layout)) = (&self.attachment, self.layout) else {
            return None;
        };
        let (line, col) = screen.cursor_cell()?;
        let origin = layout.origin
            + gpui::point(
                layout.cell.width * col as f32,
                layout.cell.height * line as f32,
            );
        Some(Bounds::new(origin, layout.cell))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        let _ = self.sender.send(ClientMessage::Detach {
            channel: self.channel,
        });
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = cx.theme().mode();
        let palette = Palette::new(mode);
        let focused = self.focus.is_focused(window);
        let (frame, status) = match &self.attachment {
            Attachment::Live(_) => (self.frame(&palette, focused), None),
            Attachment::Attaching | Attachment::AwaitingSnapshot(_) => (blank(), None),
            Attachment::Refused(reason) => (blank(), Some(reason.clone())),
        };
        let this = cx.entity().downgrade();
        let on_layout = {
            let this = this.clone();
            move |layout, _: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.set_layout(layout, cx));
            }
        };
        // Registered each frame while focused: input methods and dead keys commit through it.
        let input = {
            let focus = self.focus.clone();
            let view = cx.entity();
            canvas(
                |_, _, _| {},
                move |bounds, _, window, cx| {
                    window.handle_input(&focus, ElementInputHandler::new(bounds, view), cx);
                },
            )
            .absolute()
            .size_full()
        };
        let on_pointer = move |pointer, _: &mut Window, cx: &mut App| {
            let _ = this.update(cx, |this, cx| this.on_pointer(pointer, cx));
        };
        let muted = mode.semantic().muted_foreground;
        // Penpot `agentdeck / TerminalSurface`: `background`, padding `space.3`.
        div()
            .id(("terminal", self.channel))
            .track_focus(&self.focus)
            .key_context(keymap::TERMINAL)
            .relative()
            .size_full()
            .p(space::S3)
            .bg(palette.background)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_left_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_middle_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_down))
            .on_mouse_move(cx.listener(Self::on_hover))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste_clipboard))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll(cx.listener(Self::on_scroll))
            .child(terminal(frame, palette.background, on_layout, on_pointer))
            .child(input)
            .when_some(status, |view, status| {
                view.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(muted)
                        .child(status),
                )
            })
    }
}

/// The button a program can hear of.
fn heard(button: MouseButton) -> Option<Button> {
    match button {
        MouseButton::Left => Some(Button::Left),
        MouseButton::Middle => Some(Button::Middle),
        MouseButton::Right => Some(Button::Right),
        MouseButton::Navigate(_) => None,
    }
}

fn blank() -> Frame {
    Frame {
        lines: Vec::new(),
        cursor: None,
        preedit: None,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use agentdeck_client::ws::{Event, Outgoing, Remote};
    use agentdeck_core::AgentState;
    use alacritty_terminal::index::Side;
    use brrritscold_core_gpui::Root;
    use gpui::{
        AppContext as _, ClipboardItem, Modifiers, MouseDownEvent, Pixels, Point, ScrollDelta,
        TestAppContext, VisualTestContext, point, px,
    };

    use super::*;
    use crate::connection::fake::FakeConnector;
    use crate::connection::{Connector, Status};
    use crate::overview::fixtures::id;

    /// Holds the terminal the way the workspace view does, so a test can drop it.
    struct Host(Option<Entity<TerminalView>>);

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().children(self.0.clone())
        }
    }

    /// A window on a terminal for pane 1, over a connected fake daemon.
    struct Harness {
        remote: Remote,
        /// Keeps the socket open.
        _connection: Entity<Connection>,
        host: Entity<Host>,
    }

    impl Harness {
        fn open(cx: &mut TestAppContext) -> (Self, &mut VisualTestContext) {
            cx.update(brrritscold_core_gpui::init);
            cx.update(super::super::init);
            cx.update(crate::keymap::init);
            let connector = Rc::new(FakeConnector::default());
            let remote = connector.then_connect();
            let connector: Rc<dyn Connector> = connector;
            let connection = cx.update(|cx| cx.new(|cx| Connection::new(connector, cx)));
            cx.run_until_parked();
            let sender = match connection.read_with(cx, |c, _| c.status().clone()) {
                Status::Connected(session) => session.sender,
                _ => panic!("the fake connects"),
            };
            let mut host = None;
            let window = cx.add_window(|window, cx| {
                let terminal =
                    cx.new(|cx| TerminalView::new(id(1), &connection, sender, window, cx));
                let view = cx.new(|_| Host(Some(terminal)));
                host = Some(view.clone());
                Root::new(view, window, cx)
            });
            let cx = VisualTestContext::from_window(*window, cx).into_mut();
            let harness = Harness {
                remote,
                _connection: connection,
                host: host.unwrap(),
            };
            (harness, cx)
        }

        /// Records every title the terminal reports.
        fn titles(&self, cx: &mut VisualTestContext) -> Rc<RefCell<Vec<TerminalEvent>>> {
            let titles = Rc::new(RefCell::new(Vec::new()));
            let view = self.view(cx);
            let record = titles.clone();
            cx.update(|_, cx| {
                cx.subscribe(&view, move |_, event: &TerminalEvent, _| {
                    record.borrow_mut().push(event.clone());
                })
                .detach();
            });
            titles
        }

        fn view(&self, cx: &mut VisualTestContext) -> Entity<TerminalView> {
            self.host
                .read_with(cx, |host, _| host.0.clone().expect("a terminal"))
        }

        /// Focuses the terminal, as its pane does when shown or clicked.
        fn focus(&self, cx: &mut VisualTestContext) {
            let view = self.view(cx);
            cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
        }

        /// Brings the window to the front, as the user focusing it does.
        fn activate(&self, cx: &mut VisualTestContext) {
            cx.update(|window, _| window.activate_window());
            cx.run_until_parked();
        }

        fn draw(&self, cx: &mut VisualTestContext) {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.run_until_parked();
        }

        fn sent(&mut self) -> Vec<Outgoing> {
            std::iter::from_fn(|| self.remote.sent.try_recv().ok()).collect()
        }

        fn deliver(&self, event: Event, cx: &mut VisualTestContext) {
            self.remote.events.try_send(event).unwrap();
            cx.run_until_parked();
        }

        /// Plays the daemon's side of an attach on channel 1: `attached` at `cols` × `rows`,
        /// then `snapshot`.
        fn attached(&self, cols: u16, rows: u16, snapshot: &[u8], cx: &mut VisualTestContext) {
            self.deliver(
                Event::Message(ServerMessage::Attached {
                    channel: 1,
                    pane: id(1),
                    cols,
                    rows,
                    title: "sh".into(),
                    state: AgentState::Idle,
                }),
                cx,
            );
            self.deliver(Event::Output(1, snapshot.to_vec()), cx);
        }

        /// A window position over the cell at `line`, `col`: its left or right half.
        fn at(
            &self,
            line: usize,
            col: usize,
            side: Side,
            cx: &mut VisualTestContext,
        ) -> Point<Pixels> {
            let layout = self
                .view(cx)
                .read_with(cx, |view, _| view.layout)
                .expect("laid out");
            let x = match side {
                Side::Left => 0.25,
                Side::Right => 0.75,
            };
            layout.origin
                + point(
                    layout.cell.width * (col as f32 + x),
                    layout.cell.height * (line as f32 + 0.5),
                )
        }

        /// A live, focused terminal at 20 × 4 showing `snapshot`, laid out.
        fn live<'a>(
            snapshot: &[u8],
            cx: &'a mut TestAppContext,
        ) -> (Self, &'a mut VisualTestContext) {
            let (mut harness, cx) = Harness::open(cx);
            harness.focus(cx);
            harness.draw(cx);
            harness.attached(20, 4, snapshot, cx);
            harness.draw(cx);
            harness.sent();
            (harness, cx)
        }

        /// Drags with the left button from `from` to `to`, with `clicks` on the press.
        fn drag(
            &self,
            from: Point<Pixels>,
            to: Point<Pixels>,
            clicks: usize,
            modifiers: Modifiers,
            cx: &mut VisualTestContext,
        ) {
            cx.simulate_event(MouseDownEvent {
                position: from,
                button: MouseButton::Left,
                modifiers,
                click_count: clicks,
                first_mouse: false,
            });
            cx.simulate_mouse_move(to, MouseButton::Left, modifiers);
            cx.simulate_mouse_up(to, MouseButton::Left, modifiers);
            self.draw(cx);
        }

        /// Presses and lets go of `button` at `at`.
        fn click(
            &self,
            at: Point<Pixels>,
            button: MouseButton,
            modifiers: Modifiers,
            cx: &mut VisualTestContext,
        ) {
            cx.simulate_mouse_down(at, button, modifiers);
            cx.simulate_mouse_up(at, button, modifiers);
            self.draw(cx);
        }

        /// Turns the wheel `lines` notches at `at`: positive is up. Shift turns it sideways, as
        /// GPUI's Linux backends do.
        fn wheel(
            &self,
            at: Point<Pixels>,
            lines: f32,
            modifiers: Modifiers,
            cx: &mut VisualTestContext,
        ) {
            let delta = if modifiers.shift {
                point(lines, 0.)
            } else {
                point(0., lines)
            };
            cx.simulate_event(ScrollEvent {
                position: at,
                delta: ScrollDelta::Lines(delta),
                modifiers,
                ..ScrollEvent::default()
            });
            self.draw(cx);
        }

        /// The first visible line.
        fn top(&self, cx: &mut VisualTestContext) -> String {
            self.text(1, cx).expect("live").remove(0)
        }

        /// What `secondary-shift-c` put on the clipboard.
        fn copied(&self, cx: &mut VisualTestContext) -> Option<String> {
            cx.simulate_keystrokes("secondary-shift-c");
            cx.read_from_clipboard().and_then(|item| item.text())
        }

        /// The first `lines` visible lines, once live.
        fn text(&self, lines: usize, cx: &mut VisualTestContext) -> Option<Vec<String>> {
            let view = self.view(cx);
            view.read_with(cx, |view, _| view.text())
                .map(|text| text.into_iter().take(lines).collect())
        }
    }

    fn attach(pane: u32, channel: Channel) -> Outgoing {
        Outgoing::Message(ClientMessage::Attach {
            pane: id(pane),
            channel,
        })
    }

    mod title {
        use super::*;

        #[gpui::test]
        fn should_report_the_title_it_attached_with(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            let titles = harness.titles(cx);

            harness.attached(10, 2, b"", cx);

            assert_eq!(*titles.borrow(), [TerminalEvent::Title("sh".into())]);
        }

        #[gpui::test]
        fn should_report_a_title_the_program_sets(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            harness.attached(10, 2, b"", cx);
            let titles = harness.titles(cx);

            harness.deliver(
                Event::Message(ServerMessage::Title {
                    channel: 1,
                    title: "vim".into(),
                }),
                cx,
            );

            assert_eq!(*titles.borrow(), [TerminalEvent::Title("vim".into())]);
        }

        #[gpui::test]
        fn should_ignore_a_title_on_another_channel(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            harness.attached(10, 2, b"", cx);
            let titles = harness.titles(cx);

            harness.deliver(
                Event::Message(ServerMessage::Title {
                    channel: 9,
                    title: "theirs".into(),
                }),
                cx,
            );

            assert!(titles.borrow().is_empty());
        }
    }

    mod attach {
        use super::*;

        #[gpui::test]
        fn should_attach_its_pane_on_a_new_channel(cx: &mut TestAppContext) {
            let (mut harness, _) = Harness::open(cx);

            assert_eq!(harness.sent(), [attach(1, 1)]);
        }

        #[gpui::test]
        fn should_show_the_snapshot_once_attached(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);

            harness.attached(10, 2, b"\x1bc$ ls\r\na  b", cx);

            assert_eq!(harness.text(2, cx).unwrap(), ["$ ls", "a  b"]);
        }

        #[gpui::test]
        fn should_ignore_output_on_another_channel(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            harness.attached(10, 2, b"mine", cx);

            harness.deliver(Event::Output(9, b" theirs".to_vec()), cx);

            assert_eq!(harness.text(1, cx).unwrap(), ["mine"]);
        }

        #[gpui::test]
        fn should_attach_again_for_a_fresh_snapshot_when_lagged(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::open(cx);
            harness.attached(10, 2, b"", cx);
            harness.sent();

            harness.deliver(Event::Message(ServerMessage::Lagged { channel: 1 }), cx);

            assert_eq!(harness.sent(), [attach(1, 1)]);
            assert_eq!(harness.text(1, cx), None);
        }

        #[gpui::test]
        fn should_say_why_the_daemon_refused_it(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);

            harness.deliver(
                Event::Message(ServerMessage::Error {
                    channel: Some(1),
                    code: "not_found".into(),
                    message: "pane isn't live".into(),
                }),
                cx,
            );

            let refused = harness
                .view(cx)
                .read_with(cx, |view, _| view.refused().map(str::to_owned));
            assert_eq!(refused.as_deref(), Some("pane isn't live"));
            harness.draw(cx);
        }

        #[gpui::test]
        fn should_detach_when_dropped(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::open(cx);
            harness.sent();

            harness.host.update(cx, |host, _| host.0 = None);
            cx.run_until_parked();

            assert_eq!(
                harness.sent(),
                [Outgoing::Message(ClientMessage::Detach { channel: 1 })]
            );
        }
    }

    mod resize {
        use super::*;

        fn resized(sent: &[Outgoing]) -> Option<(u16, u16)> {
            sent.iter().rev().find_map(|outgoing| match outgoing {
                Outgoing::Message(ClientMessage::Resize { cols, rows, .. }) => Some((*cols, *rows)),
                _ => None,
            })
        }

        #[gpui::test]
        fn should_ask_for_the_grid_that_fits_once_the_snapshot_is_in(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::open(cx);
            harness.draw(cx);
            assert_eq!(resized(&harness.sent()), None);

            harness.attached(10, 2, b"", cx);

            let (cols, rows) = resized(&harness.sent()).expect("a resize");
            let size = harness
                .view(cx)
                .read_with(cx, |view, _| view.layout)
                .unwrap()
                .grid;
            assert_eq!((cols, rows), (size.cols, size.rows));
            assert_ne!((cols, rows), (10, 2));
        }

        #[gpui::test]
        fn should_take_the_size_the_daemon_keeps(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            harness.draw(cx);
            harness.attached(10, 2, b"", cx);

            harness.deliver(
                Event::Message(ServerMessage::Resize {
                    channel: 1,
                    cols: 12,
                    rows: 3,
                }),
                cx,
            );

            assert_eq!(harness.text(usize::MAX, cx).unwrap().len(), 3);
        }
    }

    mod primary {
        use super::*;

        const PRIMARY: Outgoing = Outgoing::Message(ClientMessage::Primary { channel: 1 });

        #[gpui::test]
        fn should_claim_primary_after_fitting_once_live_in_the_active_window(
            cx: &mut TestAppContext,
        ) {
            let (mut harness, cx) = Harness::open(cx);
            harness.activate(cx);
            harness.draw(cx);
            harness.sent();

            harness.attached(10, 2, b"", cx);

            let [resize, primary] = harness.sent().try_into().expect("a resize, then primary");
            assert!(matches!(
                resize,
                Outgoing::Message(ClientMessage::Resize { .. })
            ));
            assert_eq!(primary, PRIMARY);
        }

        #[gpui::test]
        fn should_claim_primary_again_when_the_window_is_activated(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::open(cx);
            harness.activate(cx);
            harness.attached(10, 2, b"", cx);
            cx.deactivate_window();
            harness.sent();

            harness.activate(cx);

            assert_eq!(harness.sent(), [PRIMARY]);
        }

        #[gpui::test]
        fn should_not_claim_primary_in_an_inactive_window(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::open(cx);
            harness.activate(cx);
            cx.deactivate_window();

            harness.attached(10, 2, b"", cx);

            assert!(!harness.sent().contains(&PRIMARY));
        }

        #[gpui::test]
        fn should_not_claim_primary_before_it_is_live(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::open(cx);
            harness.activate(cx);
            cx.deactivate_window();

            harness.activate(cx);

            assert!(!harness.sent().contains(&PRIMARY));
        }
    }

    mod typing {
        use super::*;

        #[gpui::test]
        fn should_send_keys_to_the_pane(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::open(cx);
            harness.focus(cx);
            harness.draw(cx);
            harness.attached(10, 2, b"", cx);
            harness.sent();

            cx.simulate_keystrokes("l s enter ctrl-c");

            assert_eq!(
                harness.sent(),
                [
                    Outgoing::Input(1, b"l".to_vec()),
                    Outgoing::Input(1, b"s".to_vec()),
                    Outgoing::Input(1, b"\r".to_vec()),
                    Outgoing::Input(1, vec![0x03]),
                ]
            );
        }

        #[gpui::test]
        fn should_send_nothing_before_it_is_attached(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::open(cx);
            harness.focus(cx);
            harness.sent();

            cx.simulate_keystrokes("a");

            assert_eq!(harness.sent(), []);
        }
    }

    mod select {
        use super::*;

        #[gpui::test]
        fn should_copy_the_cells_dragged_across(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::live(b"hello world", cx);
            let from = harness.at(0, 0, Side::Left, cx);
            let to = harness.at(0, 4, Side::Right, cx);

            harness.drag(from, to, 1, Modifiers::default(), cx);

            assert_eq!(harness.copied(cx).as_deref(), Some("hello"));
        }

        #[gpui::test]
        fn should_keep_selecting_when_dragged_out_of_the_terminal(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::live(b"hello world\r\nnext", cx);
            let from = harness.at(0, 6, Side::Left, cx);
            let past_the_right_edge = point(px(10_000.), harness.at(0, 0, Side::Left, cx).y);

            harness.drag(from, past_the_right_edge, 1, Modifiers::default(), cx);

            assert_eq!(harness.copied(cx).as_deref(), Some("world"));
        }

        #[gpui::test]
        fn should_select_a_word_on_a_double_click(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::live(b"cd ~/src/app", cx);
            let at = harness.at(0, 6, Side::Left, cx);

            harness.drag(at, at, 2, Modifiers::default(), cx);

            assert_eq!(harness.copied(cx).as_deref(), Some("~/src/app"));
        }

        #[gpui::test]
        fn should_select_a_line_on_a_triple_click(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::live(b"one\r\ntwo", cx);
            let at = harness.at(1, 0, Side::Left, cx);

            harness.drag(at, at, 3, Modifiers::default(), cx);

            assert_eq!(harness.copied(cx).as_deref(), Some("two\n"));
        }

        #[gpui::test]
        fn should_extend_the_selection_on_a_shift_click(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::live(b"hello world", cx);
            let (from, to) = (
                harness.at(0, 0, Side::Left, cx),
                harness.at(0, 1, Side::Right, cx),
            );
            harness.drag(from, to, 1, Modifiers::default(), cx);
            let further = harness.at(0, 10, Side::Right, cx);

            harness.drag(further, further, 1, Modifiers::shift(), cx);

            assert_eq!(harness.copied(cx).as_deref(), Some("hello world"));
        }

        #[gpui::test]
        fn should_clear_the_selection_on_a_click(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::live(b"hello world", cx);
            let (from, to) = (
                harness.at(0, 0, Side::Left, cx),
                harness.at(0, 4, Side::Right, cx),
            );
            harness.drag(from, to, 1, Modifiers::default(), cx);
            cx.write_to_clipboard(ClipboardItem::new_string("before".into()));

            harness.drag(from, from, 1, Modifiers::default(), cx);

            assert_eq!(harness.copied(cx).as_deref(), Some("before"));
        }

        #[gpui::test]
        fn should_put_a_finished_selection_on_the_primary_selection(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::live(b"hello world", cx);
            let from = harness.at(0, 6, Side::Left, cx);
            let to = harness.at(0, 10, Side::Right, cx);

            harness.drag(from, to, 1, Modifiers::default(), cx);

            let primary = cx
                .update(|_, cx| cx.read_from_primary())
                .and_then(|i| i.text());
            assert_eq!(primary.as_deref(), Some("world"));
        }

        #[gpui::test]
        fn should_send_nothing_to_the_pane(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::live(b"hello world", cx);
            let (from, to) = (
                harness.at(0, 0, Side::Left, cx),
                harness.at(0, 4, Side::Right, cx),
            );

            harness.drag(from, to, 1, Modifiers::default(), cx);
            harness.copied(cx);

            assert_eq!(harness.sent(), []);
        }

        #[gpui::test]
        fn should_draw_a_selection_in_both_themes(cx: &mut TestAppContext) {
            use brrritscold_core_gpui::{Theme, ThemePreference};

            let (harness, cx) = Harness::live(b"hello world", cx);
            let (from, to) = (
                harness.at(0, 0, Side::Left, cx),
                harness.at(0, 4, Side::Right, cx),
            );
            harness.drag(from, to, 1, Modifiers::default(), cx);

            for preference in [ThemePreference::Light, ThemePreference::Dark] {
                cx.update(|_, cx| Theme::set_preference(preference, cx));
                harness.draw(cx);
            }
        }
    }

    mod wheel {
        use super::*;

        /// Numbered lines, more than the grid shows, so some sit in the scrollback.
        fn numbered() -> Vec<u8> {
            (0..200)
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join("\r\n")
                .into_bytes()
        }

        fn top_number(harness: &Harness, cx: &mut VisualTestContext) -> i32 {
            harness.top(cx).trim().parse().expect("a numbered line")
        }

        #[gpui::test]
        fn should_scroll_the_scrollback_on_the_main_screen(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::live(&numbered(), cx);
            let bottom = top_number(&harness, cx);
            let at = harness.at(0, 0, Side::Left, cx);

            harness.wheel(at, 3., Modifiers::default(), cx);

            assert_eq!(top_number(&harness, cx), bottom - 3);
            assert_eq!(harness.sent(), []);
        }

        mod alternate_screen {
            use super::*;

            #[gpui::test]
            fn should_send_an_arrow_key_per_line(cx: &mut TestAppContext) {
                let (mut harness, cx) = Harness::live(b"\x1b[?1049h", cx);
                let at = harness.at(0, 0, Side::Left, cx);

                harness.wheel(at, 2., Modifiers::default(), cx);
                harness.wheel(at, -1., Modifiers::default(), cx);

                assert_eq!(
                    harness.sent(),
                    [
                        Outgoing::Input(1, b"\x1b[A\x1b[A".to_vec()),
                        Outgoing::Input(1, b"\x1b[B".to_vec()),
                    ]
                );
            }

            #[gpui::test]
            fn should_send_application_arrows_under_decckm(cx: &mut TestAppContext) {
                let (mut harness, cx) = Harness::live(b"\x1b[?1049h\x1b[?1h", cx);
                let at = harness.at(0, 0, Side::Left, cx);

                harness.wheel(at, 1., Modifiers::default(), cx);

                assert_eq!(harness.sent(), [Outgoing::Input(1, b"\x1bOA".to_vec())]);
            }

            #[gpui::test]
            fn should_send_nothing_once_the_program_turns_alternate_scroll_off(
                cx: &mut TestAppContext,
            ) {
                let (mut harness, cx) = Harness::live(b"\x1b[?1049h\x1b[?1007l", cx);
                let at = harness.at(0, 0, Side::Left, cx);

                harness.wheel(at, 1., Modifiers::default(), cx);

                assert_eq!(harness.sent(), []);
            }
        }

        mod mouse_tracking {
            use super::*;

            #[gpui::test]
            fn should_report_a_wheel_button_per_line_at_the_cell(cx: &mut TestAppContext) {
                let (mut harness, cx) = Harness::live(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h", cx);
                let at = harness.at(1, 3, Side::Left, cx);

                harness.wheel(at, 2., Modifiers::default(), cx);
                harness.wheel(at, -1., Modifiers::default(), cx);

                assert_eq!(
                    harness.sent(),
                    [
                        Outgoing::Input(1, b"\x1b[<64;4;2M\x1b[<64;4;2M".to_vec()),
                        Outgoing::Input(1, b"\x1b[<65;4;2M".to_vec()),
                    ]
                );
            }

            #[gpui::test]
            fn should_scroll_the_scrollback_instead_with_shift(cx: &mut TestAppContext) {
                let mut snapshot = numbered();
                snapshot.extend_from_slice(b"\x1b[?1000h");
                let (mut harness, cx) = Harness::live(&snapshot, cx);
                let bottom = top_number(&harness, cx);
                let at = harness.at(0, 0, Side::Left, cx);

                harness.wheel(at, 3., Modifiers::shift(), cx);

                assert_eq!(top_number(&harness, cx), bottom - 3);
                assert_eq!(harness.sent(), []);
            }
        }
    }

    mod mouse_tracking {
        use super::*;

        const SGR_CLICKS: &[u8] = b"hello world\x1b[?1000h\x1b[?1006h";

        fn input(bytes: &[u8]) -> Outgoing {
            Outgoing::Input(1, bytes.to_vec())
        }

        #[gpui::test]
        fn should_report_a_click_instead_of_selecting(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::live(SGR_CLICKS, cx);
            let at = harness.at(1, 3, Side::Left, cx);

            harness.drag(at, at, 1, Modifiers::default(), cx);

            assert_eq!(
                harness.sent(),
                [input(b"\x1b[<0;4;2M"), input(b"\x1b[<0;4;2m")]
            );
            assert_eq!(harness.copied(cx), None);
        }

        #[gpui::test]
        fn should_report_the_middle_and_right_buttons(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::live(SGR_CLICKS, cx);
            cx.update(|_, cx| cx.write_to_primary(ClipboardItem::new_string("echo hi".into())));
            let at = harness.at(0, 0, Side::Left, cx);

            harness.click(at, MouseButton::Middle, Modifiers::default(), cx);
            harness.click(at, MouseButton::Right, Modifiers::default(), cx);

            assert_eq!(
                harness.sent(),
                [
                    input(b"\x1b[<1;1;1M"),
                    input(b"\x1b[<1;1;1m"),
                    input(b"\x1b[<2;1;1M"),
                    input(b"\x1b[<2;1;1m"),
                ]
            );
        }

        #[gpui::test]
        fn should_use_the_normal_encoding_without_sgr(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::live(b"\x1b[?1000h", cx);
            let at = harness.at(1, 3, Side::Left, cx);

            harness.click(at, MouseButton::Left, Modifiers::default(), cx);

            assert_eq!(harness.sent(), [input(b"\x1b[M $\""), input(b"\x1b[M#$\"")]);
        }

        #[gpui::test]
        fn should_not_report_drags_when_only_clicks_are_tracked(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::live(SGR_CLICKS, cx);
            let (from, to) = (
                harness.at(0, 0, Side::Left, cx),
                harness.at(0, 2, Side::Left, cx),
            );

            harness.drag(from, to, 1, Modifiers::default(), cx);

            assert_eq!(
                harness.sent(),
                [input(b"\x1b[<0;1;1M"), input(b"\x1b[<0;3;1m")]
            );
        }

        #[gpui::test]
        fn should_report_a_drag_once_per_cell_when_asked(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::live(b"\x1b[?1002h\x1b[?1006h", cx);
            let from = harness.at(0, 0, Side::Left, cx);
            let same_cell = harness.at(0, 0, Side::Right, cx);
            let next = harness.at(0, 1, Side::Left, cx);

            cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_move(same_cell, MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_move(next, MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_up(next, MouseButton::Left, Modifiers::default());

            assert_eq!(
                harness.sent(),
                [
                    input(b"\x1b[<0;1;1M"),
                    input(b"\x1b[<32;2;1M"),
                    input(b"\x1b[<0;2;1m"),
                ]
            );
        }

        #[gpui::test]
        fn should_report_moves_without_a_button_under_any_event_tracking(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::live(b"\x1b[?1003h\x1b[?1006h", cx);
            let at = harness.at(1, 1, Side::Left, cx);
            let same_cell = harness.at(1, 1, Side::Right, cx);

            cx.simulate_mouse_move(at, None, Modifiers::default());
            cx.simulate_mouse_move(same_cell, None, Modifiers::default());

            assert_eq!(harness.sent(), [input(b"\x1b[<35;2;2M")]);
        }

        #[gpui::test]
        fn should_select_with_shift_held(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::live(SGR_CLICKS, cx);
            let from = harness.at(0, 0, Side::Left, cx);
            let to = harness.at(0, 4, Side::Right, cx);

            harness.drag(from, to, 1, Modifiers::shift(), cx);

            assert_eq!(harness.sent(), []);
            assert_eq!(harness.copied(cx).as_deref(), Some("hello"));
        }

        #[gpui::test]
        fn should_select_again_once_the_program_stops_tracking(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::live(SGR_CLICKS, cx);
            harness.deliver(Event::Output(1, b"\x1b[?1000l".to_vec()), cx);
            let from = harness.at(0, 0, Side::Left, cx);
            let to = harness.at(0, 4, Side::Right, cx);

            harness.drag(from, to, 1, Modifiers::default(), cx);

            assert_eq!(harness.sent(), []);
            assert_eq!(harness.copied(cx).as_deref(), Some("hello"));
        }
    }

    mod ime {
        use gpui::Keystroke;

        use super::*;

        /// Runs `f` on the terminal, as the platform's input handler does.
        fn handle<R>(
            harness: &Harness,
            cx: &mut VisualTestContext,
            f: impl FnOnce(&mut TerminalView, &mut Window, &mut Context<TerminalView>) -> R,
        ) -> R {
            let view = harness.view(cx);
            let result = cx.update(|window, cx| view.update(cx, |view, cx| f(view, window, cx)));
            harness.draw(cx);
            result
        }

        fn compose(harness: &Harness, text: &str, cx: &mut VisualTestContext) {
            handle(harness, cx, |view, window, cx| {
                view.replace_and_mark_text_in_range(None, text, None, window, cx)
            });
        }

        fn commit(harness: &Harness, text: &str, cx: &mut VisualTestContext) {
            handle(harness, cx, |view, window, cx| {
                view.replace_text_in_range(None, text, window, cx)
            });
        }

        /// What the terminal draws at the cursor for the composition.
        fn preedit(harness: &Harness, cx: &mut VisualTestContext) -> Option<String> {
            let palette = Palette::new(cx.update(|_, cx| cx.theme().mode()));
            harness
                .view(cx)
                .read_with(cx, |view, _| view.frame(&palette, true).preedit)
                .map(|preedit| preedit.text)
        }

        #[gpui::test]
        fn should_type_the_text_an_input_method_commits(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::live(b"", cx);

            commit(&harness, "日本", cx);

            assert_eq!(
                harness.sent(),
                [Outgoing::Input(1, "日本".as_bytes().to_vec())]
            );
        }

        #[gpui::test]
        fn should_show_a_composition_at_the_cursor_and_send_nothing(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::live(b"", cx);

            compose(&harness, "にほ", cx);

            assert_eq!(preedit(&harness, cx).as_deref(), Some("にほ"));
            let marked = handle(&harness, cx, |view, window, cx| {
                view.marked_text_range(window, cx)
            });
            assert_eq!(marked, Some(0..2));
            assert_eq!(harness.sent(), []);
        }

        #[gpui::test]
        fn should_drop_the_composition_once_committed(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::live(b"", cx);
            compose(&harness, "にほ", cx);

            commit(&harness, "日本", cx);

            assert_eq!(preedit(&harness, cx), None);
            assert_eq!(
                harness.sent(),
                [Outgoing::Input(1, "日本".as_bytes().to_vec())]
            );
        }

        #[gpui::test]
        fn should_drop_the_composition_when_the_input_method_unmarks_it(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::live(b"", cx);
            compose(&harness, "にほ", cx);

            handle(&harness, cx, |view, window, cx| {
                view.unmark_text(window, cx)
            });

            assert_eq!(preedit(&harness, cx), None);
            assert_eq!(harness.sent(), []);
        }

        #[gpui::test]
        fn should_put_the_candidate_window_at_the_cursor(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::live(b"ab", cx);
            let layout = harness
                .view(cx)
                .read_with(cx, |view, _| view.layout)
                .expect("laid out");

            let bounds = handle(&harness, cx, |view, window, cx| {
                view.bounds_for_range(0..0, Bounds::default(), window, cx)
            });

            assert_eq!(
                bounds,
                Some(Bounds::new(
                    layout.origin + point(layout.cell.width * 2., px(0.)),
                    layout.cell,
                ))
            );
        }

        #[gpui::test]
        fn should_take_text_no_key_handler_took(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::live(b"", cx);
            // The terminal sends no bytes for a function-modified key, so its text goes on to
            // the platform's input handler, as a cancelled dead key's does.
            let keystroke = Keystroke {
                key: "e".into(),
                key_char: Some("é".into()),
                modifiers: Modifiers {
                    function: true,
                    ..Modifiers::default()
                },
            };

            cx.update(|window, cx| window.dispatch_keystroke(keystroke, cx));

            assert_eq!(
                harness.sent(),
                [Outgoing::Input(1, "é".as_bytes().to_vec())]
            );
        }

        #[gpui::test]
        fn should_draw_a_composition_in_both_themes(cx: &mut TestAppContext) {
            use brrritscold_core_gpui::{Theme, ThemePreference};

            let (harness, cx) = Harness::live(b"$ ", cx);
            compose(&harness, "にほ", cx);

            for preference in [ThemePreference::Light, ThemePreference::Dark] {
                cx.update(|_, cx| Theme::set_preference(preference, cx));
                harness.draw(cx);
            }
        }
    }

    mod paste {
        use super::*;

        fn pasted(snapshot: &[u8], clipboard: &str, cx: &mut TestAppContext) -> Vec<Outgoing> {
            let (mut harness, cx) = Harness::live(snapshot, cx);
            cx.write_to_clipboard(ClipboardItem::new_string(clipboard.into()));

            cx.simulate_keystrokes("secondary-shift-v");

            harness.sent()
        }

        #[gpui::test]
        fn should_type_the_clipboard_with_line_endings_as_enter(cx: &mut TestAppContext) {
            assert_eq!(
                pasted(b"", "ls\npwd\n", cx),
                [Outgoing::Input(1, b"ls\rpwd\r".to_vec())]
            );
        }

        #[gpui::test]
        fn should_bracket_the_paste_when_the_program_asks(cx: &mut TestAppContext) {
            assert_eq!(
                pasted(b"\x1b[?2004h", "ls\n", cx),
                [Outgoing::Input(1, b"\x1b[200~ls\n\x1b[201~".to_vec())]
            );
        }

        #[gpui::test]
        fn should_send_nothing_for_an_empty_clipboard(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::live(b"", cx);

            cx.simulate_keystrokes("secondary-shift-v");

            assert_eq!(harness.sent(), []);
        }

        #[gpui::test]
        fn should_paste_the_primary_selection_on_a_middle_click(cx: &mut TestAppContext) {
            let (mut harness, cx) = Harness::live(b"", cx);
            cx.update(|_, cx| cx.write_to_primary(ClipboardItem::new_string("echo hi".into())));
            let at = harness.at(0, 0, Side::Left, cx);

            cx.simulate_mouse_down(at, MouseButton::Middle, Modifiers::default());

            assert_eq!(harness.sent(), [Outgoing::Input(1, b"echo hi".to_vec())]);
        }
    }

    mod render {
        use brrritscold_core_gpui::{Theme, ThemePreference};

        use super::*;

        #[gpui::test]
        fn should_draw_styled_output_in_both_themes(cx: &mut TestAppContext) {
            let (harness, cx) = Harness::open(cx);
            harness.draw(cx);
            harness.attached(
                20,
                3,
                "\x1b[1;31m✻ red\x1b[0m \x1b[7minv\x1b[0m 漢字 ⎿ ◐\r\n\x1b[4munder\x1b[0m"
                    .as_bytes(),
                cx,
            );

            for preference in [ThemePreference::Light, ThemePreference::Dark] {
                cx.update(|_, cx| Theme::set_preference(preference, cx));
                harness.draw(cx);
            }
        }
    }
}
