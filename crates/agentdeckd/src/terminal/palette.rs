//! The colours the daemon's `Term` reports when a program asks (OSC 4, 10, 11, 12) and
//! hasn't set them itself. Clients draw with their own theme; these only answer queries, so
//! a program picking light or dark output sees a dark terminal, agentdeck's first mode.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{NamedColor, Rgb};

/// xterm's 16 colours.
const ANSI: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00),
    (0xcd, 0x00, 0x00),
    (0x00, 0xcd, 0x00),
    (0xcd, 0xcd, 0x00),
    (0x00, 0x00, 0xee),
    (0xcd, 0x00, 0xcd),
    (0x00, 0xcd, 0xcd),
    (0xe5, 0xe5, 0xe5),
    (0x7f, 0x7f, 0x7f),
    (0xff, 0x00, 0x00),
    (0x00, 0xff, 0x00),
    (0xff, 0xff, 0x00),
    (0x5c, 0x5c, 0xff),
    (0xff, 0x00, 0xff),
    (0x00, 0xff, 0xff),
    (0xff, 0xff, 0xff),
];

const FOREGROUND: Rgb = Rgb {
    r: 0xe5,
    g: 0xe5,
    b: 0xe5,
};
const BACKGROUND: Rgb = Rgb { r: 0, g: 0, b: 0 };

/// The colour at `index` in alacritty's numbering: the 256-colour palette, then the named
/// colours (foreground, background, cursor, …). A colour the program set wins.
pub fn color(colors: &Colors, index: usize) -> Rgb {
    if let Some(rgb) = colors[index] {
        return rgb;
    }
    match index {
        0..16 => {
            let (r, g, b) = ANSI[index];
            Rgb { r, g, b }
        }
        16..232 => {
            let level = |n: usize| if n == 0 { 0 } else { (55 + n * 40) as u8 };
            let cube = index - 16;
            Rgb {
                r: level(cube / 36),
                g: level(cube / 6 % 6),
                b: level(cube % 6),
            }
        }
        232..256 => {
            let gray = (8 + (index - 232) * 10) as u8;
            Rgb {
                r: gray,
                g: gray,
                b: gray,
            }
        }
        _ if index == NamedColor::Background as usize => BACKGROUND,
        _ => FOREGROUND,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    mod color {
        use super::*;

        #[test]
        fn should_follow_xterms_palette() {
            let colors = Colors::default();
            assert_eq!(color(&colors, 1), rgb(0xcd, 0, 0));
            assert_eq!(color(&colors, 16), rgb(0, 0, 0));
            assert_eq!(color(&colors, 196), rgb(0xff, 0, 0));
            assert_eq!(color(&colors, 110), rgb(0x87, 0xaf, 0xd7));
            assert_eq!(color(&colors, 232), rgb(8, 8, 8));
            assert_eq!(color(&colors, 255), rgb(0xee, 0xee, 0xee));
        }

        #[test]
        fn should_report_a_dark_background_and_a_light_foreground() {
            let colors = Colors::default();
            assert_eq!(color(&colors, NamedColor::Background as usize), BACKGROUND);
            assert_eq!(color(&colors, NamedColor::Foreground as usize), FOREGROUND);
        }

        #[test]
        fn should_prefer_a_colour_the_program_set() {
            let mut colors = Colors::default();
            colors[NamedColor::Background] = Some(rgb(1, 2, 3));
            assert_eq!(color(&colors, NamedColor::Background as usize), rgb(1, 2, 3));
        }
    }
}
