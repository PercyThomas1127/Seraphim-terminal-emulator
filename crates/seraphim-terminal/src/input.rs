//! Encoding user input (keys, paste, focus) into bytes for the application,
//! according to the terminal's current [`Modes`].
//!
//! This is the legacy xterm encoding. The Kitty keyboard protocol
//! (spec §3.1) is planned as a separate encoder selected by mode flags.

use crate::{Modes, MouseEncoding, MouseMode};

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
    /// A modifier key itself (only reported in Kitty "report all keys" mode).
    Modifier(ModifierKey),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModifierKey {
    LeftShift,
    RightShift,
    LeftCtrl,
    RightCtrl,
    LeftAlt,
    RightAlt,
    LeftSuper,
    RightSuper,
}

/// Press, auto-repeat or release.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum KeyKind {
    #[default]
    Press,
    Repeat,
    Release,
}

/// Everything known about one key event, for [`encode_key_input`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyInput<'a> {
    /// For character keys, the *unshifted* character of the key (what the
    /// Kitty protocol reports as the key code, e.g. `a` for Shift+A).
    pub key: Key,
    pub mods: Mods,
    pub kind: KeyKind,
    /// The character with modifiers applied (`A` for Shift+a, `!` for
    /// Shift+1), if any.
    pub shifted: Option<char>,
    /// Composed text the key produces (IME/dead keys/layouts), if any.
    pub text: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    /// Super/Windows/Command key (only the Kitty protocol can report it).
    pub super_key: bool,
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
        Key::F(_) | Key::Modifier(_) => {}
    }
    out
}

/// Encode a key event using whichever protocol the application selected:
/// legacy xterm encoding, or the Kitty keyboard protocol when it pushed
/// flags (`CSI > flags u`).
pub fn encode_key_input(ev: &KeyInput, modes: &Modes) -> Vec<u8> {
    if modes.kitty_keyboard != 0 {
        return kitty(ev, modes.kitty_keyboard);
    }
    if ev.kind == KeyKind::Release {
        return Vec::new();
    }
    match ev.key {
        Key::Modifier(_) => Vec::new(),
        Key::Char(base) => {
            let m = ev.mods;
            if !(m.ctrl || m.alt) {
                if let Some(t) = ev.text {
                    return t.as_bytes().to_vec();
                }
            }
            encode_key(Key::Char(ev.shifted.unwrap_or(base)), m, modes)
        }
        k => encode_key(k, ev.mods, modes),
    }
}

