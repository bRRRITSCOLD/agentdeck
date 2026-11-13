//! One attached pane's terminal model: a `Term` the daemon's snapshot and live bytes are fed
//! into (spec §5), and the [`Frame`] it reads as, which the renderer paints.
//!
//! Replies the `Term` generates (DA, cursor position, colour queries) are dropped: only the
//! daemon's `Term` answers queries.

use alacritty_terminal::Term;
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{CursorShape, Processor};
use gpui::Hsla;

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

/// What a terminal shows now, ready to paint.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// The visible lines, top to bottom.
    pub lines: Vec<FrameLine>,
    /// A cursor to draw over its cell; `None` when hidden, scrolled away or a filled block.
    pub cursor: Option<Cursor>,
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

    /// Scrolls the view `lines` into the scrollback (positive) or back towards the bottom.
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    /// Scrolls the view back to the bottom, where output appears.
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
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
            .then(|| (cursor_line as usize, content.cursor.point.column.0));
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
        Frame { lines, cursor }
    }

    /// The visible text, one string per line with trailing blanks trimmed.
    #[cfg(test)]
    pub fn text(&self) -> Vec<String> {
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
}
