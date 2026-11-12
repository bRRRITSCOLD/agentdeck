//! PROTOTYPE: the GPUI terminal view. Paints `TermModel::snapshot()` on a canvas: one
//! background quad per styled run, then each run shaped with `force_width` = the cell width,
//! so fallback glyphs can't push later columns out of the grid.

use std::time::Instant;

use futures::channel::mpsc::UnboundedSender;
use gpui::{
    App, Bounds, Context, FocusHandle, Font, FontStyle, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Pixels, Render, SharedString, Styled as _,
    TextAlign, TextRun, UnderlineStyle, Window, canvas, div, fill, font, point, px, size,
};
use serde_json::Value;

use crate::term::{Snapshot, TermModel, keystroke_bytes};
use crate::ws::{Message, resize_msg};

pub const FONT_FAMILY: &str = "DejaVu Sans Mono";
pub const FONT_SIZE: Pixels = px(14.);
pub const LINE_HEIGHT: Pixels = px(18.);
pub const PAD: Pixels = px(8.);

/// What the client knows about the attached session.
pub struct Attached {
    pub model: TermModel,
    pub live: bool,
    pub agent_state: Value,
    pub hello: Value,
    pub live_msg: Value,
    pub exited: Option<Value>,
    pub attach_started: Instant,
    pub live_at_ms: Option<f64>,
    pub fed_bytes: usize,
    pub feed_time: std::time::Duration,
    /// Experiment: replay everything at this fixed size, ignoring in-band resizes.
    pub naive: Option<(u16, u16)>,
}

impl Attached {
    pub fn new() -> Self {
        Self {
            model: TermModel::new(80, 24),
            live: false,
            agent_state: Value::Null,
            hello: Value::Null,
            live_msg: Value::Null,
            exited: None,
            attach_started: Instant::now(),
            live_at_ms: None,
            fed_bytes: 0,
            feed_time: Default::default(),
            naive: None,
        }
    }

