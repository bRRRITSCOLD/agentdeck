//! Our own renderer for a [`Frame`] (spec §11; Zed's GPL-3.0 terminal crates are never
//! copied). A cell grid in JetBrains Mono at `font-size.sm` and line height 1.2, as Penpot's
//! `agentdeck / TerminalSurface` sets it, with the bundled Noto symbol fonts as fallbacks for
//! the glyphs agents print.

use std::borrow::Cow;

use brrritscold_core_gpui_tokens::primitive::{font_family, font_size};
use gpui::{
    App, BorderStyle, Bounds, Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, Hsla,
    IntoElement, Pixels, Point, SharedString, Size, StrikethroughStyle, Styled as _, TextAlign,
    TextRun, UnderlineStyle, Window, canvas, fill, outline, point, px, size,
};

use super::screen::{Cursor, Frame, GridSize};
use alacritty_terminal::vte::ansi::CursorShape;

/// Line height as a multiple of the font size. Raw in Penpot too: no line-height token is 1.2.
const LINE_HEIGHT: f32 = 1.2;

/// The symbol fonts the terminal falls back to, in order (see `assets/fonts/README.md`).
const SYMBOL_FAMILIES: [&str; 2] = ["Noto Sans Symbols 2", "Noto Sans Symbols"];

const SYMBOL_FONTS: [&[u8]; 2] = [
    include_bytes!("../../assets/fonts/NotoSansSymbols2-Regular.ttf"),
    include_bytes!("../../assets/fonts/NotoSansSymbols-Regular.ttf"),
];

/// Registers the bundled symbol fonts; call once, before the first terminal draws.
pub fn init(cx: &mut App) {
    cx.text_system()
        .add_fonts(SYMBOL_FONTS.iter().map(|b| Cow::Borrowed(*b)).collect())
        .expect("the bundled Noto symbol fonts are valid TTFs");
}

/// The terminal's font: `font-family.mono` without ligatures, then the symbol fonts, then
/// the token's own fallbacks.
pub fn font() -> Font {
    let fallbacks = SYMBOL_FAMILIES
        .iter()
        .chain(font_family::MONO.fallbacks)
        .map(|family| (*family).to_owned())
        .collect();
    Font {
        family: font_family::MONO.family.into(),
        features: FontFeatures::disable_ligatures(),
        fallbacks: Some(FontFallbacks::from_fonts(fallbacks)),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    }
}

/// A cell's size, and the font size it is measured at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// The font size.
    pub font_size: Pixels,
    /// One cell.
    pub cell: Size<Pixels>,
}

impl Metrics {
    /// The cell `font()` makes at `font-size.sm` in `window`.
    pub fn measure(window: &Window) -> Self {
        let font_size = font_size::SM.to_pixels(window.rem_size());
        let text = window.text_system();
        let font_id = text.resolve_font(&font());
        let width = text
            .advance(font_id, font_size, 'm')
            .map_or(font_size * 0.6, |advance| advance.width);
        Metrics {
            font_size,
            cell: size(width, font_size * LINE_HEIGHT),
        }
    }

    /// How many whole cells fit in `area`; at least one each way.
    pub fn grid(&self, area: Size<Pixels>) -> GridSize {
        let fit = |space: Pixels, cell: Pixels| (space / cell).floor().clamp(1., u16::MAX as f32);
        GridSize {
            cols: fit(area.width, self.cell.width) as u16,
            rows: fit(area.height, self.cell.height) as u16,
        }
    }
}

/// Paints `frame` over `background`, filling its parent. Each layout reports the grid that
/// fits to `on_grid`, before painting.
pub fn terminal(
    frame: Frame,
    background: Hsla,
    on_grid: impl Fn(GridSize, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    canvas(
        move |bounds, window, cx| {
            let metrics = Metrics::measure(window);
            on_grid(metrics.grid(bounds.size), window, cx);
            metrics
        },
        move |bounds, metrics, window, cx| {
            window.paint_quad(fill(bounds, background));
            paint(&frame, bounds.origin, metrics, window, cx);
        },
    )
    .size_full()
}

fn paint(frame: &Frame, origin: Point<Pixels>, m: Metrics, window: &mut Window, cx: &mut App) {
    let cell = m.cell;
    let at = |line: usize, col: usize| {
        origin + point(cell.width * col as f32, cell.height * line as f32)
    };
    let base = font();
    for (line, row) in frame.lines.iter().enumerate() {
        for bg in &row.backgrounds {
            let bounds = Bounds::new(at(line, bg.col), size(cell.width * bg.cells as f32, cell.height));
            window.paint_quad(fill(bounds, bg.color));
        }
        for run in &row.runs {
            let font = Font {
                weight: if run.style.bold {
                    FontWeight::BOLD
                } else {
                    FontWeight::NORMAL
                },
                style: if run.style.italic {
                    FontStyle::Italic
                } else {
                    FontStyle::Normal
                },
                ..base.clone()
            };
            let text_run = TextRun {
                len: run.text.len(),
                font,
                color: run.style.color,
                background_color: None,
                underline: run.style.underline.then_some(UnderlineStyle {
                    thickness: px(1.),
                    color: Some(run.style.color),
                    wavy: false,
                }),
                strikethrough: run.style.strikethrough.then_some(StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(run.style.color),
                }),
            };
            // Every glyph takes its cells' width, whatever font drew it; a wide character's
            // run is that one glyph.
            let force_width = if run.cells == 2 && run.text.chars().count() == 1 {
                cell.width * 2.
            } else {
                cell.width
            };
            let shaped = window.text_system().shape_line(
                SharedString::from(run.text.clone()),
                m.font_size,
                &[text_run],
                Some(force_width),
            );
            let _ = shaped.paint(at(line, run.col), cell.height, TextAlign::Left, None, window, cx);
        }
    }
    if let Some(cursor) = frame.cursor {
        window.paint_quad(cursor_quad(cursor, at(cursor.line, cursor.col), cell));
    }
}

fn cursor_quad(cursor: Cursor, origin: Point<Pixels>, cell: Size<Pixels>) -> gpui::PaintQuad {
    let width = cell.width * cursor.cells as f32;
    match cursor.shape {
        CursorShape::Beam => fill(Bounds::new(origin, size(px(2.), cell.height)), cursor.color),
        CursorShape::Underline => fill(
            Bounds::new(origin + point(px(0.), cell.height - px(2.)), size(width, px(2.))),
            cursor.color,
        ),
        _ => outline(
            Bounds::new(origin, size(width, cell.height)),
            cursor.color,
            BorderStyle::Solid,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod metrics {
        use super::*;

        #[test]
        fn should_fit_whole_cells_only() {
            let metrics = Metrics {
                font_size: px(14.),
                cell: size(px(8.), px(16.8)),
            };

            assert_eq!(
                metrics.grid(size(px(807.), px(500.))),
                GridSize { cols: 100, rows: 29 }
            );
        }

        #[test]
        fn should_keep_at_least_one_cell_each_way() {
            let metrics = Metrics {
                font_size: px(14.),
                cell: size(px(8.), px(16.8)),
            };

            assert_eq!(metrics.grid(size(px(0.), px(3.))), GridSize { cols: 1, rows: 1 });
        }
    }

    mod font {
        use super::*;

        #[test]
        fn should_fall_back_to_the_symbol_fonts_first() {
            let fallbacks = font().fallbacks.unwrap();

            assert_eq!(
                &fallbacks.fallback_list()[..2],
                ["Noto Sans Symbols 2", "Noto Sans Symbols"]
            );
        }
    }
}
