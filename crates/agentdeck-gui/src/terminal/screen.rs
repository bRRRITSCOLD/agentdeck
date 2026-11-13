//! One attached pane's terminal model: a `Term` the daemon's snapshot and live bytes are fed
//! into (spec §5), and the [`Frame`] it reads as, which the renderer paints.
//!
//! Replies the `Term` generates (DA, cursor position, colour queries) are dropped: only the
//! daemon's `Term` answers queries.

use alacritty_terminal::Term;
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, TermMode, viewport_to_point};
use alacritty_terminal::vte::ansi::{CursorShape, Processor};
use gpui::Hsla;

use super::input::{MouseProtocol, Tracking};
use super::palette::{Palette, dim};

/// A terminal's size in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
}

/// How a run of cells is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// The text colour.
    pub color: Hsla,
    /// SGR 1.
    pub bold: bool,
    /// SGR 3.
    pub italic: bool,
    /// Any underline style.
    pub underline: bool,
    /// SGR 9.
    pub strikethrough: bool,
}

/// Neighbouring cells on one line drawn alike.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// The first cell's column.
    pub col: usize,
    /// How many cells it covers: one per character, two for a wide one.
    pub cells: usize,
    /// The characters, combining marks included.
    pub text: String,
    /// How they are drawn.
    pub style: Style,
}

/// Neighbouring cells on one line sharing a background other than the palette's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Background {
    /// The first cell's column.
    pub col: usize,
    /// How many cells it covers.
    pub cells: usize,
    /// Its colour.
    pub color: Hsla,
}

/// One visible line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FrameLine {
    /// Backgrounds, painted first.
    pub backgrounds: Vec<Background>,
    /// Text over them. Blank cells with nothing to draw make no run.
    pub runs: Vec<Run>,
}

/// A cursor drawn over its cell. A filled block isn't one: it is the cell's background,
/// with the cell's text in the palette background.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cursor {
    /// The visible line.
    pub line: usize,
    /// The column.
    pub col: usize,
    /// One, or two over a wide character.
    pub cells: usize,
    /// `Beam`, `Underline` or `HollowBlock`.
    pub shape: CursorShape,
    /// Its colour.
    pub color: Hsla,
}

/// Text an input method is composing, drawn underlined over the cells from the cursor on.
#[derive(Debug, Clone, PartialEq)]
pub struct Preedit {
    /// The visible line.
    pub line: usize,
    /// The column it starts at.
    pub col: usize,
    pub text: String,
    pub color: Hsla,
    /// What it covers the cells with.
    pub background: Hsla,
}

/// What a terminal shows now, ready to paint.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// The visible lines, top to bottom.
    pub lines: Vec<FrameLine>,
    /// A cursor to draw over its cell; `None` when hidden, scrolled away or a filled block.
    pub cursor: Option<Cursor>,
    /// What an input method is composing, if anything.
    pub preedit: Option<Preedit>,
}

/// A spot under the pointer: a visible cell, and which half of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// The visible line, from the top.
    pub line: usize,
    /// The column.
    pub col: usize,
    /// The cell's left or right half.
    pub side: Side,
}

/// What a press selects, by its click count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    /// One click: the cells dragged across.
    Cells,
    /// A double click: whole words.
    Words,
    /// A triple click: whole lines.
    Lines,
}

impl SelectionKind {
    /// The kind a press of `clicks` starts.
    pub fn from_clicks(clicks: usize) -> Self {
        match clicks {
            0 | 1 => SelectionKind::Cells,
            2 => SelectionKind::Words,
            _ => SelectionKind::Lines,
        }
    }
}

/// A terminal model, fed by the daemon.
pub struct Screen {
    term: Term<VoidListener>,
    parser: Processor,
}

impl Screen {
    /// A blank terminal of `size`.
    pub fn new(size: GridSize) -> Self {
        Screen {
            term: Term::new(Config::default(), &term_size(size), VoidListener),
            parser: Processor::new(),
        }
    }

    /// Feeds bytes from the daemon: a snapshot or live output.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Its size now.
    pub fn size(&self) -> GridSize {
        GridSize {
            cols: self.term.columns() as u16,
            rows: self.term.screen_lines() as u16,
        }
    }

    /// Resizes it; the view stays at the bottom.
    pub fn resize(&mut self, size: GridSize) {
        if size != self.size() {
            self.term.resize(term_size(size));
        }
    }

