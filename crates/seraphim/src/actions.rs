//! Window-side input policy that doesn't need a window: shortcuts, click
//! counting, pixel → cell mapping and OSC 52 payload decoding. Kept pure so
//! it can be unit-tested.

use std::time::{Duration, Instant};

use seraphim_terminal::SelectionKind;

/// Actions the terminal handles itself instead of sending to the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Copy,
    Paste,
    ScrollPageUp,
    ScrollPageDown,
    ScrollLineUp,
    ScrollLineDown,
    ScrollTop,
    ScrollBottom,
    /// Switch to the next profile (Ctrl+Shift+P).
    NextProfile,
    /// Re-read the configuration files (Ctrl+Shift+R).
    ReloadConfig,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutKey {
    Char(char),
    Insert,
    PageUp,
    PageDown,
    Home,
    End,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// Default shortcuts. Scrollback navigation is skipped on the alternate
/// screen (it has no history; full-screen apps want those keys).
pub fn shortcut(key: ShortcutKey, m: Modifiers, alt_screen: bool) -> Option<Action> {
    use ShortcutKey as K;
    let ctrl_shift = m.ctrl && m.shift && !m.alt;
    let shift_only = m.shift && !m.ctrl && !m.alt;
    match key {
        K::Char(c) if ctrl_shift && c.eq_ignore_ascii_case(&'c') => Some(Action::Copy),
        K::Char(c) if ctrl_shift && c.eq_ignore_ascii_case(&'v') => Some(Action::Paste),
        K::Char(c) if ctrl_shift && c.eq_ignore_ascii_case(&'p') => Some(Action::NextProfile),
        K::Char(c) if ctrl_shift && c.eq_ignore_ascii_case(&'r') => Some(Action::ReloadConfig),
        K::Insert if shift_only => Some(Action::Paste),
        _ if alt_screen => None,
        K::PageUp if shift_only => Some(Action::ScrollPageUp),
        K::PageDown if shift_only => Some(Action::ScrollPageDown),
        K::Home if shift_only => Some(Action::ScrollTop),
        K::End if shift_only => Some(Action::ScrollBottom),
        K::Up if ctrl_shift => Some(Action::ScrollLineUp),
        K::Down if ctrl_shift => Some(Action::ScrollLineDown),
        _ => None,
    }
}

/// Detects double and triple clicks.
#[derive(Debug, Default)]
pub struct ClickTracker {
    last: Option<(Instant, usize, usize, u8)>,
}

pub const MULTI_CLICK: Duration = Duration::from_millis(400);

impl ClickTracker {
    /// Register a press at cell (`row`, `col`); returns the click count
    /// (1, 2 or 3, then wrapping back to 1).
    pub fn press(&mut self, now: Instant, row: usize, col: usize) -> u8 {
        let count = match self.last {
            Some((t, r, c, n)) if now.duration_since(t) <= MULTI_CLICK && r == row && c == col => {
                n % 3 + 1
            }
            _ => 1,
        };
        self.last = Some((now, row, col, count));
        count
    }
}

pub fn selection_kind(clicks: u8, block: bool) -> SelectionKind {
    match clicks {
        2 => SelectionKind::Word,
        3 => SelectionKind::Line,
        _ if block => SelectionKind::Block,
        _ => SelectionKind::Simple,
    }
}

/// Map a pixel position to a (row, col) cell, clamped to the grid.
pub fn cell_at(
    x: f64,
    y: f64,
    padding: f32,
    cell_w: f32,
    cell_h: f32,
    cols: usize,
    rows: usize,
) -> (usize, usize) {
    let col = ((x - padding as f64) / cell_w as f64).floor().max(0.0) as usize;
    let row = ((y - padding as f64) / cell_h as f64).floor().max(0.0) as usize;
    (row.min(rows.saturating_sub(1)), col.min(cols.saturating_sub(1)))
}

/// Decode standard base64 (padding optional, whitespace ignored).
pub fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for b in input.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIFT: Modifiers = Modifiers { shift: true, ctrl: false, alt: false };
    const CTRL_SHIFT: Modifiers = Modifiers { shift: true, ctrl: true, alt: false };

    #[test]
    fn shortcuts() {
        assert_eq!(shortcut(ShortcutKey::Char('C'), CTRL_SHIFT, false), Some(Action::Copy));
        assert_eq!(shortcut(ShortcutKey::Char('v'), CTRL_SHIFT, true), Some(Action::Paste));
        assert_eq!(shortcut(ShortcutKey::Insert, SHIFT, false), Some(Action::Paste));
        assert_eq!(shortcut(ShortcutKey::Char('P'), CTRL_SHIFT, true), Some(Action::NextProfile));
        assert_eq!(shortcut(ShortcutKey::Char('r'), CTRL_SHIFT, false), Some(Action::ReloadConfig));
        assert_eq!(shortcut(ShortcutKey::PageUp, SHIFT, false), Some(Action::ScrollPageUp));
        assert_eq!(shortcut(ShortcutKey::PageUp, SHIFT, true), None, "alt screen gets the key");
        assert_eq!(shortcut(ShortcutKey::PageUp, Modifiers::default(), false), None);
        let ctrl = Modifiers { ctrl: true, ..Default::default() };
        assert_eq!(shortcut(ShortcutKey::Char('c'), ctrl, false), None, "Ctrl+C is SIGINT");
    }

    #[test]
    fn click_counting() {
        let mut t = ClickTracker::default();
        let t0 = Instant::now();
        assert_eq!(t.press(t0, 1, 1), 1);
        assert_eq!(t.press(t0 + Duration::from_millis(100), 1, 1), 2);
        assert_eq!(t.press(t0 + Duration::from_millis(200), 1, 1), 3);
        assert_eq!(t.press(t0 + Duration::from_millis(300), 1, 1), 1, "wraps after triple");
        assert_eq!(t.press(t0 + Duration::from_millis(350), 1, 2), 1, "different cell");
        assert_eq!(t.press(t0 + Duration::from_secs(5), 1, 2), 1, "too slow");
        assert_eq!(selection_kind(2, false), SelectionKind::Word);
        assert_eq!(selection_kind(1, true), SelectionKind::Block);
    }

    #[test]
    fn pixel_to_cell() {
        assert_eq!(cell_at(0.0, 0.0, 6.0, 9.0, 18.0, 80, 24), (0, 0), "padding maps to edge");
        assert_eq!(cell_at(6.0 + 9.0 * 3.5, 6.0 + 18.0 * 2.1, 6.0, 9.0, 18.0, 80, 24), (2, 3));
        assert_eq!(cell_at(1e6, 1e6, 6.0, 9.0, 18.0, 80, 24), (23, 79), "clamped");
    }

    #[test]
    fn base64() {
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64("aGVsbG8").unwrap(), b"hello", "padding optional");
        assert_eq!(decode_base64("4pyTIOWkqeS9vw==").unwrap(), "✓ 天使".as_bytes());
        assert_eq!(decode_base64("").unwrap(), b"");
        assert!(decode_base64("not base64!").is_none());
    }
}
