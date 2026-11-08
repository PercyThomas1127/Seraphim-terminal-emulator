//! Encoding user input (keys, paste, focus) into bytes for the application,
//! according to the terminal's current [`Modes`].
//!
//! This is the legacy xterm encoding. The Kitty keyboard protocol
//! (spec §3.1) is planned as a separate encoder selected by mode flags.

use crate::Modes;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    /// F1..=F12
    F(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Mods {
    /// xterm modifier parameter (1 = none).
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }
    fn any(self) -> bool {
        self.shift || self.alt || self.ctrl
    }
}

/// Encode a key press. Returns an empty vector for unencodable input.
pub fn encode_key(key: Key, mods: Mods, modes: &Modes) -> Vec<u8> {
    let m = mods.param();
    let mut out = Vec::new();
    // CSI/SS3 cursor-style keys: ESC [ 1 ; m X  or  ESC O X / ESC [ X
    let cursor_key = |out: &mut Vec<u8>, final_byte: u8| {
        if mods.any() {
            out.extend_from_slice(format!("\x1b[1;{m}").as_bytes());
            out.push(final_byte);
        } else if modes.app_cursor {
            out.extend_from_slice(&[0x1b, b'O', final_byte]);
        } else {
            out.extend_from_slice(&[0x1b, b'[', final_byte]);
        }
    };
    // Tilde keys: ESC [ n ~  or ESC [ n ; m ~
    let tilde = |out: &mut Vec<u8>, n: u8| {
        if mods.any() {
            out.extend_from_slice(format!("\x1b[{n};{m}~").as_bytes());
        } else {
            out.extend_from_slice(format!("\x1b[{n}~").as_bytes());
        }
    };
    match key {
        Key::Char(c) => {
            if mods.alt {
                out.push(0x1b);
            }
            if mods.ctrl {
                if let Some(b) = ctrl_byte(c) {
                    out.push(b);
                    return out;
                }
            }
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        Key::Enter => {
            if mods.alt {
                out.push(0x1b);
            }
            out.extend_from_slice(if modes.linefeed_newline { b"\r\n" } else { b"\r" });
        }
        Key::Tab if mods.shift => out.extend_from_slice(b"\x1b[Z"),
        Key::Tab => out.push(b'\t'),
        Key::Backspace => {
            if mods.alt {
                out.push(0x1b);
            }
            out.push(if mods.ctrl { 0x08 } else { 0x7f });
        }
        Key::Escape => out.push(0x1b),
        Key::Up => cursor_key(&mut out, b'A'),
        Key::Down => cursor_key(&mut out, b'B'),
        Key::Right => cursor_key(&mut out, b'C'),
        Key::Left => cursor_key(&mut out, b'D'),
        Key::Home => cursor_key(&mut out, b'H'),
        Key::End => cursor_key(&mut out, b'F'),
        Key::Insert => tilde(&mut out, 2),
        Key::Delete => tilde(&mut out, 3),
        Key::PageUp => tilde(&mut out, 5),
        Key::PageDown => tilde(&mut out, 6),
        Key::F(n @ 1..=4) => {
            let fb = b"PQRS"[(n - 1) as usize];
            if mods.any() {
                out.extend_from_slice(format!("\x1b[1;{m}").as_bytes());
                out.push(fb);
            } else {
                out.extend_from_slice(&[0x1b, b'O', fb]);
            }
        }
        Key::F(n @ 5..=12) => tilde(&mut out, [15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize]),
        Key::F(_) => {}
    }
    out
}

fn ctrl_byte(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        '@' | ' ' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '7' | '/' => Some(0x1f),
        '8' | '?' => Some(0x7f),
        _ => None,
    }
}

/// Encode pasted text. With bracketed paste enabled the text is wrapped, and
/// any embedded end marker is stripped so pasted content cannot escape the
/// bracket and be executed as typed input.
pub fn encode_paste(text: &str, modes: &Modes) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if modes.bracketed_paste {
        let safe = normalized.replace("\x1b[201~", "").replace("\x1b[200~", "");
        let mut out = b"\x1b[200~".to_vec();
        out.extend_from_slice(safe.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        normalized.into_bytes()
    }
}

/// Focus in/out report, if the application requested it (mode 1004).
pub fn encode_focus(focused: bool, modes: &Modes) -> Vec<u8> {
    if !modes.focus_events {
        return Vec::new();
    }
    if focused {
        b"\x1b[I".to_vec()
    } else {
        b"\x1b[O".to_vec()
    }
}
