//! The attach snapshot (spec §5): a pane's `Term` re-encoded as the escape sequences that
//! rebuild it in a fresh `Term` of the same size.
//!
//! It carries the scrollback and the screen with their SGR runs, soft wraps, wide and
//! zero-width characters and hyperlinks; the primary screen under an alternate screen; the
//! cursor (position, pen, a pending wrap, its saved copy and its style); and the terminal
//! modes. It does not carry the scroll region, tab stops or the active character set, which
//! `alacritty_terminal` keeps private: a program that relies on one it set before the attach
//! draws the attaching client's copy differently until it sets it again.
//!
//! [`report_cwd`] then ends it with the shell's last OSC 7, which a `Term` doesn't keep, so a
//! client resolving file paths has the shell's directory from the attach on.

use std::fmt::Write as _;

use alacritty_terminal::Term;
use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags, Hyperlink};
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};

/// Modes the snapshot restores: each flag with the sequences that set and reset it. A mode
/// is written only when it differs from a fresh `Term`'s.
const MODES: &[(TermMode, &str, &str)] = &[
    (TermMode::SHOW_CURSOR, "\x1b[?25h", "\x1b[?25l"),
    (TermMode::APP_CURSOR, "\x1b[?1h", "\x1b[?1l"),
    (TermMode::APP_KEYPAD, "\x1b=", "\x1b>"),
    (TermMode::MOUSE_REPORT_CLICK, "\x1b[?1000h", "\x1b[?1000l"),
    (TermMode::MOUSE_DRAG, "\x1b[?1002h", "\x1b[?1002l"),
    (TermMode::MOUSE_MOTION, "\x1b[?1003h", "\x1b[?1003l"),
    (TermMode::FOCUS_IN_OUT, "\x1b[?1004h", "\x1b[?1004l"),
    (TermMode::UTF8_MOUSE, "\x1b[?1005h", "\x1b[?1005l"),
    (TermMode::SGR_MOUSE, "\x1b[?1006h", "\x1b[?1006l"),
    (TermMode::ALTERNATE_SCROLL, "\x1b[?1007h", "\x1b[?1007l"),
    (TermMode::URGENCY_HINTS, "\x1b[?1042h", "\x1b[?1042l"),
    (TermMode::BRACKETED_PASTE, "\x1b[?2004h", "\x1b[?2004l"),
    (TermMode::LINE_WRAP, "\x1b[?7h", "\x1b[?7l"),
    (TermMode::INSERT, "\x1b[4h", "\x1b[4l"),
    (TermMode::LINE_FEED_NEW_LINE, "\x1b[20h", "\x1b[20l"),
    // Last: setting origin mode homes the cursor, which is placed after the modes.
    (TermMode::ORIGIN, "\x1b[?6h", "\x1b[?6l"),
];

/// Encodes `term` as the bytes that rebuild it in a fresh `Term` of the same size. Needs
/// `&mut` only to read the primary screen under an alternate screen, which `Term` keeps
/// private: it swaps the screens, reads, and swaps back, leaving `term` as it found it.
pub fn encode<T>(term: &mut Term<T>) -> Vec<u8> {
    // A full reset first, so a client may also feed this into a `Term` it already used.
    let mut out = String::from("\x1bc");
    if term.mode().contains(TermMode::ALT_SCREEN) {
        let alternate = term.grid().clone();
        term.swap_alt();
        write_grid(&mut out, term.grid(), true);
        // Entering the alternate screen saves this cursor; leaving it restores it.
        write_cursor(&mut out, term.grid());
        term.swap_alt();
        *term.grid_mut() = alternate;
        out.push_str("\x1b[?1049h\x1b[0m\x1b[H\x1b[2J");
        write_grid(&mut out, term.grid(), false);
    } else {
        write_grid(&mut out, term.grid(), true);
    }
    write_modes(&mut out, *term.mode());
    write_cursor(&mut out, term.grid());
    let style = term.cursor_style();
    if style != Config::default().default_cursor_style {
        let code = match (style.shape, style.blinking) {
            (CursorShape::Underline, true) => 3,
            (CursorShape::Underline, false) => 4,
            (CursorShape::Beam, true) => 5,
            (CursorShape::Beam, false) => 6,
            (_, true) => 1,
            (_, false) => 2,
        };
        let _ = write!(out, "\x1b[{code} q");
    }
    out.into_bytes()
}

