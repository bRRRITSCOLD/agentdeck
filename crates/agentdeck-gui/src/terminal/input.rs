//! What a key press, a paste or the mouse sends to the pane's PTY: xterm's sequences, as a
//! terminal emulator writes them.

use gpui::{Keystroke, Modifiers};

/// The bytes `keystroke` sends, or `None` when it isn't the terminal's to handle (a
/// `secondary-` shortcut, a bare modifier). `app_cursor` is DECCKM: the cursor keys send
/// `ESC O` instead of `ESC [`.
pub fn bytes(keystroke: &Keystroke, app_cursor: bool) -> Option<Vec<u8>> {
    let m = &keystroke.modifiers;
    if m.platform || m.function {
        return None;
    }
    let modifier = 1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.control);

    let csi = |final_byte: char| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{final_byte}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            format!("\x1b[{final_byte}").into_bytes()
        }
    };
    let tilde = |code: u8| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[{code};{modifier}~").into_bytes()
        } else {
            format!("\x1b[{code}~").into_bytes()
        }
    };
    let alt = |bytes: Vec<u8>| -> Vec<u8> {
        if m.alt {
            [b"\x1b".as_slice(), &bytes].concat()
        } else {
            bytes
        }
    };

    let sequence = match keystroke.key.as_str() {
        "enter" => alt(b"\r".to_vec()),
        "tab" if m.shift => b"\x1b[Z".to_vec(),
        "tab" => alt(b"\t".to_vec()),
        "escape" => alt(b"\x1b".to_vec()),
        "backspace" if m.control => alt(b"\x08".to_vec()),
        "backspace" => alt(b"\x7f".to_vec()),
        "space" if m.control => b"\0".to_vec(),
        "up" => csi('A'),
        "down" => csi('B'),
        "right" => csi('C'),
        "left" => csi('D'),
        "home" => csi('H'),
        "end" => csi('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => ss3_or_csi('P', modifier),
        "f2" => ss3_or_csi('Q', modifier),
        "f3" => ss3_or_csi('R', modifier),
        "f4" => ss3_or_csi('S', modifier),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        key if m.control => alt(vec![control(key)?]),
        _ => alt(keystroke.key_char.clone()?.into_bytes()),
    };
    Some(sequence)
}

/// The bytes pasting `text` sends. With bracketed paste on, `text` goes between
/// `ESC [200~` and `ESC [201~`, with any ESC removed so it can't end the paste early; without
/// it, every line ending becomes the CR that Enter sends.
pub fn paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let text = text.replace('\x1b', "");
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// A mouse button a program can hear of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

/// Something the mouse did over a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mouse {
    Press(Button),
    Release(Button),
    /// Moved onto another cell, with a button held or none.
    Move(Option<Button>),
    WheelUp,
    WheelDown,
}

/// Which mouse events the program asked to hear of, least to most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tracking {
    /// Presses, releases and the wheel (DECSET 1000).
    Clicks,
    /// Those, and moves with a button held (DECSET 1002).
    Drags,
    /// Those, and every move (DECSET 1003).
    Motion,
}

/// How the program asked to hear of the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseProtocol {
    pub tracking: Tracking,
    /// SGR encoding (DECSET 1006); without it, xterm's normal one.
    pub sgr: bool,
}

/// The report `event` over the cell at zero-based `line`, `col` sends, or `None` when
/// `protocol` doesn't ask for it or can't encode the cell.
pub fn mouse(
    event: Mouse,
    line: usize,
    col: usize,
    modifiers: &Modifiers,
    protocol: MouseProtocol,
) -> Option<Vec<u8>> {
    let code = |button: Button| match button {
        Button::Left => 0,
        Button::Middle => 1,
        Button::Right => 2,
    };
    let (button, pressed) = match event {
        Mouse::Press(button) => (code(button), true),
        // The normal encoding can't say which button was let go.
        Mouse::Release(button) if protocol.sgr => (code(button), false),
        Mouse::Release(_) => (3, false),
        Mouse::Move(Some(button)) if protocol.tracking >= Tracking::Drags => {
            (32 + code(button), true)
        }
        Mouse::Move(None) if protocol.tracking == Tracking::Motion => (35, true),
        Mouse::Move(_) => return None,
        Mouse::WheelUp => (64, true),
        Mouse::WheelDown => (65, true),
    };
    let button = button
        + 4 * usize::from(modifiers.shift)
        + 8 * usize::from(modifiers.alt)
        + 16 * usize::from(modifiers.control);
    let (col, line) = (col + 1, line + 1);

    if protocol.sgr {
        let end = if pressed { 'M' } else { 'm' };
        return Some(format!("\x1b[<{button};{col};{line}{end}").into_bytes());
    }
    // One byte each, offset by 32: a cell past 223 doesn't fit, and xterm drops it.
    let byte = |n: usize| u8::try_from(32 + n).ok();
    Some(vec![
        0x1b,
        b'[',
        b'M',
        byte(button)?,
        byte(col)?,
        byte(line)?,
    ])
}