    /// Whether the cursor keys send application sequences (DECCKM).
    pub fn app_cursor(&self) -> bool {
        self.term.mode().contains(TermMode::APP_CURSOR)
    }

    /// Whether the alternate screen, which has no scrollback, is showing.
    pub fn alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// Whether the program asked for bracketed paste (DECSET 2004).
    pub fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// How the program asked to hear of the mouse (DECSET 1000 / 1002 / 1003, and 1006 for
    /// SGR); `None` when it didn't.
    pub fn mouse_protocol(&self) -> Option<MouseProtocol> {
        let mode = self.term.mode();
        let tracking = if mode.contains(TermMode::MOUSE_MOTION) {
            Tracking::Motion
        } else if mode.contains(TermMode::MOUSE_DRAG) {
            Tracking::Drags
        } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            Tracking::Clicks
        } else {
            return None;
        };
        Some(MouseProtocol {
            tracking,
            sgr: mode.contains(TermMode::SGR_MOUSE),
        })
    }

    /// Whether the wheel sends arrow keys: on the alternate screen, unless the program
    /// turned that off (DECSET 1007, on by default).
    pub fn alternate_scroll(&self) -> bool {
        self.term
            .mode()
            .contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
    }

    /// Starts a selection of `kind` at `at`, replacing any other.
    pub fn select(&mut self, kind: SelectionKind, at: Hit) {
        let ty = match kind {
            SelectionKind::Cells => SelectionType::Simple,
            SelectionKind::Words => SelectionType::Semantic,
            SelectionKind::Lines => SelectionType::Lines,
        };
        self.term.selection = Some(Selection::new(ty, self.point(at), at.side));
    }

    /// Moves the selection's moving end to `to`; nothing without a selection.
    pub fn extend_selection(&mut self, to: Hit) {
        let point = self.point(to);
        if let Some(selection) = &mut self.term.selection {
            selection.update(point, to.side);
        }
    }

    /// Whether there is a selection to extend.
    pub fn has_selection(&self) -> bool {
        self.term.selection.is_some()
    }

    /// The selected text; `None` when nothing is selected.
    pub fn selected_text(&self) -> Option<String> {
        self.term
            .selection_to_string()
            .filter(|text| !text.is_empty())
    }

    /// The grid point a visible cell shows, scrollback included.
    fn point(&self, at: Hit) -> Point {
        let offset = self.term.grid().display_offset();
        viewport_to_point(offset, Point::new(at.line, Column(at.col)))
    }

    /// Scrolls the view `lines` into the scrollback (positive) or back towards the bottom.
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    /// Scrolls the view back to the bottom, where output appears.
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// The visible cell the cursor is in, drawn or hidden; `None` once scrolled away from it.
    pub fn cursor_cell(&self) -> Option<(usize, usize)> {
        let grid = self.term.grid();
        let point = grid.cursor.point;
        let line = point.line.0 + grid.display_offset() as i32;
        let visible = usize::try_from(line)
            .ok()
            .filter(|&line| line < self.term.screen_lines())?;
        Some((visible, point.column.0))
    }

    /// `text` an input method is composing, placed at the cursor in `palette`; `None` for no
    /// text, or once scrolled away from the cursor.
    pub fn preedit(&self, text: &str, palette: &Palette) -> Option<Preedit> {
        if text.is_empty() {
            return None;
        }
        let (line, col) = self.cursor_cell()?;
        Some(Preedit {
            line,
            col,
            text: text.to_owned(),
            color: palette.foreground,
            background: palette.background,
        })
    }

    /// What it shows now, in `palette`; a `focused` terminal draws its cursor filled.
    pub fn frame(&self, palette: &Palette, focused: bool) -> Frame {
        let content = self.term.renderable_content();
        let offset = content.display_offset as i32;
        let mut lines = vec![FrameLine::default(); self.term.screen_lines()];

        let cursor_shape = match content.cursor.shape {
            CursorShape::Block if !focused => CursorShape::HollowBlock,
            shape => shape,
        };
        let cursor_line = content.cursor.point.line.0 + offset;
        let cursor_at = (cursor_shape != CursorShape::Hidden && cursor_line >= 0)
            .then_some((cursor_line as usize, content.cursor.point.column.0));
        let mut cursor = None;

        let mut builder = LineBuilder::default();
        let mut current = None;
        for indexed in content.display_iter {
            let Point { line, column } = indexed.point;
            let visible = (line.0 + offset) as usize;
            if current != Some(visible) {
                if let Some(done) = current {
                    lines[done] = builder.finish();
                }
                current = Some(visible);
            }
            let cell: &Cell = indexed.cell;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let cells = if cell.flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            let (mut fg, mut bg) = colors(cell, palette, content.colors);
            if content.selection.is_some_and(|s| s.contains(indexed.point)) {
                (fg, bg) = (palette.selection_foreground, palette.selection);
            }
            if cursor_at == Some((visible, column.0)) {
                if cursor_shape == CursorShape::Block {
                    (fg, bg) = (palette.background, palette.foreground);
                } else {
                    cursor = Some(Cursor {
                        line: visible,
                        col: column.0,
                        cells,
                        shape: cursor_shape,
                        color: palette.foreground,
                    });
                }
            }
            builder.push(column.0, cells, cell, fg, bg, palette.background);
        }
        if let Some(done) = current {
            lines[done] = builder.finish();
        }
        Frame {
            lines,
            cursor,
            preedit: None,
        }
    }

    /// The visible text, one string per line with trailing blanks trimmed.
    #[cfg(test)]
    pub fn text(&self) -> Vec<String> {
        use alacritty_terminal::index::{Column, Line};

        let grid = self.term.grid();
        (0..self.term.screen_lines() as i32)
            .map(|line| {
                let row = &grid[Line(line - grid.display_offset() as i32)];
                let text: String = (0..self.term.columns())
                    .map(|col| &row[Column(col)])
                    .filter(|cell| !cell.flags.contains(Flags::WIDE_CHAR_SPACER))
                    .map(|cell| cell.c)
                    .collect();
                text.trim_end().to_owned()
            })
            .collect()
    }
}

