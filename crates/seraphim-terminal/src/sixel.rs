//! Sixel graphics (`DCS P1 ; P2 ; P3 q <data> ST`), decoded into an RGBA
//! image that the graphics store places at the cursor.
//!
//! Supports raster attributes (`"`), color definitions in RGB and HLS
//! (`#`), repeats (`!`), carriage return (`$`) and new line (`-`). P2 = 1
//! leaves unset pixels transparent; otherwise they take color 0.

/// Largest Sixel image side we decode, in pixels.
pub const MAX_SIDE: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum St {
    Data,
    /// Collecting numeric parameters for a command byte.
    Params(u8),
}

#[derive(Debug)]
pub(crate) struct Sixel {
    palette: Vec<[u8; 3]>,
    color: usize,
    x: usize,
    y: usize,
    repeat: usize,
    st: St,
    params: Vec<u32>,
    /// RGBA, `cap_w * cap_h`; alpha 0 = not drawn.
    pixels: Vec<u8>,
    cap_w: usize,
    cap_h: usize,
    /// Extent actually drawn or declared.
    width: usize,
    height: usize,
    transparent: bool,
    overflow: bool,
}

/// The VT340's default palette (percent RGB).
const VT340: [[u8; 3]; 16] = [
    [0, 0, 0],
    [20, 20, 80],
    [80, 13, 13],
    [20, 80, 20],
    [80, 20, 80],
    [20, 80, 80],
    [80, 80, 20],
    [53, 53, 53],
    [26, 26, 26],
    [33, 33, 60],
    [60, 26, 26],
    [33, 60, 33],
    [60, 33, 60],
    [33, 60, 60],
    [60, 60, 33],
    [80, 80, 80],
];

fn pct(v: u32) -> u8 {
    ((v.min(100) * 255 + 50) / 100) as u8
}