/// F1–F4: `ESC O P` alone, `ESC [ 1 ; m P` with modifiers.
fn ss3_or_csi(final_byte: char, modifier: u8) -> Vec<u8> {
    if modifier > 1 {
        format!("\x1b[1;{modifier}{final_byte}").into_bytes()
    } else {
        format!("\x1bO{final_byte}").into_bytes()
    }
}

/// The C0 byte `ctrl-<key>` sends.
fn control(key: &str) -> Option<u8> {
    let [byte] = key.as_bytes() else {
        return None;
    };
    match byte {
        b'a'..=b'z' => Some(byte - b'a' + 1),
        b'@' | b'2' => Some(0),
        b'[' | b'3' => Some(0x1b),
        b'\\' | b'4' => Some(0x1c),
        b']' | b'5' => Some(0x1d),
        b'^' | b'6' => Some(0x1e),
        b'_' | b'-' | b'7' => Some(0x1f),
        b'/' => Some(0x1f),
        b'8' | b'?' => Some(0x7f),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod paste {
        use super::*;

        #[test]
        fn should_send_line_endings_as_enter_does() {
            assert_eq!(paste("a\nb\r\nc\rd", false), b"a\rb\rc\rd");
        }

        #[test]
        fn should_wrap_text_in_brackets_when_the_program_asks() {
            assert_eq!(paste("a\nb", true), b"\x1b[200~a\nb\x1b[201~");
        }

        #[test]
        fn should_strip_escapes_that_could_end_a_bracketed_paste_early() {
            assert_eq!(
                paste("a\x1b[201~rm -rf ~\n", true),
                b"\x1b[200~a[201~rm -rf ~\n\x1b[201~"
            );
        }
    }

    mod mouse {
        use super::*;

        const SGR: MouseProtocol = MouseProtocol {
            tracking: Tracking::Clicks,
            sgr: true,
        };
        const NORMAL: MouseProtocol = MouseProtocol {
            tracking: Tracking::Clicks,
            sgr: false,
        };

        fn report(
            event: Mouse,
            line: usize,
            col: usize,
            protocol: MouseProtocol,
        ) -> Option<Vec<u8>> {
            mouse(event, line, col, &Modifiers::default(), protocol)
        }

        fn tracking(tracking: Tracking) -> MouseProtocol {
            MouseProtocol { tracking, ..SGR }
        }

        mod sgr {
            use super::*;

            #[test]
            fn should_report_a_press_at_the_one_based_cell() {
                assert_eq!(
                    report(Mouse::Press(Button::Left), 2, 4, SGR),
                    Some(b"\x1b[<0;5;3M".to_vec())
                );
                assert_eq!(
                    report(Mouse::Press(Button::Right), 0, 0, SGR),
                    Some(b"\x1b[<2;1;1M".to_vec())
                );
            }

            #[test]
            fn should_report_a_release_with_its_button_and_a_lowercase_m() {
                assert_eq!(
                    report(Mouse::Release(Button::Middle), 0, 0, SGR),
                    Some(b"\x1b[<1;1;1m".to_vec())
                );
            }

            #[test]
            fn should_report_the_wheel_as_buttons_64_and_65() {
                assert_eq!(
                    report(Mouse::WheelUp, 0, 0, SGR),
                    Some(b"\x1b[<64;1;1M".to_vec())
                );
                assert_eq!(
                    report(Mouse::WheelDown, 0, 0, SGR),
                    Some(b"\x1b[<65;1;1M".to_vec())
                );
            }

            #[test]
            fn should_report_cells_past_column_223() {
                assert_eq!(
                    report(Mouse::Press(Button::Left), 0, 299, SGR),
                    Some(b"\x1b[<0;300;1M".to_vec())
                );
            }
        }

        mod normal {
            use super::*;

            #[test]
            fn should_offset_the_button_and_one_based_cell_by_32() {
                assert_eq!(
                    report(Mouse::Press(Button::Left), 2, 4, NORMAL),
                    Some(b"\x1b[M %#".to_vec())
                );
            }

            #[test]
            fn should_report_any_release_as_button_3() {
                assert_eq!(
                    report(Mouse::Release(Button::Right), 0, 0, NORMAL),
                    Some(b"\x1b[M#!!".to_vec())
                );
            }

            #[test]
            fn should_report_up_to_column_223_and_drop_the_rest() {
                assert_eq!(
                    report(Mouse::Press(Button::Left), 0, 222, NORMAL),
                    Some(vec![0x1b, b'[', b'M', 32, 255, 33])
                );
                assert_eq!(report(Mouse::Press(Button::Left), 0, 223, NORMAL), None);
                assert_eq!(report(Mouse::Press(Button::Left), 223, 0, NORMAL), None);
            }
        }

        #[test]
        fn should_add_the_modifiers_to_the_button() {
            let ctrl_alt = Modifiers {
                control: true,
                alt: true,
                ..Modifiers::default()
            };
            assert_eq!(
                mouse(Mouse::Press(Button::Left), 0, 0, &ctrl_alt, SGR),
                Some(b"\x1b[<24;1;1M".to_vec())
            );
            assert_eq!(
                mouse(Mouse::WheelUp, 0, 0, &Modifiers::shift(), SGR),
                Some(b"\x1b[<68;1;1M".to_vec())
            );
        }

        mod moves {
            use super::*;

            #[test]
            fn should_add_32_to_a_drag() {
                assert_eq!(
                    report(
                        Mouse::Move(Some(Button::Left)),
                        0,
                        1,
                        tracking(Tracking::Drags)
                    ),
                    Some(b"\x1b[<32;2;1M".to_vec())
                );
            }

            #[test]
            fn should_report_a_buttonless_move_as_button_35() {
                assert_eq!(
                    report(Mouse::Move(None), 0, 1, tracking(Tracking::Motion)),
                    Some(b"\x1b[<35;2;1M".to_vec())
                );
            }

            #[test]
            fn should_report_a_drag_only_when_drags_or_motion_are_tracked() {
                let drag = Mouse::Move(Some(Button::Left));
                assert_eq!(report(drag, 0, 0, tracking(Tracking::Clicks)), None);
                assert!(report(drag, 0, 0, tracking(Tracking::Motion)).is_some());
            }

            #[test]
            fn should_report_a_buttonless_move_only_when_motion_is_tracked() {
                assert_eq!(
                    report(Mouse::Move(None), 0, 0, tracking(Tracking::Drags)),
                    None
                );
            }
        }
    }

    fn send(keystroke: &'static str) -> Option<Vec<u8>> {
        bytes(&Keystroke::parse(keystroke).unwrap(), false)
    }

    fn typed(key: &'static str, key_char: &str) -> Option<Vec<u8>> {
        let keystroke = Keystroke {
            key: key.into(),
            key_char: Some(key_char.into()),
            ..Keystroke::parse(key).unwrap()
        };
        bytes(&keystroke, false)
    }

    mod bytes {
        use super::*;

        #[test]
        fn should_send_what_a_printable_key_types() {
            assert_eq!(typed("a", "a"), Some(b"a".to_vec()));
            assert_eq!(typed("é", "é"), Some("é".as_bytes().to_vec()));
        }

        #[test]
        fn should_send_the_c0_byte_for_a_control_letter() {
            assert_eq!(send("ctrl-c"), Some(vec![0x03]));
            assert_eq!(send("ctrl-d"), Some(vec![0x04]));
            assert_eq!(send("ctrl-["), Some(vec![0x1b]));
        }

        #[test]
        fn should_send_the_editing_keys_as_xterm_does() {
            assert_eq!(send("enter"), Some(b"\r".to_vec()));
            assert_eq!(send("backspace"), Some(b"\x7f".to_vec()));
            assert_eq!(send("tab"), Some(b"\t".to_vec()));
            assert_eq!(send("shift-tab"), Some(b"\x1b[Z".to_vec()));
            assert_eq!(send("escape"), Some(b"\x1b".to_vec()));
            assert_eq!(send("delete"), Some(b"\x1b[3~".to_vec()));
            assert_eq!(send("pageup"), Some(b"\x1b[5~".to_vec()));
            assert_eq!(send("f1"), Some(b"\x1bOP".to_vec()));
            assert_eq!(send("f5"), Some(b"\x1b[15~".to_vec()));
        }

        #[test]
        fn should_prefix_escape_for_alt() {
            assert_eq!(send("alt-enter"), Some(b"\x1b\r".to_vec()));
            assert_eq!(send("alt-ctrl-c"), Some(b"\x1b\x03".to_vec()));
        }

        #[test]
        fn should_leave_secondary_shortcuts_to_the_app() {
            assert_eq!(send("cmd-c"), None);
        }

        mod cursor_keys {
            use super::*;

            #[test]
            fn should_send_csi_normally() {
                assert_eq!(send("up"), Some(b"\x1b[A".to_vec()));
                assert_eq!(send("left"), Some(b"\x1b[D".to_vec()));
            }

            #[test]
            fn should_send_ss3_in_application_mode() {
                let up = Keystroke::parse("up").unwrap();

                assert_eq!(bytes(&up, true), Some(b"\x1bOA".to_vec()));
            }

            #[test]
            fn should_carry_modifiers_as_a_parameter() {
                assert_eq!(send("ctrl-right"), Some(b"\x1b[1;5C".to_vec()));
                assert_eq!(send("shift-up"), Some(b"\x1b[1;2A".to_vec()));
                assert_eq!(send("alt-delete"), Some(b"\x1b[3;3~".to_vec()));
            }
        }
    }
}