fn term_size(size: GridSize) -> TermSize {
    TermSize::new(usize::from(size.cols.max(1)), usize::from(size.rows.max(1)))
}

/// A cell's text and background colours after `INVERSE`, `DIM` and `HIDDEN`.
fn colors(
    cell: &Cell,
    palette: &Palette,
    set: &alacritty_terminal::term::color::Colors,
) -> (Hsla, Hsla) {
    let mut fg = palette.resolve(cell.fg, set);
    let mut bg = palette.resolve(cell.bg, set);
    if cell.flags.contains(Flags::INVERSE) {
        (fg, bg) = (bg, fg);
    }
    if cell.flags.contains(Flags::DIM) {
        fg = dim(fg);
    }
    if cell.flags.contains(Flags::HIDDEN) {
        fg = bg;
    }
    (fg, bg)
}

/// Gathers one line's cells into runs and backgrounds.
#[derive(Default)]
struct LineBuilder {
    line: FrameLine,
}

impl LineBuilder {
    fn push(&mut self, col: usize, cells: usize, cell: &Cell, fg: Hsla, bg: Hsla, base: Hsla) {
        if bg != base {
            match self.line.backgrounds.last_mut() {
                Some(last) if last.color == bg && last.col + last.cells == col => {
                    last.cells += cells;
                }
                _ => self.line.backgrounds.push(Background {
                    col,
                    cells,
                    color: bg,
                }),
            }
        }

        let style = Style {
            color: fg,
            bold: cell.flags.contains(Flags::BOLD),
            italic: cell.flags.contains(Flags::ITALIC),
            underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
            strikethrough: cell.flags.contains(Flags::STRIKEOUT),
        };
        let blank = cell.c == ' ' || cell.c == '\0' || cell.c == '\t';
        if blank && !style.underline && !style.strikethrough {
            return;
        }
        let mut text = String::new();
        text.push(if blank { ' ' } else { cell.c });
        text.extend(cell.zerowidth().into_iter().flatten());

        // A wide character is a run of its own, so the renderer can give it two cells; so
        // is whatever follows one.
        let extends = |last: &Run| {
            last.style == style && last.col + last.cells == col && cells == 1 && {
                let wide = last.text.chars().count() == 1 && last.cells == 2;
                !wide
            }
        };
        match self.line.runs.last_mut() {
            Some(last) if extends(last) => {
                last.cells += 1;
                last.text.push_str(&text);
            }
            _ => self.line.runs.push(Run {
                col,
                cells,
                text,
                style,
            }),
        }
    }