    /// Applies one server message. Returns bytes to send back (terminal replies).
    pub fn apply(&mut self, msg: Message) -> Vec<u8> {
        match msg {
            Message::Binary(b) => {
                let t = Instant::now();
                self.model.feed(&b);
                self.feed_time += t.elapsed();
                self.fed_bytes += b.len();
                self.model.take_replies()
            }
            Message::Text(t) => {
                let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                match v["t"].as_str() {
                    Some("hello") => {
                        // A fresh grid at the size the first replayed byte was written at.
                        let cols = v["cols"].as_u64().unwrap_or(80) as u16;
                        let rows = v["rows"].as_u64().unwrap_or(24) as u16;
                        let (cols, rows) = self.naive.unwrap_or((cols, rows));
                        self.model = TermModel::new(cols, rows);
                        self.model.set_replaying(true);
                        self.agent_state = v["state"].clone();
                        self.hello = v;
                    }
                    Some("resize") if self.naive.is_some() && !self.live => {}
                    Some("resize") => {
                        let cols = v["cols"].as_u64().unwrap_or(80) as u16;
                        let rows = v["rows"].as_u64().unwrap_or(24) as u16;
                        self.model.resize(cols, rows);
                    }
                    Some("live") => {
                        self.model.set_replaying(false);
                        self.live = true;
                        self.live_at_ms = Some(self.attach_started.elapsed().as_secs_f64() * 1000.);
                        self.live_msg = v;
                    }
                    Some("state") => self.agent_state = v["state"].clone(),
                    Some("exit") => self.exited = Some(v),
                    _ => {}
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
}

impl Default for Attached {
    fn default() -> Self {
        Self::new()
    }
}

pub struct TerminalView {
    pub attached: Attached,
    pub focus: FocusHandle,
    /// Outgoing WebSocket messages (None in headless captures).
    pub tx: Option<UnboundedSender<Message>>,
    /// Fit the grid to the window and tell the daemon (window mode).
    pub autosize: bool,
    pub status: SharedString,
}

impl TerminalView {
    pub fn new(
        attached: Attached,
        tx: Option<UnboundedSender<Message>>,
        autosize: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            attached,
            focus: cx.focus_handle(),
            tx,
            autosize,
            status: "connecting".into(),
        }
    }

    fn send(&self, msg: Message) {
        if let Some(tx) = &self.tx {
            let _ = tx.unbounded_send(msg);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        let app_cursor = self
            .attached
            .model
            .mode()
            .contains(alacritty_terminal::term::TermMode::APP_CURSOR);
        if let Some(bytes) = keystroke_bytes(
            &k.key,
            k.key_char.as_deref(),
            k.modifiers.control,
            k.modifiers.alt,
            k.modifiers.shift,
            app_cursor,
        ) {
            self.send(Message::binary(bytes));
            cx.stop_propagation();
        }
    }

    pub fn cell_width(window: &Window) -> Pixels {
        let ts = window.text_system();
        let id = ts.resolve_font(&font(FONT_FAMILY));
        ts.advance(id, FONT_SIZE, 'm')
            .map(|s| s.width)
            .unwrap_or(px(8.4))
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cell_w = Self::cell_width(window);
        if self.autosize && self.attached.live {
            let vp = window.viewport_size();
            let cols = (((vp.width - PAD * 2.) / cell_w).floor() as u16).max(10);
            let rows = (((vp.height - PAD * 2. - LINE_HEIGHT) / LINE_HEIGHT).floor() as u16).max(3);
            if self.attached.model.size() != (cols, rows) {
                self.attached.model.resize(cols, rows);
                self.send(resize_msg(cols, rows));
            }
        }
        let snap = self.attached.model.snapshot();
        let bg = rgb(snap.background);
        let (cols, rows) = self.attached.model.size();
        let bar = format!(
            "{}  {}x{}  replay {} B",
            self.status, cols, rows, self.attached.hello["replay_bytes"]
        );
        div()
            .size_full()
            .bg(bg)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .flex()
            .flex_col()
            .child(
                div()
                    .h(LINE_HEIGHT)
                    .px(PAD)
                    .bg(gpui::rgb(0x2a2c30))
                    .text_color(gpui::rgb(0x9da1a6))
                    .text_size(px(12.))
                    .font_family(FONT_FAMILY)
                    .child(bar),
            )
            .child(
                div().flex_1().p(PAD).child(
                    canvas(
                        |_, _, _| {},
                        move |bounds, (), window, cx| paint(&snap, bounds, cell_w, window, cx),
                    )
                    .size_full(),
                ),
            )
    }
}

fn rgb(c: alacritty_terminal::vte::ansi::Rgb) -> Hsla {
    gpui::rgb((c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32).into()
}

fn paint(
    snap: &Snapshot,
    bounds: Bounds<Pixels>,
    cell_w: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let origin = bounds.origin;
    for (i, segs) in snap.rows.iter().enumerate() {
        let y = origin.y + LINE_HEIGHT * i as f32;
        for seg in segs {
            let x = origin.x + cell_w * seg.col as f32;
            if let Some(bg) = seg.bg {
                window.paint_quad(fill(
                    Bounds::new(point(x, y), size(cell_w * seg.cells as f32, LINE_HEIGHT)),
                    rgb(bg),
                ));
            }
        }
    }
    if let Some((col, row)) = snap.cursor
        && row < snap.rows.len()
    {
        let x = origin.x + cell_w * col as f32;
        let y = origin.y + LINE_HEIGHT * row as f32;
        window.paint_quad(fill(
            Bounds::new(point(x, y), size(cell_w, LINE_HEIGHT)),
            rgb(snap.foreground).opacity(0.7),
        ));
    }
    for (i, segs) in snap.rows.iter().enumerate() {
        let y = origin.y + LINE_HEIGHT * i as f32;
        for seg in segs {
            if seg.text.trim().is_empty() {
                continue;
            }
            let x = origin.x + cell_w * seg.col as f32;
            let font = Font {
                weight: if seg.bold {
                    FontWeight::BOLD
                } else {
                    FontWeight::NORMAL
                },
                style: if seg.italic {
                    FontStyle::Italic
                } else {
                    FontStyle::Normal
                },
                ..font(FONT_FAMILY)
            };
            let run = TextRun {
                len: seg.text.len(),
                font,
                color: rgb(seg.fg),
                background_color: None,
                underline: seg.underline.then(|| UnderlineStyle {
                    thickness: px(1.),
                    color: Some(rgb(seg.fg)),
                    wavy: false,
                }),
                strikethrough: None,
            };
            let width = if seg.wide { cell_w * 2. } else { cell_w };
            let line = window.text_system().shape_line(
                seg.text.clone().into(),
                FONT_SIZE,
                &[run],
                Some(width),
            );
            let _ = line.paint(point(x, y), LINE_HEIGHT, TextAlign::Left, None, window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn should_send_typed_keys_to_the_pty(cx: &mut TestAppContext) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let (view, cx) =
            cx.add_window_view(|_, cx| TerminalView::new(Attached::new(), Some(tx), false, cx));
        cx.update(|window, cx| {
            let focus = view.read(cx).focus.clone();
            focus.focus(window, cx);
        });
        cx.simulate_keystrokes("l s enter ctrl-c up");
        let mut sent = Vec::new();
        while let Ok(Some(Message::Binary(b))) = rx.try_next() {
            sent.extend_from_slice(&b);
        }
        assert_eq!(sent, b"ls\r\x03\x1b[A");
    }
}