/// Ends `snapshot` with the working directory the shell last reported, `cwd`, as the OSC 7
/// it sent (`ESC ] 7 ; <uri> ST`), so a client that reads OSC 7 from the pane's output knows it
/// from the attach on. A `Term` ignores OSC 7, so the screen it rebuilds is the same.
pub fn report_cwd(snapshot: &mut Vec<u8>, cwd: Option<&str>) {
    if let Some(uri) = cwd {
        snapshot.extend_from_slice(b"\x1b]7;");
        snapshot.extend_from_slice(uri.as_bytes());
        snapshot.extend_from_slice(b"\x1b\\");
    }
}

/// The grid's rows, top to bottom, written from the top-left as a program would have written
/// them: with history, rows past the screen's height scroll up into the new `Term`'s
/// scrollback. A soft-wrapped row is written in full with no line break, so the next row
/// wraps onto it again.
fn write_grid(out: &mut String, grid: &Grid<Cell>, with_history: bool) {
    let top = if with_history {
        -(grid.history_size() as i32)
    } else {
        0
    };
    let bottom = grid.screen_lines() as i32;
    let mut pen = Pen::default();
    for line in top..bottom {
        let row = &grid[Line(line)];
        let columns = grid.columns();
        let wrapped = row[Column(columns - 1)].flags.contains(Flags::WRAPLINE);
        let end = if wrapped {
            columns
        } else {
            (0..columns)
                .rev()
                .find(|&column| !is_blank(&row[Column(column)]))
                .map_or(0, |column| column + 1)
        };
        for column in 0..end {
            let cell = &row[Column(column)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            pen.write_cell(out, cell);
        }
        if !wrapped && line + 1 < bottom {
            pen.reset(out);
            out.push_str("\r\n");
        }
    }
    pen.reset(out);
}

/// A cell that a fresh row already holds, so trailing ones need not be written.
fn is_blank(cell: &Cell) -> bool {
    let structural = Flags::WRAPLINE | Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER;
    cell.c == ' '
        && cell.fg == Color::Named(NamedColor::Foreground)
        && cell.bg == Color::Named(NamedColor::Background)
        && (cell.flags - structural).is_empty()
        && cell.extra.is_none()
}

fn write_modes(out: &mut String, mode: TermMode) {
    let fresh = TermMode::default();
    for &(flag, set, reset) in MODES {
        if mode.contains(flag) != fresh.contains(flag) {
            out.push_str(if mode.contains(flag) { set } else { reset });
        }
    }
}

/// The saved cursor (through DECSC), then the cursor: its position, a pending wrap, and its
/// pen, the attributes the program's next character gets.
fn write_cursor(out: &mut String, grid: &Grid<Cell>) {
    let saved = &grid.saved_cursor;
    goto(out, saved.point.line.0, saved.point.column.0);
    Pen::default().write_template(out, &saved.template);
    out.push_str("\x1b7");

    let cursor = &grid.cursor;
    let line = cursor.point.line.0;
    let mut column = cursor.point.column.0;
    if cursor.input_needs_wrap {
        // Only writing the last column leaves a wrap pending, so write it again.
        let row = &grid[cursor.point.line];
        if row[Column(column)].flags.contains(Flags::WIDE_CHAR_SPACER) && column > 0 {
            column -= 1;
        }
        goto(out, line, column);
        let mut pen = Pen::default();
        pen.write_cell(out, &row[Column(column)]);
        pen.reset(out);
    } else {
        goto(out, line, column);
    }
    Pen::default().write_template(out, &cursor.template);
}

fn goto(out: &mut String, line: i32, column: usize) {
    let _ = write!(out, "\x1b[{};{}H", line + 1, column + 1);
}

/// The SGR attributes and hyperlink last written, so a run of like cells shares one sequence.
#[derive(Default)]
struct Pen {
    sgr: String,
    hyperlink: Option<Hyperlink>,
}

impl Pen {
    fn write_cell(&mut self, out: &mut String, cell: &Cell) {
        self.set(out, cell);
        out.push(cell.c);
        if let Some(marks) = cell.zerowidth() {
            out.extend(marks);
        }
    }

    /// Sets the attributes without writing a character: the cursor's pen.
    fn write_template(&mut self, out: &mut String, template: &Cell) {
        out.push_str("\x1b[0m");
        self.set(out, template);
    }

    fn set(&mut self, out: &mut String, cell: &Cell) {
        let sgr = sgr(cell);
        if sgr != self.sgr {
            let _ = write!(out, "\x1b[0{sgr}m");
            self.sgr = sgr;
        }
        let hyperlink = cell.hyperlink();
        if hyperlink != self.hyperlink {
            match &hyperlink {
                Some(link) => {
                    let _ = write!(out, "\x1b]8;id={};{}\x1b\\", link.id(), link.uri());
                }
                None => out.push_str("\x1b]8;;\x1b\\"),
            }
            self.hyperlink = hyperlink;
        }
    }

    /// Back to no attributes and no hyperlink, so line breaks and fresh rows stay plain.
    fn reset(&mut self, out: &mut String) {
        if self.hyperlink.take().is_some() {
            out.push_str("\x1b]8;;\x1b\\");
        }
        if !self.sgr.is_empty() {
            out.push_str("\x1b[0m");
            self.sgr.clear();
        }
    }
}

/// A cell's attributes as SGR parameters, each prefixed by `;`, to follow a reset (`0`).
fn sgr(cell: &Cell) -> String {
    let mut sgr = String::new();
    for (flag, code) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::UNDERLINE, "4"),
        (Flags::DOUBLE_UNDERLINE, "4:2"),
        (Flags::UNDERCURL, "4:3"),
        (Flags::DOTTED_UNDERLINE, "4:4"),
        (Flags::DASHED_UNDERLINE, "4:5"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKEOUT, "9"),
    ] {
        if cell.flags.contains(flag) {
            sgr.push(';');
            sgr.push_str(code);
        }
    }
    color(&mut sgr, cell.fg, 30, 90, 38);
    color(&mut sgr, cell.bg, 40, 100, 48);
    match cell.underline_color() {
        Some(Color::Indexed(index)) => {
            let _ = write!(sgr, ";58;5;{index}");
        }
        Some(Color::Spec(rgb)) => {
            let _ = write!(sgr, ";58;2;{};{};{}", rgb.r, rgb.g, rgb.b);
        }
        // SGR 58 can't name a colour, so no program can have set one.
        Some(Color::Named(_)) | None => {}
    }
    sgr
}

