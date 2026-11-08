//! Dynamic colors: the base color scheme (supplied by the embedder, e.g.
//! from the active theme) plus overrides applications set with OSC 4 /
//! 10 / 11 / 12. The Terminal Core needs these to answer color queries;
//! renderers apply the overrides when resolving cell colors.

pub type Rgb = [u8; 3];

/// The embedder's base colors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorScheme {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// ANSI colors 0-15. Entries 16-255 follow the xterm cube and greys.
    pub ansi: [Rgb; 16],
}

impl Default for ColorScheme {
    /// xterm's defaults.
    fn default() -> Self {
        ColorScheme {
            foreground: [0xe5, 0xe5, 0xe5],
            background: [0, 0, 0],
            cursor: [0xe5, 0xe5, 0xe5],
            ansi: [
                [0x00, 0x00, 0x00],
                [0xcd, 0x00, 0x00],
                [0x00, 0xcd, 0x00],
                [0xcd, 0xcd, 0x00],
                [0x00, 0x00, 0xee],
                [0xcd, 0x00, 0xcd],
                [0x00, 0xcd, 0xcd],
                [0xe5, 0xe5, 0xe5],
                [0x7f, 0x7f, 0x7f],
                [0xff, 0x00, 0x00],
                [0x00, 0xff, 0x00],
                [0xff, 0xff, 0x00],
                [0x5c, 0x5c, 0xff],
                [0xff, 0x00, 0xff],
                [0x00, 0xff, 0xff],
                [0xff, 0xff, 0xff],
            ],
        }
    }
}

/// The xterm 256-color palette entry for indices 16-255 (and `None` for the
/// scheme-defined 0-15).
pub fn xterm_256(i: u8) -> Option<Rgb> {
    match i {
        0..=15 => None,
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Some([level(i / 36), level((i / 6) % 6), level(i % 6)])
        }
        232..=255 => {
            let v = 8 + (i - 232) * 10;
            Some([v, v, v])
        }
    }
}

/// Application-set overrides. `None` means "use the scheme".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorOverrides {
    pub indexed: Box<[Option<Rgb>; 256]>,
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
    pub cursor: Option<Rgb>,
}

impl Default for ColorOverrides {
    fn default() -> Self {
        ColorOverrides {
            indexed: Box::new([None; 256]),
            foreground: None,
            background: None,
            cursor: None,
        }
    }
}

impl ColorOverrides {
    pub fn is_empty(&self) -> bool {
        self.foreground.is_none()
            && self.background.is_none()
            && self.cursor.is_none()
            && self.indexed.iter().all(Option::is_none)
    }
}

/// Parse an X11 color spec: `rgb:R/G/B` (1-4 hex digits per channel) or
/// `#RGB`, `#RRGGBB`, `#RRRGGGBBB`, `#RRRRGGGGBBBB`.
pub fn parse_color(spec: &str) -> Option<Rgb> {
    // Scale an n-digit hex channel to 8 bits.
    fn channel(h: &str) -> Option<u8> {
        if h.is_empty() || h.len() > 4 {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        let max = (1u32 << (4 * h.len())) - 1;
        Some(((v * 255 + max / 2) / max) as u8)
    }
    if let Some(rest) = spec.strip_prefix("rgb:") {
        let mut it = rest.split('/');
        let (r, g, b) = (it.next()?, it.next()?, it.next()?);
        if it.next().is_some() {
            return None;
        }
        return Some([channel(r)?, channel(g)?, channel(b)?]);
    }
    if let Some(hex) = spec.strip_prefix('#') {
        if hex.is_empty() || hex.len() % 3 != 0 || hex.len() > 12 {
            return None;
        }
        let n = hex.len() / 3;
        // In the '#' form, short channels are the *high* digits (#fff = #f0f0f0).
        let ch = |i: usize| -> Option<u8> {
            let v = u32::from_str_radix(&hex[i * n..(i + 1) * n], 16).ok()?;
            Some(match n {
                1 => (v << 4) as u8,
                2 => v as u8,
                3 => (v >> 4) as u8,
                _ => (v >> 8) as u8,
            })
        };
        return Some([ch(0)?, ch(1)?, ch(2)?]);
    }
    None
}

/// Format as xterm does in query replies: `rgb:rrrr/gggg/bbbb`.
pub fn format_color(c: Rgb) -> String {
    let w = |v: u8| v as u16 * 257;
    format!("rgb:{:04x}/{:04x}/{:04x}", w(c[0]), w(c[1]), w(c[2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_specs() {
        assert_eq!(parse_color("rgb:ff/80/00"), Some([255, 128, 0]));
        assert_eq!(parse_color("rgb:f/8/0"), Some([255, 136, 0]));
        assert_eq!(parse_color("rgb:ffff/8080/0000"), Some([255, 128, 0]));
        assert_eq!(parse_color("#ff8000"), Some([255, 128, 0]));
        assert_eq!(parse_color("#f80"), Some([240, 128, 0]));
        assert_eq!(parse_color("#ffff80800000"), Some([255, 128, 0]));
        assert_eq!(parse_color("red"), None);
        assert_eq!(parse_color("rgb:1/2"), None);
        assert_eq!(parse_color("#12345"), None);
    }

    #[test]
    fn format_round_trips() {
        let c = [0x14, 0x12, 0x1f];
        assert_eq!(format_color(c), "rgb:1414/1212/1f1f");
        assert_eq!(parse_color(&format_color(c)), Some(c));
    }

    #[test]
    fn cube() {
        assert_eq!(xterm_256(196), Some([255, 0, 0]));
        assert_eq!(xterm_256(255), Some([238, 238, 238]));
        assert_eq!(xterm_256(3), None);
    }
}