/// Kitty keyboard protocol encoding.
/// <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>
fn kitty(ev: &KeyInput, flags: u8) -> Vec<u8> {
    const DISAMBIGUATE: u8 = 1;
    const EVENT_TYPES: u8 = 2;
    const ALTERNATES: u8 = 4;
    const ALL_KEYS: u8 = 8;
    const TEXT: u8 = 16;
    let has = |f: u8| flags & f != 0;
    if ev.kind == KeyKind::Release && !has(EVENT_TYPES) {
        return Vec::new();
    }
    let kind =
        if ev.kind == KeyKind::Repeat && !has(EVENT_TYPES) { KeyKind::Press } else { ev.kind };
    let m = ev.mods;
    let modval = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8 + 8 * m.super_key as u8;
    let event = match kind {
        KeyKind::Press => None,
        KeyKind::Repeat => Some(2),
        KeyKind::Release => Some(3),
    };
    // CSI code[:alternate] [; mods[:event]] [; text] final
    let csi = |code: Option<u32>, alt: Option<u32>, text: Option<&str>, fin: char| -> Vec<u8> {
        let mut s = String::from("\x1b[");
        let need_mods = modval != 1 || event.is_some() || text.is_some();
        match code {
            Some(c) => s.push_str(&c.to_string()),
            // Letter-final keys (arrows, Home, End, F1/F2/F4) use "1" as
            // the key number, omitted when nothing else follows.
            None if need_mods => s.push('1'),
            None => {}
        }
        if let Some(a) = alt {
            s.push_str(&format!(":{a}"));
        }
        if need_mods {
            s.push(';');
            if modval != 1 || event.is_some() {
                s.push_str(&modval.to_string());
            }
            if let Some(e) = event {
                s.push_str(&format!(":{e}"));
            }
        }
        if let Some(t) = text {
            let cps: Vec<String> = t.chars().map(|c| (c as u32).to_string()).collect();
            s.push(';');
            s.push_str(&cps.join(":"));
        }
        s.push(fin);
        s.into_bytes()
    };
    let legacy = |k: Key| encode_key(k, m, &Modes::default());
    match ev.key {
        Key::Char(base) => {
            let code = base.to_lowercase().next().unwrap_or(base) as u32;
            let alt = (has(ALTERNATES) && m.shift)
                .then_some(ev.shifted)
                .flatten()
                .filter(|&c| c as u32 != code)
                .map(|c| c as u32);
            if has(ALL_KEYS) {
                let text = (has(TEXT) && kind != KeyKind::Release)
                    .then_some(ev.text)
                    .flatten()
                    .filter(|t| !t.is_empty() && !t.chars().any(char::is_control));
                return csi(Some(code), alt, text, 'u');
            }
            let modified = m.ctrl || m.alt || m.super_key;
            if kind == KeyKind::Release || (has(DISAMBIGUATE) && modified) {
                return csi(Some(code), alt, None, 'u');
            }
            // Plain (or Shift-only) typing is sent as text.
            match ev.text {
                Some(t) => t.as_bytes().to_vec(),
                None => legacy(Key::Char(ev.shifted.unwrap_or(base))),
            }
        }
        Key::Enter | Key::Tab | Key::Backspace => {
            let code = match ev.key {
                Key::Enter => 13,
                Key::Tab => 9,
                _ => 127,
            };
            // Unmodified Enter/Tab/Backspace stay legacy (so typing `reset`
            // still works after a crashed program), and have no release
            // events, unless every key is reported.
            if has(ALL_KEYS) {
                csi(Some(code), None, None, 'u')
            } else if kind == KeyKind::Release {
                Vec::new()
            } else if has(DISAMBIGUATE) && modval != 1 {
                csi(Some(code), None, None, 'u')
            } else {
                legacy(ev.key)
            }
        }
        Key::Escape => {
            if has(DISAMBIGUATE) || has(ALL_KEYS) {
                csi(Some(27), None, None, 'u')
            } else if kind == KeyKind::Release {
                Vec::new()
            } else {
                legacy(Key::Escape)
            }
        }
        Key::Up => csi(None, None, None, 'A'),
        Key::Down => csi(None, None, None, 'B'),
        Key::Right => csi(None, None, None, 'C'),
        Key::Left => csi(None, None, None, 'D'),
        Key::Home => csi(None, None, None, 'H'),
        Key::End => csi(None, None, None, 'F'),
        Key::F(1) => csi(None, None, None, 'P'),
        Key::F(2) => csi(None, None, None, 'Q'),
        // F3 is `CSI 13 ~` (its legacy SS3 R form collides with CPR).
        Key::F(3) => csi(Some(13), None, None, '~'),
        Key::F(4) => csi(None, None, None, 'S'),
        Key::Insert => csi(Some(2), None, None, '~'),
        Key::Delete => csi(Some(3), None, None, '~'),
        Key::PageUp => csi(Some(5), None, None, '~'),
        Key::PageDown => csi(Some(6), None, None, '~'),
        Key::F(n @ 5..=12) => {
            csi(Some([15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize]), None, None, '~')
        }
        Key::F(_) => Vec::new(),
        Key::Modifier(k) => {
            if !has(ALL_KEYS) {
                return Vec::new();
            }
            let code = match k {
                ModifierKey::LeftShift => 57441,
                ModifierKey::LeftCtrl => 57442,
                ModifierKey::LeftAlt => 57443,
                ModifierKey::LeftSuper => 57444,
                ModifierKey::RightShift => 57447,
                ModifierKey::RightCtrl => 57448,
                ModifierKey::RightAlt => 57449,
                ModifierKey::RightSuper => 57450,
            };
            csi(Some(code), None, None, 'u')
        }
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
    /// Motion with no button held.
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

impl MouseButton {
    fn is_wheel(self) -> bool {
        matches!(self, Self::WheelUp | Self::WheelDown | Self::WheelLeft | Self::WheelRight)
    }
}

/// Whether the application currently wants mouse events at all.
pub fn mouse_reporting(modes: &Modes) -> bool {
    modes.mouse != MouseMode::Off
}

/// Encode a mouse event at cell (`col`, `row`) (0-based) for the
/// application, or `None` if the current modes don't report it.
pub fn encode_mouse(
    button: MouseButton,
    action: MouseAction,
    col: usize,
    row: usize,
    mods: Mods,
    modes: &Modes,
) -> Option<Vec<u8>> {
    match (modes.mouse, action) {
        (MouseMode::Off, _) => return None,
        (MouseMode::Press, MouseAction::Motion) => return None,
        (MouseMode::ButtonMotion, MouseAction::Motion) if button == MouseButton::None => {
            return None
        }
        _ => {}
    }
    // Wheels have no release event.
    if button.is_wheel() && action == MouseAction::Release {
        return None;
    }
    let sgr = modes.mouse_encoding == MouseEncoding::Sgr;
    let mut code: u32 = match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::None => 3,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
        MouseButton::WheelLeft => 66,
        MouseButton::WheelRight => 67,
    };
    // Legacy encodings can't say which button was released.
    if action == MouseAction::Release && !sgr {
        code = 3;
    }
    if action == MouseAction::Motion {
        code += 32;
    }
    if mods.shift {
        code += 4;
    }
    if mods.alt {
        code += 8;
    }
    if mods.ctrl {
        code += 16;
    }
    let (x, y) = (col as u32 + 1, row as u32 + 1);
    match modes.mouse_encoding {
        MouseEncoding::Sgr => {
            let fin = if action == MouseAction::Release { 'm' } else { 'M' };
            Some(format!("\x1b[<{code};{x};{y}{fin}").into_bytes())
        }
        MouseEncoding::Default => {
            // One byte per value, offset by 32: coordinates past 223 can't
            // be expressed, and xterm drops such events.
            if x > 223 || y > 223 {
                return None;
            }
            Some(vec![0x1b, b'[', b'M', (32 + code) as u8, (32 + x) as u8, (32 + y) as u8])
        }
        MouseEncoding::Utf8 => {
            let mut out = b"\x1b[M".to_vec();
            for v in [32 + code, 32 + x, 32 + y] {
                let c = char::from_u32(v.min(2047))?;
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            Some(out)
        }
    }
}
