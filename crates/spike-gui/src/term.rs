//! PROTOTYPE: the terminal model, written against `alacritty_terminal`'s public API only.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, Rgb};

/// Collects what the terminal wants written back to the PTY (DA / DSR / colour replies).
/// While replaying, replies are dropped: the app asked those questions long ago and already
/// got its answer from whichever client was attached (or none).
#[derive(Clone)]
pub struct Listener {
    pub replaying: Arc<AtomicBool>,
    pub replies: Arc<Mutex<Vec<u8>>>,
    pub dropped_replies: Arc<Mutex<usize>>,
    pub sent_replies: Arc<Mutex<Vec<String>>>,
    pub title: Arc<Mutex<Option<String>>>,
}

impl Listener {
    fn reply(&self, s: String) {
        if self.replaying.load(Ordering::Relaxed) {
            *self.dropped_replies.lock().unwrap() += 1;
        } else {
            self.sent_replies.lock().unwrap().push(s.clone());
            self.replies.lock().unwrap().extend_from_slice(s.as_bytes());
        }
    }
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(s) => self.reply(s),
            // OSC 10/11/4 colour queries: answer from the spike's palette.
            Event::ColorRequest(index, format) => {
                let rgb = match index {
                    256 => hex(0xd4d4d4),
                    257 => hex(0x161719),
                    i if i < 256 => palette(i),
                    _ => hex(0xd4d4d4),
                };
                self.reply(format(rgb));
            }
            Event::Title(t) => *self.title.lock().unwrap() = Some(t),
            _ => {}
        }
    }
}

pub struct TermModel {
    pub term: Term<Listener>,
    parser: Processor,
    pub listener: Listener,
}

/// A run of cells sharing one style, starting at `col`, `cells` wide.
#[derive(Clone, Debug)]
pub struct Seg {
    pub col: usize,
    pub cells: usize,
    pub text: String,
    pub fg: Rgb,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub wide: bool,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub cols: usize,
    pub rows: Vec<Vec<Seg>>,
    pub cursor: Option<(usize, usize)>,
    pub background: Rgb,
    pub foreground: Rgb,
}