/// HLS (hue 0-360 with blue at 0, lightness and saturation in percent),
/// the DEC way, to RGB.
fn hls(h: u32, l: u32, s: u32) -> [u8; 3] {
    let (h, l, s) =
        (((h % 360) as f32 + 240.0) % 360.0, l.min(100) as f32 / 100.0, s.min(100) as f32 / 100.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [r, g, b].map(|v| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8)
}

impl Sixel {
    /// `params` are the DCS parameters P1;P2;P3.
    pub fn new(params: &[u16]) -> Self {
        let mut palette = vec![[0u8; 3]; 256];
        for (i, c) in VT340.iter().enumerate() {
            palette[i] = c.map(|v| pct(v as u32));
        }
        Sixel {
            palette,
            color: 0,
            x: 0,
            y: 0,
            repeat: 1,
            st: St::Data,
            params: Vec::new(),
            pixels: Vec::new(),
            cap_w: 0,
            cap_h: 0,
            width: 0,
            height: 0,
            transparent: params.get(1) == Some(&1),
            overflow: false,
        }
    }

    pub fn put(&mut self, b: u8) {
        if let St::Params(cmd) = self.st {
            match b {
                b'0'..=b'9' => {
                    let last = self.params.last_mut().expect("params start with one entry");
                    *last = last.saturating_mul(10).saturating_add((b - b'0') as u32).min(1 << 20);
                    return;
                }
                b';' => {
                    if self.params.len() < 8 {
                        self.params.push(0);
                    }
                    return;
                }
                _ => {
                    self.st = St::Data;
                    self.command(cmd);
                }
            }
        }
        match b {
            b'?'..=b'~' => self.sixel(b - b'?'),
            b'!' | b'#' | b'"' => {
                self.params.clear();
                self.params.push(0);
                self.st = St::Params(b);
            }
            b'$' => self.x = 0,
            b'-' => {
                self.x = 0;
                self.y += 6;
            }
            _ => {} // whitespace and anything unknown
        }
    }

    fn command(&mut self, cmd: u8) {
        let p = |i: usize| self.params.get(i).copied().unwrap_or(0);
        match cmd {
            b'!' => self.repeat = (p(0) as usize).clamp(1, MAX_SIDE),
            b'#' => {
                let reg = (p(0) as usize).min(255);
                if self.params.len() >= 5 {
                    self.palette[reg] = match p(1) {
                        1 => hls(p(2), p(3), p(4)),
                        _ => [pct(p(2)), pct(p(3)), pct(p(4))],
                    };
                }
                self.color = reg;
            }
            b'"' => {
                // Pan; Pad; Ph; Pv: declared size.
                let (w, h) = ((p(2) as usize).min(MAX_SIDE), (p(3) as usize).min(MAX_SIDE));
                if w > 0 && h > 0 {
                    self.grow(w, h);
                    self.width = self.width.max(w);
                    self.height = self.height.max(h);
                }
            }
            _ => {}
        }
    }

    fn grow(&mut self, w: usize, h: usize) {
        if w <= self.cap_w && h <= self.cap_h {
            return;
        }
        let (nw, nh) = (
            w.max(self.cap_w).max(self.cap_w * 2).min(MAX_SIDE),
            h.max(self.cap_h).max(self.cap_h * 2).min(MAX_SIDE),
        );
        let mut px = vec![0u8; nw * nh * 4];
        for row in 0..self.cap_h {
            let src = &self.pixels[row * self.cap_w * 4..(row + 1) * self.cap_w * 4];
            px[row * nw * 4..row * nw * 4 + src.len()].copy_from_slice(src);
        }
        self.pixels = px;
        self.cap_w = nw;
        self.cap_h = nh;
    }

    fn sixel(&mut self, bits: u8) {
        let n = std::mem::replace(&mut self.repeat, 1);
        if self.x + n > MAX_SIDE || self.y + 6 > MAX_SIDE {
            self.overflow = true;
            self.x = (self.x + n).min(MAX_SIDE);
            return;
        }
        if bits != 0 {
            self.grow(self.x + n, self.y + 6);
            let [r, g, b] = self.palette[self.color];
            for bit in 0..6 {
                if bits & (1 << bit) != 0 {
                    let row = self.y + bit;
                    for x in self.x..self.x + n {
                        let i = (row * self.cap_w + x) * 4;
                        self.pixels[i..i + 4].copy_from_slice(&[r, g, b, 255]);
                    }
                    self.height = self.height.max(row + 1);
                }
            }
        }
        self.x += n;
        self.width = self.width.max(self.x);
    }

    /// The finished image: (width, height, RGBA), or `None` if empty.
    pub fn finish(mut self) -> Option<(u32, u32, Vec<u8>)> {
        if let St::Params(cmd) = self.st {
            self.command(cmd);
        }
        let (w, h) = (self.width.min(self.cap_w), self.height.min(self.cap_h));
        if w == 0 || h == 0 {
            return None;
        }
        let bg = if self.transparent {
            [0, 0, 0, 0]
        } else {
            let [r, g, b] = self.palette[0];
            [r, g, b, 255]
        };
        let mut out = Vec::with_capacity(w * h * 4);
        for row in 0..h {
            for px in self.pixels[row * self.cap_w * 4..(row * self.cap_w + w) * 4].chunks_exact(4)
            {
                out.extend_from_slice(if px[3] == 0 { &bg } else { px });
            }
        }
        Some((w as u32, h as u32, out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(params: &[u16], data: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut s = Sixel::new(params);
        for &b in data {
            s.put(b);
        }
        s.finish().unwrap()
    }

    fn px(img: &(u32, u32, Vec<u8>), x: u32, y: u32) -> [u8; 4] {
        let i = ((y * img.0 + x) * 4) as usize;
        img.2[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn bands_colors_and_repeats() {
        // Red 100%, two columns full height; then '-' and a blue row.
        let img = decode(&[0, 1], b"#1;2;100;0;0#1~~-#2;2;0;0;100!3@");
        assert_eq!((img.0, img.1), (3, 7));
        assert_eq!(px(&img, 0, 0), [255, 0, 0, 255]);
        assert_eq!(px(&img, 1, 5), [255, 0, 0, 255]);
        assert_eq!(px(&img, 2, 0), [0, 0, 0, 0], "transparent background (P2=1)");
        assert_eq!(px(&img, 2, 6), [0, 0, 255, 255], "'@' is the top bit of the second band");
    }

    #[test]
    fn raster_attributes_and_opaque_background() {
        let img = decode(&[0, 0], b"\"1;1;5;4#0;2;0;100;0#7?");
        assert_eq!((img.0, img.1), (5, 4), "declared size wins over drawn extent");
        assert_eq!(px(&img, 4, 3), [0, 255, 0, 255], "unset pixels take color 0");
    }

    #[test]
    fn carriage_return_overdraws() {
        let img = decode(&[0, 1], b"#1;2;100;0;0A$#2;2;0;0;100B");
        // 'A' (2) sets row 1 red; after '$', 'B' (3) sets rows 0-1 blue.
        assert_eq!(px(&img, 0, 1), [0, 0, 255, 255], "drawn over");
        assert_eq!(px(&img, 0, 0), [0, 0, 255, 255]);
        assert_eq!(img.0, 1, "'$' returns to column 0");
    }

    #[test]
    fn hls_primaries() {
        assert_eq!(hls(0, 50, 100), [0, 0, 255], "DEC hue 0 is blue");
        assert_eq!(hls(120, 50, 100), [255, 0, 0]);
        assert_eq!(hls(240, 50, 100), [0, 255, 0]);
    }

    #[test]
    fn hostile_sizes_are_bounded() {
        let mut s = Sixel::new(&[0, 1]);
        for &b in b"!99999999~-!5000~" {
            s.put(b);
        }
        let img = s.finish();
        assert!(img.is_none_or(|i| i.0 as usize <= MAX_SIDE && i.1 as usize <= MAX_SIDE));
    }
}
