//! One attached pane (spec §5): attaches on a channel of its own, feeds the snapshot and live
//! output into a [`Screen`], asks for the size that fits, types into the PTY, and detaches
//! when dropped.
//!
//! While its window is the active one it claims `primary`, so its size wins over other
//! clients' (spec §5): when it goes live, and again whenever the window is activated.

use agentdeck_client::ws;
use agentdeck_core::PaneId;
use agentdeck_core::ws::{Channel, ClientMessage, ServerMessage};
use brrritscold_core_gpui::ActiveTheme as _;
use brrritscold_core_gpui_tokens::primitive::space;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, ParentElement as _, Render, ScrollEvent, SharedString,
    Styled as _, Subscription, Window, div,
};

use super::element::{Metrics, terminal};
use super::input;
use super::palette::Palette;
use super::screen::{Frame, GridSize, Screen};
use crate::connection::{Connection, DaemonEvent};

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
    /// The grid that fits the view, once laid out.
    grid: Option<GridSize>,
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
            grid: None,
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

    /// Records the grid that fits, and resizes the pane to it once live.
    fn set_grid(&mut self, grid: GridSize, cx: &mut Context<Self>) {
        if self.grid != Some(grid) {
            self.grid = Some(grid);
            if self.fit() {
                cx.notify();
            }
        }
    }

    /// Resizes a live pane to the grid that fits; whether its size changed.
    fn fit(&mut self) -> bool {
        let (Attachment::Live(screen), Some(grid)) = (&mut self.attachment, self.grid) else {
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
        let Attachment::Live(screen) = &mut self.attachment else {
            return;
        };
        if screen.alt_screen() {
            return;
        }
        let line = Metrics::measure(window).cell.height;
        let lines = (event.delta.pixel_delta(line).y / line).round() as i32;
        if lines != 0 {
            screen.scroll(lines);
            cx.notify();
        }
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
            Attachment::Live(screen) => (screen.frame(&palette, focused), None),
            Attachment::Attaching | Attachment::AwaitingSnapshot(_) => (blank(), None),
            Attachment::Refused(reason) => (blank(), Some(reason.clone())),
        };
        let view = cx.entity().downgrade();
        let on_grid = move |grid, _: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |view, cx| view.set_grid(grid, cx));
        };
        let muted = mode.semantic().muted_foreground;
        // Penpot `agentdeck / TerminalSurface`: `background`, padding `space.3`.
        div()
            .id(("terminal", self.channel))
            .track_focus(&self.focus)
            .key_context("Terminal")
            .relative()
            .size_full()
            .p(space::S3)
            .bg(palette.background)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)),
            )
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll(cx.listener(Self::on_scroll))
            .child(terminal(frame, palette.background, on_grid))
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

fn blank() -> Frame {
    Frame {
        lines: Vec::new(),
        cursor: None,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use agentdeck_client::ws::{Event, Outgoing, Remote};
    use agentdeck_core::AgentState;
    use brrritscold_core_gpui::Root;
    use gpui::{AppContext as _, TestAppContext, VisualTestContext};

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
            let size = harness.view(cx).read_with(cx, |view, _| view.grid).unwrap();
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

            let sent = harness.sent();
            assert!(
                matches!(
                    sent.as_slice(),
                    [Outgoing::Message(ClientMessage::Resize { .. }), primary] if *primary == PRIMARY
                ),
                "{sent:?}"
            );
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