impl TermModel {
    pub fn new(cols: u16, rows: u16) -> Self {
        let listener = Listener {
            replaying: Arc::new(AtomicBool::new(false)),
            replies: Default::default(),
            dropped_replies: Default::default(),
            sent_replies: Default::default(),
            title: Default::default(),
        };
        let size = TermSize::new(cols as usize, rows as usize);
        let term = Term::new(Config::default(), &size, listener.clone());
        Self {
            term,
            parser: Processor::new(),
            listener,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    pub fn set_replaying(&self, on: bool) {
        self.listener.replaying.store(on, Ordering::Relaxed);
    }

    pub fn take_replies(&self) -> Vec<u8> {
        std::mem::take(&mut *self.listener.replies.lock().unwrap())
    }

    pub fn size(&self) -> (u16, u16) {
        (self.term.columns() as u16, self.term.screen_lines() as u16)
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if self.size() != (cols, rows) {
            self.term
                .resize(TermSize::new(cols as usize, rows as usize));
        }
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    /// The visible screen as plain text, one line per row (trailing spaces trimmed).
    pub fn text(&self) -> String {
        let grid = self.term.grid();
        let mut out = String::new();
        for i in 0..self.term.screen_lines() {
            let row = &grid[Line(i as i32)];
            let mut line = String::new();
            for j in 0..self.term.columns() {
                let cell = &row[Column(j)];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                line.push(cell.c);
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    /// The visible screen with resolved colours, as cell-aligned style runs.
    pub fn snapshot(&self) -> Snapshot {
        let grid = self.term.grid();
        let colors = self.term.colors();
        let resolve = |c: Color, bold: bool| -> Rgb {
            match c {
                Color::Spec(rgb) => rgb,
                Color::Indexed(i) => {
                    let i = if bold && i < 8 { i + 8 } else { i };
                    colors[i as usize].unwrap_or_else(|| palette(i as usize))
                }
                Color::Named(n) => {
                    let mut idx = n as usize;
                    if bold && idx < 8 {
                        idx += 8;
                    }
                    if let Some(rgb) = colors[idx] {
                        return rgb;
                    }
                    named(n, idx)
                }
            }
        };
        let fg_default = resolve(Color::Named(NamedColor::Foreground), false);
        let bg_default = resolve(Color::Named(NamedColor::Background), false);
        let mut rows = Vec::new();
        for i in 0..self.term.screen_lines() {
            let row = &grid[Line(i as i32)];
            let mut segs: Vec<Seg> = Vec::new();
            for j in 0..self.term.columns() {
                let cell = &row[Column(j)];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                let bold = cell.flags.contains(Flags::BOLD);
                let mut fg = resolve(cell.fg, bold);
                let mut bg = resolve(cell.bg, false);
                if cell.flags.contains(Flags::DIM) {
                    fg = dim(fg);
                }
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cell.flags.contains(Flags::HIDDEN) {
                    fg = bg;
                }
                let bg = (bg != bg_default).then_some(bg);
                let wide = cell.flags.contains(Flags::WIDE_CHAR);
                let italic = cell.flags.contains(Flags::ITALIC);
                let underline = cell.flags.intersects(Flags::ALL_UNDERLINES);
                let mut text = String::from(cell.c);
                if let Some(zw) = cell.zerowidth() {
                    text.extend(zw);
                }
                let cells = if wide { 2 } else { 1 };
                if let Some(last) = segs.last_mut()
                    && !wide
                    && !last.wide
                    && last.fg == fg
                    && last.bg == bg
                    && last.bold == bold
                    && last.italic == italic
                    && last.underline == underline
                {
                    last.text.push_str(&text);
                    last.cells += 1;
                    continue;
                }
                segs.push(Seg {
                    col: j,
                    cells,
                    text,
                    fg,
                    bg,
                    bold,
                    italic,
                    underline,
                    wide,
                });
            }
            rows.push(segs);
        }
        let mode = self.mode();
        let point = self.term.grid().cursor.point;
        let cursor = mode
            .contains(TermMode::SHOW_CURSOR)
            .then_some((point.column.0, point.line.0 as usize));
        Snapshot {
            cols: self.term.columns(),
            rows,
            cursor,
            background: bg_default,
            foreground: fg_default,
        }
    }
}

impl TermModel {
    /// Experiment for Q2: the screen re-encoded as escape sequences (a "snapshot"), so a
    /// daemon running its own `Term` could send this on attach instead of an unbounded log.
    /// Visible screen + modes + cursor only: no scrollback, and the primary screen under an
    /// alternate screen is lost.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = String::from("\x1bc");
        let mode = self.mode();
        if mode.contains(TermMode::ALT_SCREEN) {
            out.push_str("\x1b[?1049h");
        }
        out.push_str("\x1b[H\x1b[2J");
        let grid = self.term.grid();
        for i in 0..self.term.screen_lines() {
            out.push_str(&format!("\x1b[{};1H\x1b[0m", i + 1));
            let row = &grid[Line(i as i32)];
            let mut last = String::from("0");
            for j in 0..self.term.columns() {
                let cell = &row[Column(j)];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                let sgr = sgr(cell);
                if sgr != last {
                    out.push_str(&format!("\x1b[{sgr}m"));
                    last = sgr;
                }
                out.push(cell.c);
                if let Some(zw) = cell.zerowidth() {
                    out.extend(zw);
                }
            }
        }
        out.push_str("\x1b[0m");
        let p = grid.cursor.point;
        out.push_str(&format!("\x1b[{};{}H", p.line.0 + 1, p.column.0 + 1));
        for (flag, on) in [
            (TermMode::APP_CURSOR, "\x1b[?1h"),
            (TermMode::APP_KEYPAD, "\x1b="),
            (TermMode::BRACKETED_PASTE, "\x1b[?2004h"),
            (TermMode::MOUSE_REPORT_CLICK, "\x1b[?1000h"),
            (TermMode::MOUSE_DRAG, "\x1b[?1002h"),
            (TermMode::MOUSE_MOTION, "\x1b[?1003h"),
            (TermMode::SGR_MOUSE, "\x1b[?1006h"),
            (TermMode::FOCUS_IN_OUT, "\x1b[?1004h"),
        ] {
            if mode.contains(flag) {
                out.push_str(on);
            }
        }
        if !mode.contains(TermMode::SHOW_CURSOR) {
            out.push_str("\x1b[?25l");
        }
        out.into_bytes()
    }
}

fn sgr(cell: &alacritty_terminal::term::cell::Cell) -> String {
    let mut p = vec!["0".to_owned()];
    for (f, code) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::UNDERLINE, "4"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKEOUT, "9"),
    ] {
        if cell.flags.contains(f) {
            p.push(code.into());
        }
    }
    let color = |c: Color, base: u8| -> Option<String> {
        match c {
            Color::Named(n) => {
                let i = n as usize;
                if i < 8 {
                    Some(format!("{}", base as usize + i))
                } else if i < 16 {
                    Some(format!("{}", base as usize + 60 + i - 8))
                } else {
                    None
                }
            }
            Color::Indexed(i) => Some(format!("{};5;{i}", base + 8)),
            Color::Spec(c) => Some(format!("{};2;{};{};{}", base + 8, c.r, c.g, c.b)),
        }
    };
    p.extend(color(cell.fg, 30));
    p.extend(color(cell.bg, 40));
    p.join(";")
}

const ANSI: [u32; 16] = [
    0x1d1f21, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6, //
    0x666666, 0xd54e53, 0xb9ca4a, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea,
];

fn hex(v: u32) -> Rgb {
    Rgb {
        r: (v >> 16) as u8,
        g: (v >> 8) as u8,
        b: v as u8,
    }
}

/// xterm's 256-colour palette over a Tomorrow Night base.
fn palette(i: usize) -> Rgb {
    match i {
        0..16 => hex(ANSI[i]),
        16..232 => {
            let i = i - 16;
            let step = |v: usize| if v == 0 { 0 } else { (v * 40 + 55) as u8 };
            Rgb {
                r: step(i / 36),
                g: step((i / 6) % 6),
                b: step(i % 6),
            }
        }
        _ => {
            let v = (8 + (i - 232) * 10) as u8;
            Rgb { r: v, g: v, b: v }
        }
    }
}

fn named(n: NamedColor, idx: usize) -> Rgb {
    match n {
        NamedColor::Foreground | NamedColor::BrightForeground => hex(0xd4d4d4),
        NamedColor::Background => hex(0x161719),
        NamedColor::Cursor => hex(0xd4d4d4),
        NamedColor::DimForeground => hex(0x8c8c8c),
        _ if idx < 16 => palette(idx),
        // Dim variants: the normal colour, darkened.
        _ => {
            let base = palette((idx - NamedColor::DimBlack as usize) % 8);
            dim(base)
        }
    }
}

/// GPUI keystroke → bytes for the PTY.
pub fn keystroke_bytes(
    key: &str,
    key_char: Option<&str>,
    ctrl: bool,
    alt: bool,
    shift: bool,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    let arrow = |c: char| {
        if app_cursor {
            format!("\x1bO{c}")
        } else {
            format!("\x1b[{c}")
        }
        .into_bytes()
    };
    let special: Option<Vec<u8>> = match key {
        "enter" => Some(b"\r".to_vec()),
        "backspace" => Some(if ctrl { vec![0x08] } else { vec![0x7f] }),
        "tab" if shift => Some(b"\x1b[Z".to_vec()),
        "tab" => Some(b"\t".to_vec()),
        "escape" => Some(b"\x1b".to_vec()),
        "up" => Some(arrow('A')),
        "down" => Some(arrow('B')),
        "right" => Some(arrow('C')),
        "left" => Some(arrow('D')),
        "home" => Some(arrow('H')),
        "end" => Some(arrow('F')),
        "pageup" => Some(b"\x1b[5~".to_vec()),
        "pagedown" => Some(b"\x1b[6~".to_vec()),
        "delete" => Some(b"\x1b[3~".to_vec()),
        "insert" => Some(b"\x1b[2~".to_vec()),
        "space" if ctrl => Some(vec![0]),
        _ => None,
    };
    if let Some(mut b) = special {
        if alt {
            b.insert(0, 0x1b);
        }
        return Some(b);
    }
    if ctrl && key.len() == 1 {
        let c = key.as_bytes()[0].to_ascii_lowercase();
        let b = match c {
            b'a'..=b'z' => c & 0x1f,
            b'[' => 0x1b,
            b'\\' => 0x1c,
            b']' => 0x1d,
            _ => return None,
        };
        return Some(if alt { vec![0x1b, b] } else { vec![b] });
    }
    let text = key_char
        .map(str::to_owned)
        .or_else(|| (key == "space").then(|| " ".into()))?;
    let mut b = text.into_bytes();
    if alt {
        b.insert(0, 0x1b);
    }
    Some(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_keep_the_alternate_screen_across_a_replay() {
        let mut live = TermModel::new(20, 5);
        live.feed(b"$ ls\r\nfoo\r\n\x1b[?1049h\x1b[H\x1b[2Jvim-ish");
        assert!(live.mode().contains(TermMode::ALT_SCREEN));
        assert!(live.text().starts_with("vim-ish"));
    }

    #[test]
    fn should_map_ctrl_c_and_app_cursor_arrows() {
        assert_eq!(
            keystroke_bytes("c", None, true, false, false, false),
            Some(vec![3])
        );
        assert_eq!(
            keystroke_bytes("up", None, false, false, false, true),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            keystroke_bytes("a", Some("a"), false, false, false, false),
            Some(b"a".to_vec())
        );
    }
}

fn dim(c: Rgb) -> Rgb {
    let d = |v: u8| (v as u16 * 2 / 3) as u8;
    Rgb {
        r: d(c.r),
        g: d(c.g),
        b: d(c.b),
    }
}