/// One colour as SGR: `normal + n` for the 8 colours, `bright + n` for their bright forms,
/// `extended;5;n` for the 256-colour palette and `extended;2;r;g;b` for true colour. The
/// defaults (and the named colours no SGR can set) write nothing.
fn color(sgr: &mut String, color: Color, normal: u8, bright: u8, extended: u8) {
    let _ = match color {
        Color::Named(named) => match named as usize {
            index @ 0..8 => write!(sgr, ";{}", normal as usize + index),
            index @ 8..16 => write!(sgr, ";{}", bright as usize + index - 8),
            _ => Ok(()),
        },
        Color::Indexed(index) => write!(sgr, ";{extended};5;{index}"),
        Color::Spec(rgb) => write!(sgr, ";{extended};2;{};{};{}", rgb.r, rgb.g, rgb.b),
    };
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

    use super::*;

    const COLS: usize = 12;
    const ROWS: usize = 5;

    fn term() -> Term<VoidListener> {
        let config = Config {
            scrolling_history: 1_000,
            ..Config::default()
        };
        Term::new(config, &TermSize::new(COLS, ROWS), VoidListener)
    }

    fn feed(term: &mut Term<VoidListener>, bytes: &[u8]) {
        Processor::<StdSyncHandler>::new().advance(term, bytes);
    }

    fn term_with(bytes: &[u8]) -> Term<VoidListener> {
        let mut term = term();
        feed(&mut term, bytes);
        term
    }

    /// Every row, history first, as the cells themselves.
    fn cells(grid: &Grid<Cell>) -> Vec<Vec<Cell>> {
        let top = -(grid.history_size() as i32);
        (top..grid.screen_lines() as i32)
            .map(|line| {
                let row = &grid[Line(line)];
                (0..grid.columns())
                    .map(|c| row[Column(c)].clone())
                    .collect()
            })
            .collect()
    }

    fn assert_same_screen(original: &Term<VoidListener>, restored: &Term<VoidListener>) {
        let (a, b) = (original.grid(), restored.grid());
        assert_eq!(cells(a), cells(b), "cells");
        assert_eq!(a.cursor.point, b.cursor.point, "cursor");
        assert_eq!(a.cursor.template, b.cursor.template, "pen");
        assert_eq!(
            a.cursor.input_needs_wrap, b.cursor.input_needs_wrap,
            "pending wrap"
        );
        assert_eq!(a.saved_cursor.point, b.saved_cursor.point, "saved cursor");
        assert_eq!(
            a.saved_cursor.template, b.saved_cursor.template,
            "saved pen"
        );
        assert_eq!(original.mode(), restored.mode(), "modes");
        assert_eq!(original.cursor_style(), restored.cursor_style(), "style");
    }

    /// Encodes `bytes`' terminal, decodes the snapshot into a fresh `Term`, and checks the two
    /// match: before more output, after it (so the pen and a pending wrap carry on), and,
    /// on an alternate screen, after both leave it (so the primary screen came across).
    fn assert_round_trip(bytes: &[u8]) {
        let mut original = term_with(bytes);
        let snapshot = encode(&mut original);
        let mut restored = term_with(&snapshot);
        assert_same_screen(&original, &restored);

        for next in [b"Zz\r\nnext".as_slice(), b"\x1b[?1049l"] {
            feed(&mut original, next);
            feed(&mut restored, next);
            assert_same_screen(&original, &restored);
        }
    }

    mod report_cwd {
        use super::*;

        #[test]
        fn should_end_the_snapshot_with_the_reported_dir() {
            let mut term = term_with(b"$ ");
            let mut snapshot = encode(&mut term);

            report_cwd(&mut snapshot, Some("file://box/w/repo"));

            assert!(snapshot.ends_with(b"\x1b]7;file://box/w/repo\x1b\\"));
        }

        #[test]
        fn should_leave_the_snapshot_as_it_is_without_one() {
            let mut term = term_with(b"$ ");
            let mut snapshot = encode(&mut term);
            let before = snapshot.clone();

            report_cwd(&mut snapshot, None);

            assert_eq!(snapshot, before);
        }

        #[test]
        fn should_rebuild_the_same_screen() {
            let mut original = term_with(b"one\r\n$ ");
            let mut snapshot = encode(&mut original);

            report_cwd(&mut snapshot, Some("file://box/w"));

            assert_same_screen(&original, &term_with(&snapshot));
        }
    }

    mod encode {
        use super::*;

        #[test]
        fn should_round_trip_an_empty_terminal() {
            assert_round_trip(b"");
        }

        #[test]
        fn should_round_trip_plain_text_and_line_breaks() {
            assert_round_trip(b"hello\r\n\r\n  world");
        }

        #[test]
        fn should_round_trip_every_colour_form() {
            assert_round_trip(
                b"\x1b[31mred\x1b[92mbright\x1b[44mbg\x1b[0m\r\n\
                  \x1b[38;5;200mix\x1b[48;5;17mbg\x1b[0m\r\n\
                  \x1b[38;2;1;2;3mrgb\x1b[48;2;250;0;9mbg\x1b[0m",
            );
        }

        #[test]
        fn should_round_trip_every_attribute() {
            assert_round_trip(
                b"\x1b[1mb\x1b[2md\x1b[0;3mi\x1b[0;4mu\x1b[0;21mU\x1b[0;4:3mc\x1b[0;4:4m.\
                  \x1b[0;4:5m-\x1b[0;7mr\x1b[0;8mh\x1b[0;9ms\x1b[0m\r\n\
                  \x1b[4;58;2;9;8;7mul\x1b[58;5;3mix\x1b[0m",
            );
        }

        #[test]
        fn should_round_trip_coloured_blanks_at_the_end_of_a_row() {
            assert_round_trip(b"bar\x1b[44m   \x1b[0m\r\n\x1b[45m\x1b[K\x1b[0mx");
        }

        #[test]
        fn should_round_trip_wide_and_zero_width_characters() {
            assert_round_trip("日本e\u{301}\r\nab日本語x".as_bytes());
        }

        #[test]
        fn should_round_trip_a_wide_character_that_wrapped_early() {
            assert_round_trip("0123456789a日".as_bytes());
        }

        #[test]
        fn should_round_trip_soft_wrapped_rows() {
            assert_round_trip(b"abcdefghijklmnopqrstuvwxyz0123456789ABCD\r\nshort");
        }

        #[test]
        fn should_round_trip_the_scrollback() {
            let lines: String = (0..40).map(|i| format!("line {i}\r\n")).collect();
            let mut original = term_with(lines.as_bytes());
            assert!(original.grid().history_size() > 0);
            let restored = term_with(&encode(&mut original));
            assert_same_screen(&original, &restored);
        }

        #[test]
        fn should_round_trip_a_hyperlink() {
            assert_round_trip(
                b"see \x1b]8;;https://example.com\x07here\x1b]8;;\x07 and \
                  \x1b]8;id=x;https://b.example\x07there\x1b]8;;\x07",
            );
        }

        #[test]
        fn should_round_trip_the_alternate_screen_and_the_primary_under_it() {
            let history: String = (0..20).map(|i| format!("shell {i}\r\n")).collect();
            let bytes = format!(
                "{history}$ vi\x1b[1;33m\x1b[?1049h\x1b[0m\x1b[H\x1b[2J~\r\n~\r\n\
                 \x1b[7m file \x1b[0m\x1b[2;3H\x1b[32m"
            );
            assert_round_trip(bytes.as_bytes());
        }

        #[test]
        fn should_leave_the_terminal_as_it_was_after_reading_the_primary_screen() {
            let bytes = b"primary\x1b[?1049halt\x1b7\x1b[3;4H";
            let mut term = term_with(bytes);
            encode(&mut term);
            assert_same_screen(&term_with(bytes), &term);
            feed(&mut term, b"\x1b[?1049l");
            assert_eq!(term.grid()[Line(0)][Column(0)].c, 'p');
        }

        #[test]
        fn should_round_trip_the_modes() {
            assert_round_trip(
                b"\x1b[?1h\x1b=\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1004h\
                  \x1b[?25l\x1b[?7l\x1b[4h\x1b[20h\x1b[?1007l\x1b[?1042l",
            );
        }

        #[test]
        fn should_round_trip_origin_mode() {
            assert_round_trip(b"text\x1b[?6h\x1b[2;3H");
        }

        #[test]
        fn should_round_trip_the_cursor_and_its_pen() {
            assert_round_trip(b"abc\r\ndef\x1b[3;7H\x1b[1;4;35;46m");
        }

        #[test]
        fn should_round_trip_a_pending_wrap() {
            assert_round_trip(b"\x1b[31m0123456789AB\x1b[0m");
        }

        #[test]
        fn should_round_trip_a_pending_wrap_after_a_wide_character() {
            assert_round_trip("0123456789日".as_bytes());
        }

        #[test]
        fn should_round_trip_the_saved_cursor() {
            assert_round_trip(b"ab\x1b[2;5H\x1b[33m\x1b7\x1b[0m\x1b[4;1Hcd");
        }

        #[test]
        fn should_round_trip_the_cursor_style() {
            assert_round_trip(b"\x1b[5 q");
        }

        #[test]
        fn should_be_small_for_a_mostly_empty_screen() {
            let mut term = term_with(b"$ ");
            assert!(encode(&mut term).len() < 64);
        }
    }
}
