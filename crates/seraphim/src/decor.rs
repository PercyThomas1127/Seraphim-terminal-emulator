//! Seraphim's own window decorations (`window.decorations = "seraphim"`):
//! a title bar (or the right end of the tab bar or menu bar) with
//! minimize, maximize and close buttons, and edges that resize the window
//! when the desktop draws no border. Pure: painting and hit-testing.

use unicode_width::UnicodeWidthStr;
use winit::window::{CursorIcon, ResizeDirection};

/// Columns the three buttons take at the right end of their row.
pub const BUTTONS: usize = 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Minimize,
    Maximize,
    Close,
}

/// Colors for the bar and its buttons.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colors {
    pub bar: [u8; 3],
    pub fg: [u8; 3],
    pub dim: [u8; 3],
    pub hover: [u8; 3],
    /// The close button under the pointer.
    pub alert: [u8; 3],
}

fn sgr(out: &mut String, fg: [u8; 3], bg: [u8; 3], bold: bool) {
    let b = if bold { "1;" } else { "" };
    out.push_str(&format!(
        "\x1b[0;{b}38;2;{};{};{};48;2;{};{};{}m",
        fg[0], fg[1], fg[2], bg[0], bg[1], bg[2]
    ));
}

/// Which button is at `col` of a row `cols` wide.
pub fn button_at(col: usize, cols: usize) -> Option<Button> {
    let start = cols.checked_sub(BUTTONS)?;
    match col.checked_sub(start)? {
        0..=2 => Some(Button::Minimize),
        3..=5 => Some(Button::Maximize),
        6..=8 => Some(Button::Close),
        _ => None,
    }
}

/// The buttons, as ANSI text placed at the right end of row 1 of a
/// terminal `cols` wide.
pub fn paint_buttons(cols: usize, hover: Option<Button>, maximized: bool, c: &Colors) -> Vec<u8> {
    let mut out = String::new();
    if cols < BUTTONS {
        return out.into_bytes();
    }
    out.push_str(&format!("\x1b[1;{}H", cols - BUTTONS + 1));
    let max = if maximized { '❐' } else { '□' };
    for (b, glyph) in [(Button::Minimize, '─'), (Button::Maximize, max), (Button::Close, '✕')] {
        let (fg, bg) = match (hover == Some(b), b) {
            (true, Button::Close) => ([255, 255, 255], c.alert),
            (true, _) => (c.fg, c.hover),
            _ => (c.fg, c.bar),
        };
        sgr(&mut out, fg, bg, false);
        out.push(' ');
        out.push(glyph);
        out.push(' ');
    }
    out.push_str("\x1b[0m");
    out.into_bytes()
}

/// A title bar: the title in the middle, the buttons at the right.
pub fn paint_title(
    title: &str,
    cols: usize,
    focused: bool,
    hover: Option<Button>,
    maximized: bool,
    c: &Colors,
) -> Vec<u8> {
    let mut out = String::from("\x1b[H\x1b[?7l");
    sgr(&mut out, if focused { c.fg } else { c.dim }, c.bar, focused);
    let room = cols.saturating_sub(2 * BUTTONS + 2);
    let mut t = String::new();
    for ch in title.chars().filter(|c| !c.is_control()) {
        if t.width() + unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) > room {
            t.pop();
            t.push('…');
            break;
        }
        t.push(ch);
    }
    let left = (cols.saturating_sub(t.width())) / 2;
    out.push_str(&" ".repeat(left));
    out.push_str(&t);
    out.push_str(&" ".repeat(cols.saturating_sub(left + t.width())));
    let mut bytes = out.into_bytes();
    bytes.extend(paint_buttons(cols, hover, maximized, c));
    bytes
}

/// The window edge (or corner) at `(x, y)` in a window `w`×`h`, within
/// `border` pixels.
pub fn edge_at(x: f64, y: f64, w: f64, h: f64, border: f64) -> Option<ResizeDirection> {
    let (l, r, t, b) = (x < border, x >= w - border, y < border, y >= h - border);
    // Corners are a little larger, as they are harder to hit.
    let (lc, rc, tc, bc) =
        (x < border * 3.0, x >= w - border * 3.0, y < border * 3.0, y >= h - border * 3.0);
    use ResizeDirection as D;
    Some(match () {
        _ if (t && lc) || (l && tc) => D::NorthWest,
        _ if (t && rc) || (r && tc) => D::NorthEast,
        _ if (b && lc) || (l && bc) => D::SouthWest,
        _ if (b && rc) || (r && bc) => D::SouthEast,
        _ if t => D::North,
        _ if b => D::South,
        _ if l => D::West,
        _ if r => D::East,
        _ => return None,
    })
}

pub fn cursor_for(d: ResizeDirection) -> CursorIcon {
    use ResizeDirection as D;
    match d {
        D::North => CursorIcon::NResize,
        D::South => CursorIcon::SResize,
        D::East => CursorIcon::EResize,
        D::West => CursorIcon::WResize,
        D::NorthEast => CursorIcon::NeResize,
        D::NorthWest => CursorIcon::NwResize,
        D::SouthEast => CursorIcon::SeResize,
        D::SouthWest => CursorIcon::SwResize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colors() -> Colors {
        Colors { bar: [0; 3], fg: [200; 3], dim: [100; 3], hover: [50; 3], alert: [255, 0, 0] }
    }

    #[test]
    fn buttons_and_title() {
        assert_eq!(button_at(70, 80), None);
        assert_eq!(button_at(71, 80), Some(Button::Minimize));
        assert_eq!(button_at(75, 80), Some(Button::Maximize));
        assert_eq!(button_at(79, 80), Some(Button::Close));
        assert_eq!(button_at(3, 5), None);
        let mut t = seraphim_terminal::Terminal::new(40, 1);
        t.advance(&paint_title("vim notes.md", 40, true, Some(Button::Close), false, &colors()));
        let s = t.screen_text();
        assert!(s.contains("vim notes.md"), "{s:?}");
        assert!(s.ends_with(" ─  □  ✕"), "{s:?}");
        let mut t = seraphim_terminal::Terminal::new(30, 1);
        t.advance(&paint_title(&"x".repeat(50), 30, true, None, true, &colors()));
        let s = t.screen_text();
        assert!(s.contains('…') && s.contains('❐'), "{s:?}");
    }

    #[test]
    fn edges_and_corners() {
        use ResizeDirection as D;
        assert_eq!(edge_at(400.0, 300.0, 800.0, 600.0, 4.0), None);
        assert_eq!(edge_at(1.0, 300.0, 800.0, 600.0, 4.0), Some(D::West));
        assert_eq!(edge_at(799.0, 300.0, 800.0, 600.0, 4.0), Some(D::East));
        assert_eq!(edge_at(400.0, 599.0, 800.0, 600.0, 4.0), Some(D::South));
        assert_eq!(edge_at(1.0, 1.0, 800.0, 600.0, 4.0), Some(D::NorthWest));
        assert_eq!(edge_at(10.0, 599.0, 800.0, 600.0, 4.0), Some(D::SouthWest));
        assert_eq!(edge_at(799.0, 10.0, 800.0, 600.0, 4.0), Some(D::NorthEast));
    }
}
