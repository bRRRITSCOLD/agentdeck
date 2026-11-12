//! Reads an `alacritty_terminal` screen into the rows an [`Observation`](super::Observation)
//! carries. Behind the `screen` feature, so core's default build has no terminal crate; the
//! daemon, which owns a `Term` per pane, turns it on.

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;

/// The live screen, top to bottom, one string per row with trailing spaces trimmed. It is the
/// screen the program is drawing, not a scrolled-back view of history, so text that scrolled
/// away can't match a rule.
pub fn rows<T>(term: &Term<T>) -> Vec<String> {
    let grid = term.grid();
    (0..term.screen_lines())
        .map(|line| {
            let row = &grid[Line(line as i32)];
            let mut text = String::with_capacity(term.columns());
            for column in 0..term.columns() {
                let cell = &row[Column(column)];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                text.push(cell.c);
                if let Some(marks) = cell.zerowidth() {
                    text.extend(marks);
                }
            }
            text.truncate(text.trim_end().len());
            text
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::vte::ansi::Processor;

    use super::*;

    fn term_with(bytes: &[u8]) -> Term<VoidListener> {
        let mut term = Term::new(Config::default(), &TermSize::new(20, 4), VoidListener);
        Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new().advance(&mut term, bytes);
        term
    }

    mod rows {
        use super::*;

        #[test]
        fn should_give_one_trimmed_string_per_screen_row() {
            let term = term_with(b"hello\r\n\r\n  indented  ");
            assert_eq!(rows(&term), ["hello", "", "  indented", ""]);
        }

        #[test]
        fn should_write_a_wide_glyph_once() {
            let term = term_with("日本 ok".as_bytes());
            assert_eq!(rows(&term)[0], "日本 ok");
        }

        #[test]
        fn should_show_what_a_redraw_left_not_what_it_erased() {
            let term = term_with(b"working\r\nstatus\x1b[2K\x1b[1A\x1b[2K\x1b[Gidle");
            assert_eq!(rows(&term)[..2], ["idle", ""]);
        }

        #[test]
        fn should_read_the_live_screen_not_scrollback() {
            let term = term_with(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix");
            assert_eq!(rows(&term), ["three", "four", "five", "six"]);
        }
    }
}