    fn finish(&mut self) -> FrameLine {
        std::mem::take(&mut self.line)
    }
}

#[cfg(test)]
mod tests {
    use brrritscold_core_gpui::Mode;

    use super::*;

    const SIZE: GridSize = GridSize { cols: 20, rows: 4 };

    fn screen(bytes: &[u8]) -> Screen {
        let mut screen = Screen::new(SIZE);
        screen.feed(bytes);
        screen
    }

    fn palette() -> Palette {
        Palette::new(Mode::Dark)
    }

    fn frame(bytes: &[u8]) -> Frame {
        screen(bytes).frame(&palette(), true)
    }

    fn red() -> Hsla {
        gpui::rgb(0xf87171).into()
    }

    mod feed {
        use super::*;

        #[test]
        fn should_show_what_was_written() {
            assert_eq!(screen(b"hello\r\nworld").text(), ["hello", "world", "", ""]);
        }

        #[test]
        fn should_start_over_on_a_full_reset() {
            assert_eq!(screen(b"old\x1bcnew").text(), ["new", "", "", ""]);
        }
    }

    mod resize {
        use super::*;

        #[test]
        fn should_take_the_new_size() {
            let mut screen = screen(b"");
            let size = GridSize { cols: 80, rows: 24 };

            screen.resize(size);

            assert_eq!(screen.size(), size);
        }
    }

    mod scroll {
        use super::*;

        #[test]
        fn should_show_the_scrollback_and_come_back() {
            let mut screen = screen(b"1\r\n2\r\n3\r\n4\r\n5\r\n6");

            screen.scroll(2);
            assert_eq!(screen.text(), ["1", "2", "3", "4"]);

            screen.scroll_to_bottom();
            assert_eq!(screen.text(), ["3", "4", "5", "6"]);
        }
    }

    mod select {
        use super::*;

        fn hit(line: usize, col: usize, side: Side) -> Hit {
            Hit { line, col, side }
        }

        fn selected(bytes: &[u8], kind: SelectionKind, from: Hit, to: Hit) -> Option<String> {
            let mut screen = screen(bytes);
            screen.select(kind, from);
            screen.extend_selection(to);
            screen.selected_text()
        }

        #[test]
        fn should_select_the_cells_dragged_across() {
            let text = selected(
                b"hello world",
                SelectionKind::Cells,
                hit(0, 0, Side::Left),
                hit(0, 4, Side::Right),
            );

            assert_eq!(text.as_deref(), Some("hello"));
        }

        #[test]
        fn should_select_backwards_too() {
            let text = selected(
                b"hello world",
                SelectionKind::Cells,
                hit(0, 10, Side::Right),
                hit(0, 6, Side::Left),
            );

            assert_eq!(text.as_deref(), Some("world"));
        }

        #[test]
        fn should_join_lines_with_a_newline() {
            let text = selected(
                b"one\r\ntwo",
                SelectionKind::Cells,
                hit(0, 0, Side::Left),
                hit(1, 2, Side::Right),
            );

            assert_eq!(text.as_deref(), Some("one\ntwo"));
        }

        #[test]
        fn should_select_nothing_for_a_click_without_a_drag() {
            let at = hit(0, 1, Side::Left);

            assert_eq!(selected(b"hello", SelectionKind::Cells, at, at), None);
        }

        #[test]
        fn should_select_whole_words() {
            let at = hit(0, 8, Side::Left);

            let text = selected(b"cd ~/src/app && ls", SelectionKind::Words, at, at);

            assert_eq!(text.as_deref(), Some("~/src/app"));
        }

        #[test]
        fn should_select_whole_lines() {
            let text = selected(
                b"one\r\ntwo\r\nthree",
                SelectionKind::Lines,
                hit(0, 2, Side::Left),
                hit(1, 0, Side::Left),
            );

            assert_eq!(text.as_deref(), Some("one\ntwo\n"));
        }

        #[test]
        fn should_select_what_the_scrollback_shows() {
            let mut screen = screen(b"1\r\n2\r\n3\r\n4\r\n5\r\n6");
            screen.scroll(2);

            screen.select(SelectionKind::Lines, hit(0, 0, Side::Left));

            assert_eq!(screen.selected_text().as_deref(), Some("1\n"));
        }

