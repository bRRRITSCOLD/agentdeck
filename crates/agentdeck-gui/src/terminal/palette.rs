//! The terminal's colours in the active mode (spec §11: the terminal palette follows the
//! theme). The default foreground, background and cursor are the `foreground` and
//! `background` tokens, as on Penpot's `agentdeck / TerminalSurface`.
//!
//! The 16 ANSI colours are app-local, one set per mode: the design system has no terminal
//! palette yet, and its primitives lack magenta and cyan. Each set is Tailwind's hue scale at
//! the step that reads on that mode's background. When core-react ships terminal tokens, they
//! replace these tables.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use brrritscold_core_gpui::Mode;
use gpui::{Hsla, Rgba, rgb};

/// How much a `DIM` (SGR 2) foreground keeps of its alpha.
const DIM_ALPHA: f32 = 0.66;

/// Black, red, green, yellow, blue, magenta, cyan and white, then their bright forms, on the
/// dark background (`neutral-950`).
const DARK_ANSI: [u32; 16] = [
    0x404040, 0xf87171, 0x4ade80, 0xfacc15, 0x60a5fa, 0xc084fc, 0x22d3ee, 0xd4d4d4, //
    0x737373, 0xfca5a5, 0x86efac, 0xfde047, 0x93c5fd, 0xd8b4fe, 0x67e8f9, 0xfafafa,
];

/// The same, on the light background (white). "White" is a mid grey, so text a program
/// writes in white stays readable.
const LIGHT_ANSI: [u32; 16] = [
    0x171717, 0xdc2626, 0x16a34a, 0xa16207, 0x2563eb, 0x9333ea, 0x0891b2, 0xa3a3a3, //
    0x525252, 0xef4444, 0x22c55e, 0xca8a04, 0x3b82f6, 0xa855f7, 0x06b6d4, 0xd4d4d4,
];

/// The colours a terminal draws with in one mode.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    /// Default text, and the cursor.
    pub foreground: Hsla,
    /// The surface behind every cell without a background of its own.
    pub background: Hsla,
    ansi: [Hsla; 16],
}

impl Palette {
    /// The palette for `mode`.
    pub fn new(mode: Mode) -> Self {
        let s = mode.semantic();
        let table = mode.pick(LIGHT_ANSI, DARK_ANSI);
        Palette {
            foreground: s.foreground,
            background: s.background,
            ansi: table.map(|hex| rgb(hex).into()),
        }
    }

    /// What `color` is drawn as. A colour the program set (OSC 4, 10, 11), held in
    /// `colors`, wins over the palette.
    pub fn resolve(&self, color: Color, colors: &Colors) -> Hsla {
        match color {
            Color::Spec(rgb) => from_rgb(rgb),
            Color::Indexed(index) => self.indexed(index as usize, colors),
            Color::Named(name) => self.named(name, colors),
        }
    }

    fn named(&self, name: NamedColor, colors: &Colors) -> Hsla {
        if let Some(rgb) = colors[name] {
            return from_rgb(rgb);
        }
        use NamedColor::*;
        match name {
            Foreground | BrightForeground | Cursor => self.foreground,
            Background => self.background,
            DimForeground => dim(self.foreground),
            DimBlack | DimRed | DimGreen | DimYellow | DimBlue | DimMagenta | DimCyan
            | DimWhite => dim(self.ansi[name as usize - DimBlack as usize]),
            _ => self.indexed(name as usize, colors),
        }
    }

    fn indexed(&self, index: usize, colors: &Colors) -> Hsla {
        if let Some(rgb) = colors[index] {
            return from_rgb(rgb);
        }
        match index {
            0..16 => self.ansi[index],
            // The 6×6×6 cube, then the 24-step grey ramp, as xterm draws them.
            16..232 => {
                let level = |n: usize| if n == 0 { 0 } else { 55 + n * 40 } as u32;
                let cube = index - 16;
                rgb((level(cube / 36) << 16) | (level(cube / 6 % 6) << 8) | level(cube % 6)).into()
            }
            232..256 => {
                let grey = (8 + (index - 232) * 10) as u32;
                rgb((grey << 16) | (grey << 8) | grey).into()
            }
            _ => self.foreground,
        }
    }
}

/// `color` as SGR 2 draws it.
pub fn dim(color: Hsla) -> Hsla {
    color.opacity(DIM_ALPHA)
}

fn from_rgb(Rgb { r, g, b }: Rgb) -> Hsla {
    Rgba {
        r: f32::from(r) / 255.,
        g: f32::from(g) / 255.,
        b: f32::from(b) / 255.,
        a: 1.,
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(hex: u32) -> Hsla {
        rgb(hex).into()
    }

    mod new {
        use super::*;

        #[test]
        fn should_take_the_default_colours_from_the_modes_tokens() {
            for mode in [Mode::Light, Mode::Dark] {
                let palette = Palette::new(mode);

                assert_eq!(palette.foreground, mode.semantic().foreground);
                assert_eq!(palette.background, mode.semantic().background);
            }
        }

        #[test]
        fn should_draw_ansi_colours_differently_in_each_mode() {
            let red = Color::Named(NamedColor::Red);
            let colors = Colors::default();

            assert_ne!(
                Palette::new(Mode::Light).resolve(red, &colors),
                Palette::new(Mode::Dark).resolve(red, &colors)
            );
        }
    }

    mod resolve {
        use super::*;

        fn dark(color: Color) -> Hsla {
            Palette::new(Mode::Dark).resolve(color, &Colors::default())
        }

        #[test]
        fn should_draw_the_default_foreground_and_background_from_the_tokens() {
            let s = Mode::Dark.semantic();

            assert_eq!(dark(Color::Named(NamedColor::Foreground)), s.foreground);
            assert_eq!(dark(Color::Named(NamedColor::Background)), s.background);
        }

        #[test]
        fn should_draw_a_named_colour_and_its_index_alike() {
            assert_eq!(dark(Color::Named(NamedColor::BrightCyan)), hex(0x67e8f9));
            assert_eq!(dark(Color::Indexed(14)), hex(0x67e8f9));
        }

        #[test]
        fn should_draw_the_256_colour_cube_and_grey_ramp_as_xterm_does() {
            assert_eq!(dark(Color::Indexed(16)), hex(0x000000));
            assert_eq!(dark(Color::Indexed(196)), hex(0xff0000));
            assert_eq!(dark(Color::Indexed(110)), hex(0x87afd7));
            assert_eq!(dark(Color::Indexed(232)), hex(0x080808));
            assert_eq!(dark(Color::Indexed(255)), hex(0xeeeeee));
        }

        #[test]
        fn should_draw_a_true_colour_as_given() {
            assert_eq!(
                dark(Color::Spec(Rgb {
                    r: 0x12,
                    g: 0x34,
                    b: 0x56
                })),
                hex(0x123456)
            );
        }

        #[test]
        fn should_dim_a_dim_named_colour() {
            assert_eq!(dark(Color::Named(NamedColor::DimRed)), dim(hex(0xf87171)));
        }

        #[test]
        fn should_prefer_a_colour_the_program_set() {
            let mut colors = Colors::default();
            let set = Rgb { r: 1, g: 2, b: 3 };
            colors[NamedColor::Background] = Some(set);
            colors[1] = Some(set);
            let palette = Palette::new(Mode::Dark);

            assert_eq!(
                palette.resolve(Color::Named(NamedColor::Background), &colors),
                from_rgb(set)
            );
            assert_eq!(
                palette.resolve(Color::Named(NamedColor::Red), &colors),
                from_rgb(set)
            );
        }
    }
}
