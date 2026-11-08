//! Color theme and resolution of terminal colors to RGB.

use seraphim_terminal::{Attrs, Color, Flags};

pub type Rgb = [u8; 3];

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// Text color drawn over a block cursor.
    pub cursor_text: Rgb,
    /// ANSI colors 0-15.
    pub ansi: [Rgb; 16],
    /// Render bold text in colors 0-7 with their bright variants.
    pub bold_is_bright: bool,
}

impl Default for Theme {
    /// "Celestial": a dark theme with soft rose and violet accents.
    fn default() -> Self {
        Theme {
            foreground: [0xe6, 0xe1, 0xf0],
            background: [0x14, 0x12, 0x1f],
            cursor: [0xf5, 0xa6, 0xd0],
            cursor_text: [0x14, 0x12, 0x1f],
            ansi: [
                [0x2a, 0x27, 0x3a], // black
                [0xff, 0x55, 0x77], // red
                [0x8b, 0xe9, 0xa8], // green
                [0xf9, 0xd9, 0x8b], // yellow
                [0x89, 0xa8, 0xff], // blue
                [0xc8, 0x9b, 0xff], // magenta
                [0x7f, 0xe0, 0xe8], // cyan
                [0xd8, 0xd3, 0xe6], // white
                [0x5c, 0x57, 0x75], // bright black
                [0xff, 0x7a, 0x96], // bright red
                [0xa9, 0xf5, 0xbf], // bright green
                [0xfc, 0xe6, 0xa9], // bright yellow
                [0xa8, 0xbf, 0xff], // bright blue
                [0xf5, 0xa6, 0xd0], // bright magenta (rose)
                [0xa3, 0xee, 0xf2], // bright cyan
                [0xff, 0xff, 0xff], // bright white
            ],
            bold_is_bright: false,
        }
    }
}

impl Theme {
    /// xterm 256-color palette entry.
    pub fn indexed(&self, i: u8) -> Rgb {
        match i {
            0..=15 => self.ansi[i as usize],
            16..=231 => {
                let i = i - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                [level(i / 36), level((i / 6) % 6), level(i % 6)]
            }
            232..=255 => {
                let v = 8 + (i - 232) * 10;
                [v, v, v]
            }
        }
    }

    fn resolve(&self, c: Color, default: Rgb, bold: bool) -> Rgb {
        match c {
            Color::Default => default,
            Color::Indexed(i) if bold && self.bold_is_bright && i < 8 => self.ansi[i as usize + 8],
            Color::Indexed(i) => self.indexed(i),
            Color::Rgb(r, g, b) => [r, g, b],
        }
    }

    /// Final (foreground, background) for a cell, applying bold-bright,
    /// inverse, dim and hidden. `bg` is `None` when it is the default
    /// background (nothing needs drawing).
    pub fn cell_colors(&self, a: &Attrs) -> (Rgb, Option<Rgb>) {
        let bold = a.flags.contains(Flags::BOLD);
        let mut fg = self.resolve(a.fg, self.foreground, bold);
        let mut bg = match a.bg {
            Color::Default => None,
            c => Some(self.resolve(c, self.background, false)),
        };
        if a.flags.contains(Flags::INVERSE) {
            let old_fg = fg;
            fg = bg.unwrap_or(self.background);
            bg = Some(old_fg);
        }
        if a.flags.contains(Flags::DIM) {
            let base = bg.unwrap_or(self.background);
            fg = mix(fg, base, 0.4);
        }
        if a.flags.contains(Flags::HIDDEN) {
            fg = bg.unwrap_or(self.background);
        }
        (fg, bg)
    }

    pub fn underline_color(&self, a: &Attrs, fg: Rgb) -> Rgb {
        match a.underline_color {
            Color::Default => fg,
            c => self.resolve(c, fg, false),
        }
    }
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round() as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2])]
}

/// Convert to the `[0, 1]` RGBA used by the shader. Colors stay in sRGB
/// space: the surface is a non-sRGB format, so blending matches what other
/// terminals (and users' eyes) expect for text.
pub fn to_f32(c: Rgb, alpha: f32) -> [f32; 4] {
    [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, alpha]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_cube_and_greys() {
        let t = Theme::default();
        assert_eq!(t.indexed(16), [0, 0, 0]);
        assert_eq!(t.indexed(196), [255, 0, 0]);
        assert_eq!(t.indexed(231), [255, 255, 255]);
        assert_eq!(t.indexed(232), [8, 8, 8]);
        assert_eq!(t.indexed(255), [238, 238, 238]);
    }

    #[test]
    fn inverse_swaps_including_defaults() {
        let t = Theme::default();
        let mut a = Attrs::default();
        a.flags.insert(Flags::INVERSE);
        assert_eq!(t.cell_colors(&a), (t.background, Some(t.foreground)));
    }
}