        #[test]
        fn should_extend_nothing_without_a_selection() {
            let mut screen = screen(b"hello");

            screen.extend_selection(hit(0, 4, Side::Right));

            assert!(!screen.has_selection());
            assert_eq!(screen.selected_text(), None);
        }

        #[test]
        fn should_drop_the_selection_when_the_screen_is_cleared() {
            let mut screen = screen(b"hello");
            screen.select(SelectionKind::Lines, hit(0, 0, Side::Left));

            screen.feed(b"\x1b[H\x1b[2J");

            assert_eq!(screen.selected_text(), None);
        }

        #[test]
        fn should_keep_the_selection_on_its_text_as_output_scrolls_it() {
            let mut screen = screen(b"1\r\n2\r\n3\r\n4");
            screen.select(SelectionKind::Lines, hit(0, 0, Side::Left));

            screen.feed(b"\r\n5");

            assert_eq!(screen.selected_text().as_deref(), Some("1\n"));
        }
    }

    mod from_clicks {
        use super::*;

        #[test]
        fn should_select_cells_then_words_then_lines() {
            let kinds: Vec<_> = (1..=4).map(SelectionKind::from_clicks).collect();

            assert_eq!(
                kinds,
                [
                    SelectionKind::Cells,
                    SelectionKind::Words,
                    SelectionKind::Lines,
                    SelectionKind::Lines
                ]
            );
        }
    }

    mod bracketed_paste {
        use super::*;

        #[test]
        fn should_follow_the_programs_request() {
            let mut screen = screen(b"");
            assert!(!screen.bracketed_paste());

            screen.feed(b"\x1b[?2004h");
            assert!(screen.bracketed_paste());

            screen.feed(b"\x1b[?2004l");
            assert!(!screen.bracketed_paste());
        }
    }

    mod frame {
        use super::*;

        fn style(color: Hsla) -> Style {
            Style {
                color,
                bold: false,
                italic: false,
                underline: false,
                strikethrough: false,
            }
        }

        #[test]
        fn should_split_a_line_into_runs_by_style() {
            let frame = frame(b"ab\x1b[31mcd\x1b[0m\x1b[?25l");

            assert_eq!(
                frame.lines[0].runs,
                [
                    Run {
                        col: 0,
                        cells: 2,
                        text: "ab".into(),
                        style: style(palette().foreground)
                    },
                    Run {
                        col: 2,
                        cells: 2,
                        text: "cd".into(),
                        style: style(red())
                    },
                ]
            );
        }

        #[test]
        fn should_make_no_run_for_blank_cells() {
            let frame = frame(b"a   b\x1b[?25l");

            let cols: Vec<_> = frame.lines[0].runs.iter().map(|r| r.col).collect();
            assert_eq!(cols, [0, 4]);
        }

        #[test]
        fn should_carry_bold_italic_underline_and_strikethrough() {
            let frame = frame(b"\x1b[1;3;4;9mx\x1b[?25l");

            let style = frame.lines[0].runs[0].style;
            assert!(style.bold && style.italic && style.underline && style.strikethrough);
        }

        #[test]
        fn should_merge_neighbouring_backgrounds_of_one_colour() {
            let frame = frame(b"\x1b[41mab \x1b[0mc\x1b[?25l");

            assert_eq!(
                frame.lines[0].backgrounds,
                [Background {
                    col: 0,
                    cells: 3,
                    color: red()
                }]
            );
        }

        #[test]
        fn should_swap_colours_for_inverse() {
            let frame = frame(b"\x1b[7mx\x1b[?25l");
            let line = &frame.lines[0];

            assert_eq!(line.runs[0].style.color, palette().background);
            assert_eq!(line.backgrounds[0].color, palette().foreground);
        }

        #[test]
        fn should_give_a_wide_character_two_cells_and_a_run_of_its_own() {
            let frame = frame("a漢b\x1b[?25l".as_bytes());

            let runs: Vec<_> = frame.lines[0]
                .runs
                .iter()
                .map(|r| (r.col, r.cells, r.text.as_str()))
                .collect();
            assert_eq!(runs, [(0, 1, "a"), (1, 2, "漢"), (3, 1, "b")]);
        }

        #[test]
        fn should_keep_a_combining_mark_with_its_character() {
            let frame = frame("e\u{301}x\x1b[?25l".as_bytes());

            assert_eq!(frame.lines[0].runs[0].text, "e\u{301}x");
            assert_eq!(frame.lines[0].runs[0].cells, 2);
        }

