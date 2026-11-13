//! Our own renderer for a [`Frame`] (spec §11; Zed's GPL-3.0 terminal crates are never
//! copied). A cell grid in JetBrains Mono at `font-size.sm` and line height 1.2, as Penpot's
//! `agentdeck / TerminalSurface` sets it, with its italic faces and the Noto symbol fonts
//! bundled here (see `assets/fonts/README.md`).

use std::borrow::Cow;
use std::rc::Rc;

use brrritscold_core_gpui_tokens::primitive::{font_family, font_size};
use gpui::{
    App, BorderStyle, Bounds, DispatchPhase, Font, FontFallbacks, FontFeatures, FontStyle,
    FontWeight, Hsla, IntoElement, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    SharedString, Size, StrikethroughStyle, Styled as _, TextAlign, TextRun, UnderlineStyle,
    Window, canvas, fill, outline, point, px, size,
};

use super::screen::{Cursor, Frame, GridSize, Hit};
use alacritty_terminal::index::Side;
use alacritty_terminal::vte::ansi::CursorShape;

/// Line height as a multiple of the font size. Raw in Penpot too: no line-height token is 1.2.
const LINE_HEIGHT: f32 = 1.2;

/// The symbol fonts the terminal falls back to, in order (see `assets/fonts/README.md`).
pub const SYMBOL_FAMILIES: [&str; 2] = ["Noto Sans Symbols 2", "Noto Sans Symbols"];

/// JetBrains Mono's italic faces, which core-gpui doesn't bundle, then the symbol fonts.
const FONTS: [&[u8]; 4] = [
    include_bytes!("../../assets/fonts/JetBrainsMono-Italic.ttf"),
    include_bytes!("../../assets/fonts/JetBrainsMono-BoldItalic.ttf"),
    include_bytes!("../../assets/fonts/NotoSansSymbols2-Regular.ttf"),
    include_bytes!("../../assets/fonts/NotoSansSymbols-Regular.ttf"),
];

/// Registers the bundled fonts; call once, before the first terminal draws.
pub fn init(cx: &mut App) {
    cx.text_system()
        .add_fonts(FONTS.iter().map(|b| Cow::Borrowed(*b)).collect())
        .expect("the bundled JetBrains Mono italics and Noto symbol fonts are valid TTFs");
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

/// Where a terminal's grid was laid out in its window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    /// The top-left cell's top-left corner, in window coordinates.
    pub origin: Point<Pixels>,
    /// One cell.
    pub cell: Size<Pixels>,
    /// The grid that fits.
    pub grid: GridSize,
}

impl Layout {
    /// The cell under `position`, and which half of it; a position off the grid takes the
    /// nearest cell, so a drag past an edge selects up to it.
    pub fn hit(&self, position: Point<Pixels>) -> Hit {
        let at = position - self.origin;
        let rows = usize::from(self.grid.rows.max(1));
        let line = (at.y / self.cell.height)
            .floor()
            .clamp(0., (rows - 1) as f32) as usize;
        let cols = usize::from(self.grid.cols.max(1));
        let col = at.x / self.cell.width;
        let (col, side) = if col < 0. {
            (0, Side::Left)
        } else if col >= cols as f32 {
            (cols - 1, Side::Right)
        } else if col.fract() < 0.5 {
            (col as usize, Side::Left)
        } else {
            (col as usize, Side::Right)
        };
        Hit { line, col, side }
    }
}

/// What the left button does, wherever the pointer is: a drag that started on a terminal
/// goes on past its edges and over other views.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pointer {
    /// Moved with the button held, to this window position.
    Dragged(Point<Pixels>),
    /// Let go.
    Released,
}

/// Paints `frame` over `background`, filling its parent. Each layout reports where the grid
/// sits and the grid that fits to `on_layout`, before painting; every left-button drag and
/// release in the window goes to `on_pointer`.
pub fn terminal(
    frame: Frame,
    background: Hsla,
    on_layout: impl Fn(Layout, &mut Window, &mut App) + 'static,
    on_pointer: impl Fn(Pointer, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    canvas(
        move |bounds, window, cx| {
            let metrics = Metrics::measure(window);
            let layout = Layout {
                origin: bounds.origin,
                cell: metrics.cell,
                grid: metrics.grid(bounds.size),
            };
            on_layout(layout, window, cx);
            metrics
        },
        move |bounds, metrics, window, cx| {
            // Window-wide, not on a hitbox, so the drag isn't lost when the pointer leaves.
            let on_pointer = Rc::new(on_pointer);
            let dragged = on_pointer.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture
                    && event.pressed_button == Some(MouseButton::Left)
                {
                    dragged(Pointer::Dragged(event.position), window, cx);
                }
            });
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                    on_pointer(Pointer::Released, window, cx);
                }
            });
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
            let bounds = Bounds::new(
                at(line, bg.col),
                size(cell.width * bg.cells as f32, cell.height),
            );
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
            let _ = shaped.paint(
                at(line, run.col),
                cell.height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
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
            Bounds::new(
                origin + point(px(0.), cell.height - px(2.)),
                size(width, px(2.)),
            ),
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

    mod hit {
        use super::*;

        /// A 10 × 4 grid of 8 × 16 cells at (100, 50).
        const LAYOUT: Layout = Layout {
            origin: Point {
                x: px(100.),
                y: px(50.),
            },
            cell: Size {
                width: px(8.),
                height: px(16.),
            },
            grid: GridSize { cols: 10, rows: 4 },
        };

        fn hit(x: f32, y: f32) -> (usize, usize, Side) {
            let Hit { line, col, side } = LAYOUT.hit(point(px(x), px(y)));
            (line, col, side)
        }

        #[test]
        fn should_find_the_cell_and_its_half() {
            assert_eq!(
                hit(100. + 8. * 3. + 2., 50. + 16. * 2. + 1.),
                (2, 3, Side::Left)
            );
            assert_eq!(
                hit(100. + 8. * 3. + 6., 50. + 16. * 2. + 1.),
                (2, 3, Side::Right)
            );
        }

        #[test]
        fn should_take_the_first_cell_above_or_left_of_the_grid() {
            assert_eq!(hit(0., 0.), (0, 0, Side::Left));
        }

        #[test]
        fn should_take_the_last_cell_below_or_right_of_the_grid() {
            assert_eq!(hit(1000., 1000.), (3, 9, Side::Right));
        }

        #[test]
        fn should_take_the_line_end_right_of_the_grid() {
            assert_eq!(hit(1000., 50. + 16. + 1.), (1, 9, Side::Right));
        }
    }

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
                GridSize {
                    cols: 100,
                    rows: 29
                }
            );
        }

        #[test]
        fn should_keep_at_least_one_cell_each_way() {
            let metrics = Metrics {
                font_size: px(14.),
                cell: size(px(8.), px(16.8)),
            };

            assert_eq!(
                metrics.grid(size(px(0.), px(3.))),
                GridSize { cols: 1, rows: 1 }
            );
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