        mod selection {
            use super::*;

            #[test]
            fn should_draw_selected_cells_in_the_selection_colours() {
                let mut screen = screen(b"ab cd\x1b[?25l");
                screen.select(
                    SelectionKind::Cells,
                    Hit {
                        line: 0,
                        col: 0,
                        side: Side::Left,
                    },
                );
                screen.extend_selection(Hit {
                    line: 0,
                    col: 2,
                    side: Side::Right,
                });

                let frame = screen.frame(&palette(), true);
                let line = &frame.lines[0];

                assert_eq!(
                    line.backgrounds,
                    [Background {
                        col: 0,
                        cells: 3,
                        color: palette().selection
                    }]
                );
                assert_eq!(line.runs[0].text, "ab");
                assert_eq!(line.runs[0].style.color, palette().selection_foreground);
                assert_eq!(line.runs[1].style.color, palette().foreground);
            }

            #[test]
            fn should_move_the_selection_with_its_line_when_scrolled() {
                let mut screen = screen(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\x1b[?25l");
                screen.select(
                    SelectionKind::Lines,
                    Hit {
                        line: 0,
                        col: 0,
                        side: Side::Left,
                    },
                );

                screen.scroll(1);
                let frame = screen.frame(&palette(), true);

                let selected: Vec<_> = frame
                    .lines
                    .iter()
                    .map(|line| !line.backgrounds.is_empty())
                    .collect();
                assert_eq!(selected, [false, true, false, false]);
            }
        }

        mod cursor {
            use super::*;

            #[test]
            fn should_fill_the_cell_under_a_focused_block_cursor() {
                let frame = frame(b"ab\x1b[1D");
                let line = &frame.lines[0];

                assert_eq!(frame.cursor, None);
                assert_eq!(
                    line.backgrounds,
                    [Background {
                        col: 1,
                        cells: 1,
                        color: palette().foreground
                    }]
                );
                assert_eq!(line.runs[1].style.color, palette().background);
            }

            #[test]
            fn should_outline_the_block_when_unfocused() {
                let frame = screen(b"ab").frame(&palette(), false);

                assert_eq!(
                    frame.cursor,
                    Some(Cursor {
                        line: 0,
                        col: 2,
                        cells: 1,
                        shape: CursorShape::HollowBlock,
                        color: palette().foreground
                    })
                );
            }

            #[test]
            fn should_draw_a_beam_where_the_program_asks_for_one() {
                let frame = frame(b"\x1b[6 q");

                assert_eq!(frame.cursor.map(|c| c.shape), Some(CursorShape::Beam));
            }

            #[test]
            fn should_draw_no_cursor_when_the_program_hides_it() {
                let frame = frame(b"ab\x1b[?25l");

                assert_eq!(frame.cursor, None);
                assert!(frame.lines[0].backgrounds.is_empty());
            }

            #[test]
            fn should_draw_no_cursor_once_scrolled_away_from_it() {
                let mut screen = screen(b"1\r\n2\r\n3\r\n4\r\n5\r\n6");
                screen.scroll(4);

                let frame = screen.frame(&palette(), false);

                assert_eq!(frame.cursor, None);
            }
        }
    }

    mod preedit {
        use super::*;

        #[test]
        fn should_sit_at_the_cursor_in_the_foreground_over_the_background() {
            let preedit = screen(b"ab\r\ncd").preedit("にほ", &palette());

            assert_eq!(
                preedit,
                Some(Preedit {
                    line: 1,
                    col: 2,
                    text: "にほ".into(),
                    color: palette().foreground,
                    background: palette().background,
                })
            );
        }

        #[test]
        fn should_sit_at_the_cursor_the_program_hid() {
            let preedit = screen(b"ab\x1b[?25l").preedit("é", &palette());

            assert_eq!(preedit.map(|p| (p.line, p.col)), Some((0, 2)));
        }

        #[test]
        fn should_be_none_without_text() {
            assert_eq!(screen(b"ab").preedit("", &palette()), None);
        }

        #[test]
        fn should_be_none_once_scrolled_away_from_the_cursor() {
            let mut screen = screen(b"1\r\n2\r\n3\r\n4\r\n5\r\n6");
            screen.scroll(4);

            assert_eq!(screen.preedit("é", &palette()), None);
            assert_eq!(screen.cursor_cell(), None);
        }
    }
}
