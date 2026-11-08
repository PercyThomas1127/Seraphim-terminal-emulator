//! Inline images: the Kitty graphics protocol (APC `G`) and Sixel (see
//! [`crate::sixel`]) share this store.
//!
//! Images are decoded to RGBA once and kept by id. A [`Placement`] puts an
//! image on the grid; it is anchored to an *absolute* line number (lines
//! scrolled so far + screen row), so it moves with the text as the screen
//! scrolls into history, and disappears with that history.
//!
//! Supported (kitty spec): transmit (`a=t`), transmit and display (`a=T`),
//! put (`a=p`), delete (`a=d`, all selectors), query (`a=q`); formats
//! RGB, RGBA and PNG; zlib compression (`o=z`); chunked transfers (`m=1`);
//! direct data and files (`t=d`, `t=f`, `t=t`); source rectangles,
//! cell offsets, sizes in cells, z-index and `C=1`. Not supported yet:
//! nothing of the current spec. Animation (`a=f`, `a=a`, `a=c`), shared
//! memory (`t=s`) and Unicode placeholders (`U=1`, see [`PLACEHOLDER`])
//! are included.

use std::collections::HashMap;
use std::sync::Arc;

/// Largest image side we accept, in pixels.
pub const MAX_SIDE: u32 = 10_000;
/// Decoded image data kept at most (oldest unused images are dropped).
pub const QUOTA_BYTES: usize = 320 << 20;
/// Largest compressed/encoded transmission we buffer.
const MAX_TRANSMISSION: usize = 256 << 20;

/// A decoded image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub id: u32,
    /// Client-chosen image number (`I=`), 0 if none.
    pub number: u32,
    pub width: u32,
    pub height: u32,
    /// Straight (non-premultiplied) RGBA, `width * height * 4` bytes.
    pub rgba: Arc<Vec<u8>>,
    /// Unique across every image this terminal has ever stored, so a
    /// renderer can cache textures by it.
    pub key: u64,
    /// No id from the client (Sixel, anonymous kitty images): dropped as
    /// soon as nothing shows it.
    transient: bool,
    /// Animation: frames after the first (`rgba` is frame 1) and playback.
    pub anim: Animation,
}

/// Default gap between frames, in milliseconds (kitty's).
pub const DEFAULT_GAP: u32 = 40;
/// Frames kept per image.
pub const MAX_FRAMES: usize = 1000;

/// One frame after the first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub rgba: Arc<Vec<u8>>,
    /// Texture cache key, unique per frame contents.
    pub key: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlayState {
    #[default]
    Stopped,
    /// Play, but wait on the last frame for more to arrive.
    Loading,
    Running,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Animation {
    pub frames: Vec<Frame>,
    /// Gap after each frame (frame 1 first), ms; 0 = shown only in passing.
    pub gaps: Vec<u32>,
    pub state: PlayState,
    /// Loops to play: 0 forever, else this many passes.
    pub loops: u32,
    /// Frame shown while stopped (0-based), and where playback started.
    pub current: usize,
    /// When playback (re)started.
    pub epoch: std::time::Instant,
}

impl Default for Animation {
    fn default() -> Self {
        Animation {
            frames: Vec::new(),
            gaps: vec![DEFAULT_GAP],
            state: PlayState::Stopped,
            loops: 0,
            current: 0,
            epoch: std::time::Instant::now(),
        }
    }
}

impl Image {
    pub fn frame_count(&self) -> usize {
        1 + self.anim.frames.len()
    }

    /// Whether it changes over time right now.
    pub fn animating(&self) -> bool {
        self.frame_count() > 1 && self.anim.state != PlayState::Stopped
    }

    /// The frame index to show at `now`.
    pub fn frame_at(&self, now: std::time::Instant) -> usize {
        let a = &self.anim;
        let n = self.frame_count();
        if n == 1 || a.state == PlayState::Stopped {
            return a.current.min(n - 1);
        }
        let gaps: Vec<u64> =
            (0..n).map(|i| a.gaps.get(i).copied().unwrap_or(DEFAULT_GAP) as u64).collect();
        let period: u64 = gaps.iter().sum::<u64>().max(1);
        let mut t = now.saturating_duration_since(a.epoch).as_millis() as u64;
        // Playback starts at `current`: offset by the time before it.
        t += gaps[..a.current.min(n - 1)].iter().sum::<u64>();
        let last = n - 1;
        let finished = match a.state {
            PlayState::Loading => t >= period - gaps[last],
            _ => a.loops > 0 && t >= period * a.loops as u64,
        };
        if finished {
            return last;
        }
        let mut t = t % period;
        for (i, g) in gaps.iter().enumerate() {
            if t < *g {
                return i;
            }
            t -= g;
        }
        last
    }

    /// RGBA and texture key of frame `i` (0-based).
    pub fn frame(&self, i: usize) -> (&Arc<Vec<u8>>, u64) {
        match i.checked_sub(1).and_then(|j| self.anim.frames.get(j)) {
            Some(f) => (&f.rgba, f.key),
            None => (&self.rgba, self.key),
        }
    }
}

/// An image shown on the grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    pub image: u32,
    /// Client placement id (`p=`), 0 if none.
    pub id: u32,
    /// On the alternate screen.
    pub alt: bool,
    /// Absolute line of the top row (see [`Graphics::scrolled`]).
    pub line: i64,
    pub col: usize,
    /// Source rectangle in image pixels: x, y, w, h.
    pub src: [u32; 4],
    /// Pixel offset inside the first cell.
    pub offset: [u32; 2],
    /// Size in cells.
    pub cols: usize,
    pub rows: usize,
    /// Scale the image to fill `cols` x `rows` cells (sizes given by the
    /// client) rather than drawing it at its own pixel size.
    pub fill: bool,
    /// Below text when negative; below cell backgrounds under
    /// [`BELOW_BACKGROUND`].
    pub z: i32,
    /// A virtual placement (`U=1`): shown only through placeholder cells,
    /// not anchored to the grid.
    pub virtual_: bool,
}

/// The Unicode placeholder character: cells holding it show part of a
/// virtual placement (`U=1`). Combining marks from [`DIACRITICS`] give the
/// row, the column and the image id's high byte; the foreground color gives
/// the image id, the underline color the placement id.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// Row/column diacritics, in order (index = value), from the kitty spec.
pub const DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1,
    0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8,
    0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB,
    0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954,
    0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76,
    0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71,
    0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
    0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1,
    0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7,
    0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2,
    0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE,
    0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1,
    0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED,
    0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1,
    0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186,
    0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

/// The number a placeholder diacritic stands for.
pub fn diacritic_value(c: char) -> Option<u32> {
    DIACRITICS.binary_search(&(c as u32)).ok().map(|i| i as u32)
}

/// z-index under which an image is drawn below cell backgrounds too.
pub const BELOW_BACKGROUND: i32 = -1_073_741_824;

/// A placement resolved for the current viewport.
#[derive(Clone, Debug)]
pub struct VisiblePlacement<'a> {
    pub image: &'a Image,
    pub placement: &'a Placement,
    /// Viewport row of the top edge; negative when partly scrolled off.
    pub row: i64,
}

/// Where the cursor is and what the grid looks like, for a command.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ctx {
    pub row: usize,
    pub col: usize,
    pub rows: usize,
    pub alt: bool,
}

/// What the terminal must do after a command.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Effect {
    /// Bytes to send back to the application.
    pub response: Vec<u8>,
    /// A placement was made at the cursor: move past it (cols, rows).
    pub cursor_move: Option<(usize, usize)>,
}

/// Image store and placements.
#[derive(Debug)]
pub struct Graphics {
    images: HashMap<u32, Image>,
    placements: Vec<Placement>,
    /// Lines scrolled off the top of each screen (primary, alternate).
    scrolled: [i64; 2],
    loading: Option<Loading>,
    next_id: u32,
    next_key: u64,
    /// Bumped on every visible change.
    pub generation: u64,
    /// Cell size in pixels, set by the front end (default 8x16).
    pub cell_px: (f32, f32),
    /// `cell_px` came from the front end (pixel size reports need it).
    pub cell_px_known: bool,
    /// Insertion order of image ids, oldest first (for the quota).
    order: Vec<u32>,
}

#[derive(Debug)]
struct Loading {
    cmd: Cmd,
    data: Vec<u8>,
    /// base64 bytes not yet decoded (a chunk may split a quartet).
    pending: Vec<u8>,
    failed: Option<String>,
}

impl Default for Graphics {
    fn default() -> Self {
        Graphics {
            images: HashMap::new(),
            placements: Vec::new(),
            scrolled: [0, 0],
            loading: None,
            next_id: 1 << 31,
            next_key: 1,
            generation: 0,
            cell_px: (8.0, 16.0),
            cell_px_known: false,
            order: Vec::new(),
        }
    }
}

/// Parsed control data of one APC `G` command.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Cmd {
    action: u8,
    quiet: u8,
    format: u32,
    medium: u8,
    width: u32,
    height: u32,
    size: usize,
    file_offset: usize,
    id: u32,
    number: u32,
    placement: u32,
    more: bool,
    compression: u8,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    off_x: u32,
    off_y: u32,
    cols: u32,
    rows: u32,
    z: i32,
    no_move: bool,
    unicode: bool,
    delete: u8,
    /// `a=` was given (a continuation chunk has no action).
    explicit_action: bool,
}

impl Cmd {
    fn parse(control: &[u8]) -> Result<Cmd, String> {
        let mut c =
            Cmd { action: b't', medium: b'd', format: 32, delete: b'a', ..Default::default() };
        for kv in control.split(|&b| b == b',') {
            if kv.is_empty() {
                continue;
            }
            let (k, v) = match kv.iter().position(|&b| b == b'=') {
                Some(1) => (kv[0], &kv[2..]),
                _ => return Err(format!("EINVAL:malformed key {:?}", String::from_utf8_lossy(kv))),
            };
            let num = || -> Result<u32, String> {
                std::str::from_utf8(v)
                    .ok()
                    .and_then(|s| s.parse::<u32>().ok())
                    .ok_or_else(|| format!("EINVAL:bad value for {}", k as char))
            };
            let ch = || v.first().copied().unwrap_or(0);
            match k {
                b'a' => {
                    c.action = ch();
                    c.explicit_action = true;
                }
                b'q' => c.quiet = num()? as u8,
                b'f' => c.format = num()?,
                b't' => c.medium = ch(),
                b's' => c.width = num()?,
                b'v' => c.height = num()?,
                b'S' => c.size = num()? as usize,
                b'O' => c.file_offset = num()? as usize,
                b'i' => c.id = num()?,
                b'I' => c.number = num()?,
                b'p' => c.placement = num()?,
                b'm' => c.more = num()? == 1,
                b'o' => c.compression = ch(),
                b'x' => c.x = num()?,
                b'y' => c.y = num()?,
                b'w' => c.w = num()?,
                b'h' => c.h = num()?,
                b'X' => c.off_x = num()?,
                b'Y' => c.off_y = num()?,
                b'c' => c.cols = num()?,
                b'r' => c.rows = num()?,
                b'z' => {
                    c.z = std::str::from_utf8(v)
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .ok_or("EINVAL:bad value for z")?
                }
                b'C' => c.no_move = num()? == 1,
                b'U' => c.unicode = num()? == 1,
                b'd' => c.delete = ch(),
                _ => {} // unknown keys are ignored, per the spec
            }
        }
        Ok(c)
    }
}

impl Graphics {
    pub fn images(&self) -> impl Iterator<Item = &Image> {
        self.images.values()
    }

    pub fn image(&self, id: u32) -> Option<&Image> {
        self.images.get(&id)
    }

    pub fn placements(&self) -> &[Placement] {
        &self.placements
    }

    pub fn is_empty(&self) -> bool {
        self.placements.is_empty()
    }

    /// Any virtual placements (so renderers know to look for placeholders).
    pub fn has_virtual(&self) -> bool {
        self.placements.iter().any(|p| p.virtual_)
    }

    /// The virtual placement a placeholder cell refers to: by image id and
    /// placement id (0: the newest virtual placement of that image).
    pub fn virtual_placement(&self, image: u32, placement: u32) -> Option<(&Image, &Placement)> {
        let p =
            self.placements.iter().rev().find(|p| {
                p.virtual_ && p.image == image && (placement == 0 || p.id == placement)
            })?;
        Some((self.images.get(&image)?, p))
    }

    /// Lines scrolled off the top of a screen so far.
    pub fn scrolled(&self, alt: bool) -> i64 {
        self.scrolled[alt as usize]
    }

    fn changed(&mut self) {
        self.generation += 1;
    }

    /// The screen scrolled up by `n` lines (full-screen scroll).
    pub(crate) fn scroll(&mut self, alt: bool, n: usize, history: usize) {
        self.scrolled[alt as usize] += n as i64;
        if self.placements.is_empty() {
            return;
        }
        // Gone once their last row has left the history too.
        let oldest = self.scrolled[alt as usize] - history as i64;
        let before = self.placements.len();
        self.placements.retain(|p| p.virtual_ || p.alt != alt || p.line + p.rows as i64 > oldest);
        if self.placements.len() != before {
            self.drop_unused();
        }
        self.changed();
    }

    /// Primary-screen placements as (index, row, col) in the reflow input
    /// (history first, then the screen), for those still in it.
    pub(crate) fn primary_anchors(
        &self,
        history: usize,
        total: usize,
    ) -> Vec<(usize, usize, usize)> {
        let base = self.scrolled[0] - history as i64;
        self.placements
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.alt && !p.virtual_)
            .filter_map(|(i, p)| {
                let row = p.line - base;
                (row >= 0 && (row as usize) < total).then_some((i, row as usize, p.col))
            })
            .collect()
    }

    /// After a reflow: placement `i` now starts on screen row `row`
    /// (negative: in history) at column `col`.
    pub(crate) fn move_primary(&mut self, i: usize, row: i64, col: usize) {
        if let Some(p) = self.placements.get_mut(i) {
            p.line = self.scrolled[0] + row;
            p.col = col;
            self.generation += 1;
        }
    }

    /// Rows `top..=bottom` of a screen scrolled by `delta` lines (negative:
    /// up). Images entirely inside the region move with it; one pushed out
    /// of the region is removed. Images not inside it stay put.
    pub(crate) fn scroll_region(&mut self, alt: bool, top: usize, bottom: usize, delta: i64) {
        let base = self.scrolled[alt as usize];
        let (t, b) = (top as i64, bottom as i64);
        let before = self.placements.len();
        let mut moved = false;
        self.placements.retain_mut(|p| {
            if p.alt != alt || p.virtual_ {
                return true;
            }
            let (r0, r1) = (p.line - base, p.line - base + p.rows as i64 - 1);
            if r0 < t || r1 > b {
                return true;
            }
            moved = true;
            let n0 = r0 + delta;
            if n0 < t || n0 + p.rows as i64 - 1 > b {
                return false;
            }
            p.line += delta;
            true
        });
        if self.placements.len() != before {
            self.drop_unused();
        }
        if moved {
            self.changed();
        }
    }

    /// Placements visible in a `rows`-row viewport scrolled back by
    /// `display_offset`, bottom z first.
    pub fn visible(
        &self,
        alt: bool,
        rows: usize,
        display_offset: usize,
    ) -> Vec<VisiblePlacement<'_>> {
        let top = self.scrolled[alt as usize] - display_offset as i64;
        let mut out: Vec<VisiblePlacement> = self
            .placements
            .iter()
            .filter(|p| p.alt == alt && !p.virtual_)
            .filter_map(|p| {
                let row = p.line - top;
                if row + p.rows as i64 <= 0 || row >= rows as i64 {
                    return None;
                }
                Some(VisiblePlacement { image: self.images.get(&p.image)?, placement: p, row })
            })
            .collect();
        out.sort_by_key(|v| v.placement.z);
        out
    }

    /// Remove placements that intersect screen rows `top..bottom` (screen
    /// coordinates) of a screen (ED 2), or all of that screen's.
    pub(crate) fn clear_screen(&mut self, alt: bool, rows: Option<(usize, usize)>) {
        let base = self.scrolled[alt as usize];
        let before = self.placements.len();
        self.placements.retain(|p| {
            if p.alt != alt || p.virtual_ {
                return true;
            }
            match rows {
                None => false,
                Some((top, bottom)) => {
                    let (a, b) = (p.line - base, p.line - base + p.rows as i64);
                    b <= top as i64 || a >= bottom as i64
                }
            }
        });
        if before != self.placements.len() {
            self.drop_unused();
            self.changed();
        }
    }

    /// RIS: forget everything.
    pub(crate) fn reset(&mut self) {
        let (cell, known) = (self.cell_px, self.cell_px_known);
        let (gen, key, scrolled) = (self.generation, self.next_key, self.scrolled);
        *self = Graphics::default();
        self.cell_px = cell;
        self.cell_px_known = known;
        self.next_key = key;
        self.scrolled = scrolled;
        self.generation = gen + 1;
    }

    /// Store a decoded image; returns its id.
    pub(crate) fn add_image(
        &mut self,
        id: u32,
        number: u32,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> u32 {
        let transient = id == 0 && number == 0;
        let id = if id == 0 { self.fresh_id() } else { id };
        if self.images.contains_key(&id) {
            // Re-transmitting an id replaces the image and its placements.
            self.placements.retain(|p| p.image != id);
            self.order.retain(|&i| i != id);
        }
        let key = self.next_key;
        self.next_key += 1;
        self.images.insert(
            id,
            Image {
                id,
                number,
                width,
                height,
                rgba: Arc::new(rgba),
                key,
                transient,
                anim: Animation::default(),
            },
        );
        self.order.push(id);
        self.enforce_quota(id);
        self.changed();
        id
    }

    fn fresh_id(&mut self) -> u32 {
        loop {
            self.next_id = self.next_id.wrapping_add(1).max(1 << 31);
            if !self.images.contains_key(&self.next_id) {
                return self.next_id;
            }
        }
    }

    fn enforce_quota(&mut self, keep: u32) {
        let size =
            |i: &Image| i.rgba.len() + i.anim.frames.iter().map(|f| f.rgba.len()).sum::<usize>();
        let mut used: usize = self.images.values().map(size).sum();
        if used <= QUOTA_BYTES {
            return;
        }
        // Oldest images nothing shows go first, then the oldest of all.
        for pass in 0..2 {
            let mut i = 0;
            while used > QUOTA_BYTES && i < self.order.len() {
                let id = self.order[i];
                let shown = self.placements.iter().any(|p| p.image == id);
                if id != keep && (pass == 1 || !shown) {
                    if let Some(img) = self.images.remove(&id) {
                        used -= size(&img);
                    }
                    self.placements.retain(|p| p.image != id);
                    self.order.remove(i);
                } else {
                    i += 1;
                }
            }
        }
    }

    /// Drop transient images that no placement shows any more.
    fn drop_unused(&mut self) {
        let placements = &self.placements;
        let before = self.images.len();
        self.images.retain(|id, img| !img.transient || placements.iter().any(|p| p.image == *id));
        if self.images.len() != before {
            let images = &self.images;
            self.order.retain(|id| images.contains_key(id));
        }
    }

    /// Add a placement, replacing one with the same image and non-zero
    /// placement id.
    pub(crate) fn place(&mut self, p: Placement) {
        if p.id != 0 {
            self.placements.retain(|q| !(q.image == p.image && q.id == p.id));
        }
        self.placements.push(p);
        self.drop_unused();
        self.changed();
    }

    /// A Sixel image at the cursor; older Sixel images it covers entirely
    /// are dropped (programs animate by redrawing in place).
    pub(crate) fn place_sixel(
        &mut self,
        ctx: Ctx,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> (usize, usize) {
        let id = self.add_image(0, 0, width, height, rgba);
        let (cw, ch) = self.cell_px;
        let cols = ((width as f32 / cw).ceil() as usize).max(1);
        let rows = ((height as f32 / ch).ceil() as usize).max(1);
        let line = self.scrolled[ctx.alt as usize] + ctx.row as i64;
        let images = &self.images;
        self.placements.retain(|q| {
            let transient = images.get(&q.image).is_some_and(|i| i.transient);
            !(transient
                && q.alt == ctx.alt
                && q.line >= line
                && q.line + q.rows as i64 <= line + rows as i64
                && q.col >= ctx.col
                && q.col + q.cols <= ctx.col + cols)
        });
        self.place(Placement {
            image: id,
            id: 0,
            alt: ctx.alt,
            line,
            col: ctx.col,
            src: [0, 0, width, height],
            offset: [0, 0],
            cols,
            rows,
            fill: false,
            z: 0,
            virtual_: false,
        });
        (cols, rows)
    }

    /// Run one APC `G` command (`body` is everything after the `G`).
    pub(crate) fn command(&mut self, body: &[u8], ctx: Ctx) -> Effect {
        let (control, payload) = match body.iter().position(|&b| b == b';') {
            Some(i) => (&body[..i], &body[i + 1..]),
            None => (body, &[][..]),
        };
        let cmd = match Cmd::parse(control) {
            Ok(c) => c,
            Err(e) => {
                self.loading = None;
                return Effect {
                    response: reply(&Cmd::parse(b"").unwrap_or_default(), &e),
                    ..Default::default()
                };
            }
        };

        // Continuation of a chunked transmission: only m (and q) matter.
        if let Some(mut l) = self.loading.take() {
            if !cmd.explicit_action {
                l.feed(payload);
                if cmd.more {
                    self.loading = Some(l);
                    return Effect::default();
                }
                if cmd.quiet != 0 {
                    l.cmd.quiet = cmd.quiet;
                }
                l.end();
                return self.finish(l.cmd, l.data, l.failed, ctx);
            }
            // A new command while loading: the old transfer is abandoned.
        }

        match cmd.action {
            b't' | b'T' | b'q' => {
                let mut l = Loading { cmd, data: Vec::new(), pending: Vec::new(), failed: None };
                l.feed(payload);
                if l.cmd.more {
                    self.loading = Some(l);
                    return Effect::default();
                }
                l.end();
                self.finish(l.cmd, l.data, l.failed, ctx)
            }
            b'p' => {
                let id = match self.resolve(&cmd) {
                    Some(id) => id,
                    None => return respond(&cmd, "ENOENT:no such image"),
                };
                self.put(&cmd, id, ctx)
            }
            b'd' => {
                self.delete(&cmd, ctx);
                Effect::default()
            }
            b'f' => {
                if self.resolve(&cmd).is_none() {
                    return respond(&cmd, "ENOENT:no such image");
                }
                let mut l = Loading { cmd, data: Vec::new(), pending: Vec::new(), failed: None };
                l.feed(payload);
                if l.cmd.more {
                    self.loading = Some(l);
                    return Effect::default();
                }
                l.end();
                self.finish(l.cmd, l.data, l.failed, ctx)
            }
            b'a' => self.control(&cmd),
            b'c' => self.compose(&cmd),
            _ => respond(&cmd, "EINVAL:unknown action"),
        }
    }

    /// Image id for `i=` or the newest image with number `I=`.
    fn resolve(&self, cmd: &Cmd) -> Option<u32> {
        if cmd.id != 0 {
            return self.images.contains_key(&cmd.id).then_some(cmd.id);
        }
        if cmd.number != 0 {
            return self
                .order
                .iter()
                .rev()
                .find(|id| self.images.get(id).is_some_and(|i| i.number == cmd.number))
                .copied();
        }
        None
    }

    fn finish(&mut self, cmd: Cmd, data: Vec<u8>, failed: Option<String>, ctx: Ctx) -> Effect {
        if let Some(e) = failed {
            return respond(&cmd, &e);
        }
        let decoded = decode(&cmd, data);
        let (w, h, rgba) = match decoded {
            Ok(d) => d,
            Err(e) => return respond(&cmd, &e),
        };
        if cmd.action == b'q' {
            return respond(&cmd, "OK");
        }
        if cmd.action == b'f' {
            return self.add_frame(&cmd, w, h, rgba);
        }
        let id = self.add_image(cmd.id, cmd.number, w, h, rgba);
        // With only an image number, the reply names the id we chose.
        let cmd = if cmd.number != 0 { Cmd { id, ..cmd } } else { cmd };
        if cmd.action == b'T' {
            return self.put(&cmd, id, ctx);
        }
        respond(&cmd, "OK")
    }

    /// `a=f`: a new frame (or an edit of frame `r`), composited from base
    /// frame `c` or a background color `Y`, with `s x v` data at `x, y`
    /// (alpha-blended, or replacing with `X=1`), shown for `z` ms.
    fn add_frame(&mut self, cmd: &Cmd, w: u32, h: u32, data: Vec<u8>) -> Effect {
        let Some(id) = self.resolve(cmd) else { return respond(cmd, "ENOENT:no such image") };
        let key = self.next_key;
        self.next_key += 1;
        let img = self.images.get_mut(&id).expect("resolved");
        let n = img.frame_count();
        let (iw, ih) = (img.width as usize, img.height as usize);
        let edit = (cmd.rows as usize).checked_sub(1).filter(|&r| r < n);
        if edit.is_none() && n >= MAX_FRAMES {
            return respond(cmd, "ENOSPC:too many frames");
        }
        // The canvas: the frame being edited, a base frame, or a color.
        let mut canvas: Vec<u8> =
            match (edit, (cmd.cols as usize).checked_sub(1).filter(|&c| c < n)) {
                (Some(r), _) => img.frame(r).0.as_ref().clone(),
                (None, Some(c)) => img.frame(c).0.as_ref().clone(),
                (None, None) => {
                    let [r, g, b, a] = cmd.off_y.to_be_bytes();
                    [r, g, b, a].repeat(iw * ih)
                }
            };
        let (x0, y0) = (cmd.x as usize, cmd.y as usize);
        let replace = cmd.off_x == 1;
        for y in 0..(h as usize) {
            let ty = y0 + y;
            if ty >= ih {
                break;
            }
            for x in 0..(w as usize) {
                let tx = x0 + x;
                if tx >= iw {
                    break;
                }
                let s = &data[(y * w as usize + x) * 4..][..4];
                let d = &mut canvas[(ty * iw + tx) * 4..][..4];
                blend(d, s, replace);
            }
        }
        let gap = if cmd.z > 0 {
            cmd.z as u32
        } else if cmd.z < 0 {
            0
        } else {
            DEFAULT_GAP
        };
        match edit {
            Some(0) => {
                img.rgba = Arc::new(canvas);
                img.key = key;
            }
            Some(r) => img.anim.frames[r - 1] = Frame { rgba: Arc::new(canvas), key },
            None => img.anim.frames.push(Frame { rgba: Arc::new(canvas), key }),
        }
        let index = edit.unwrap_or(n);
        if img.anim.gaps.len() <= index {
            img.anim.gaps.resize(index + 1, DEFAULT_GAP);
        }
        if cmd.z != 0 || edit.is_none() {
            img.anim.gaps[index] = gap;
        }
        self.enforce_quota(id);
        self.changed();
        respond(cmd, "OK")
    }

    /// `a=c`: copy a `w x h` rectangle at `x, y` of frame `r` onto frame
    /// `c` at `X, Y`, alpha-blended (or replacing with `C=1`).
    fn compose(&mut self, cmd: &Cmd) -> Effect {
        let Some(id) = self.resolve(cmd) else { return respond(cmd, "ENOENT:no such image") };
        let key = self.next_key;
        self.next_key += 1;
        let img = self.images.get_mut(&id).expect("resolved");
        let n = img.frame_count();
        let (Some(src), Some(dst)) = (
            (cmd.rows as usize).checked_sub(1).filter(|&r| r < n),
            (cmd.cols as usize).checked_sub(1).filter(|&c| c < n),
        ) else {
            return respond(cmd, "ENOENT:no such frame");
        };
        let (iw, ih) = (img.width, img.height);
        let (x, y) = (cmd.x.min(iw), cmd.y.min(ih));
        let w = if cmd.w == 0 { iw - x } else { cmd.w.min(iw - x) };
        let h = if cmd.h == 0 { ih - y } else { cmd.h.min(ih - y) };
        let (dx, dy) = (cmd.off_x, cmd.off_y);
        if dx + w > iw || dy + h > ih {
            return respond(cmd, "EINVAL:rectangle out of bounds");
        }
        let from = img.frame(src).0.clone();
        let mut canvas = img.frame(dst).0.as_ref().clone();
        let iw = iw as usize;
        for row in 0..h as usize {
            for col in 0..w as usize {
                let s = &from[((y as usize + row) * iw + x as usize + col) * 4..][..4];
                let d = &mut canvas[((dy as usize + row) * iw + dx as usize + col) * 4..][..4];
                blend(d, s, cmd.no_move);
            }
        }
        match dst {
            0 => {
                img.rgba = Arc::new(canvas);
                img.key = key;
            }
            k => img.anim.frames[k - 1] = Frame { rgba: Arc::new(canvas), key },
        }
        self.changed();
        respond(cmd, "OK")
    }

    /// `a=a`: playback state `s` (1 stop, 2 loading, 3 run), current frame
    /// `c`, loops `v`, gap `z` of frame `r`.
    fn control(&mut self, cmd: &Cmd) -> Effect {
        let Some(id) = self.resolve(cmd) else { return respond(cmd, "ENOENT:no such image") };
        let img = self.images.get_mut(&id).expect("resolved");
        let n = img.frame_count();
        let now = std::time::Instant::now();
        // Freeze where it is before changing anything.
        img.anim.current = img.frame_at(now);
        img.anim.epoch = now;
        if let Some(r) = (cmd.rows as usize).checked_sub(1).filter(|&r| r < n) {
            if img.anim.gaps.len() <= r {
                img.anim.gaps.resize(r + 1, DEFAULT_GAP);
            }
            if cmd.z != 0 {
                img.anim.gaps[r] = cmd.z.max(0) as u32;
            }
        }
        if let Some(c) = (cmd.cols as usize).checked_sub(1).filter(|&c| c < n) {
            img.anim.current = c;
        }
        match cmd.width {
            1 => img.anim.state = PlayState::Stopped,
            2 => img.anim.state = PlayState::Loading,
            3 => img.anim.state = PlayState::Running,
            _ => {}
        }
        // v: 0 leaves it, 1 loops forever, n plays n-1 loops.
        match cmd.height {
            0 => {}
            1 => img.anim.loops = 0,
            v => img.anim.loops = v - 1,
        }
        self.changed();
        respond(cmd, "OK")
    }

    /// Any visible image is playing an animation.
    pub fn animating(&self) -> bool {
        self.placements.iter().any(|p| self.images.get(&p.image).is_some_and(Image::animating))
    }

    fn put(&mut self, cmd: &Cmd, id: u32, ctx: Ctx) -> Effect {
        let img = &self.images[&id];
        let (iw, ih) = (img.width, img.height);
        // Source rectangle, clamped to the image.
        let x = cmd.x.min(iw);
        let y = cmd.y.min(ih);
        let w = if cmd.w == 0 { iw - x } else { cmd.w.min(iw - x) };
        let h = if cmd.h == 0 { ih - y } else { cmd.h.min(ih - y) };
        let (cw, ch) = self.cell_px;
        let (ox, oy) = (cmd.off_x.min(cw as u32), cmd.off_y.min(ch as u32));
        let natural_cols = (((w + ox) as f32 / cw).ceil() as usize).max(1);
        let natural_rows = (((h + oy) as f32 / ch).ceil() as usize).max(1);
        let aspect = if h == 0 { 1.0 } else { w as f32 / h as f32 };
        let (cols, rows, fill) = match (cmd.cols, cmd.rows) {
            (0, 0) => (natural_cols, natural_rows, false),
            (c, 0) => {
                let px_h = c as f32 * cw / aspect.max(1e-6);
                (c as usize, ((px_h / ch).ceil() as usize).max(1), true)
            }
            (0, r) => {
                let px_w = r as f32 * ch * aspect;
                (((px_w / cw).ceil() as usize).max(1), r as usize, true)
            }
            (c, r) => (c as usize, r as usize, true),
        };
        let cols = cols.min(10_000);
        let rows = rows.min(10_000);
        self.place(Placement {
            image: id,
            id: cmd.placement,
            alt: ctx.alt,
            line: self.scrolled[ctx.alt as usize] + ctx.row as i64,
            col: ctx.col,
            src: [x, y, w, h],
            offset: [ox, oy],
            cols,
            rows,
            fill,
            z: cmd.z,
            virtual_: cmd.unicode,
        });
        let mut e = respond(cmd, "OK");
        if !cmd.no_move && !cmd.unicode {
            e.cursor_move = Some((cols, rows));
        }
        e
    }

    fn delete(&mut self, cmd: &Cmd, ctx: Ctx) {
        let base = self.scrolled[ctx.alt as usize];
        let free = cmd.delete.is_ascii_uppercase();
        let hits = |p: &Placement, row: i64, col: i64| {
            let r = p.line - base;
            row >= r
                && row < r + p.rows as i64
                && col >= p.col as i64
                && col < (p.col + p.cols) as i64
        };
        let number_id = (cmd.number != 0).then(|| self.resolve(cmd)).flatten();
        let mut affected: Vec<u32> = Vec::new();
        let screen_rows = ctx.rows as i64;
        self.placements.retain(|p| {
            if p.alt != ctx.alt {
                return true;
            }
            let r = p.line - base;
            let on_screen = r + p.rows as i64 > 0 && r < screen_rows;
            let positional =
                matches!(cmd.delete.to_ascii_lowercase(), b'a' | b'c' | b'p' | b'q' | b'x' | b'y');
            if p.virtual_ && positional {
                return true;
            }
            let gone = match cmd.delete.to_ascii_lowercase() {
                b'a' => on_screen,
                b'i' => p.image == cmd.id && (cmd.placement == 0 || p.id == cmd.placement),
                b'n' => Some(p.image) == number_id && (cmd.placement == 0 || p.id == cmd.placement),
                b'c' => hits(p, ctx.row as i64, ctx.col as i64),
                b'p' => hits(p, cmd.y as i64 - 1, cmd.x as i64 - 1),
                b'q' => hits(p, cmd.y as i64 - 1, cmd.x as i64 - 1) && p.z == cmd.z,
                b'x' => {
                    let c = cmd.x as i64 - 1;
                    c >= p.col as i64 && c < (p.col + p.cols) as i64
                }
                b'y' => {
                    let row = cmd.y as i64 - 1;
                    row >= r && row < r + p.rows as i64
                }
                b'z' => p.z == cmd.z,
                b'r' => (cmd.x..=cmd.y).contains(&p.image),
                _ => false,
            };
            if gone {
                affected.push(p.image);
            }
            !gone
        });
        if free {
            // Uppercase: also free image data nothing shows any more.
            if cmd.delete == b'I' && affected.is_empty() {
                affected.push(cmd.id);
            }
            if cmd.delete == b'R' {
                affected.extend(self.images.keys().filter(|i| (cmd.x..=cmd.y).contains(i)));
            }
            for id in affected {
                if !self.placements.iter().any(|p| p.image == id) {
                    self.images.remove(&id);
                    self.order.retain(|&i| i != id);
                }
            }
        }
        self.drop_unused();
        self.changed();
    }
}

impl Loading {
    fn feed(&mut self, payload: &[u8]) {
        if self.failed.is_some() {
            return;
        }
        if self.data.len() + self.pending.len() + payload.len() > MAX_TRANSMISSION {
            self.failed = Some("EFBIG:transmission too large".into());
            self.data = Vec::new();
            return;
        }
        self.pending.extend(payload.iter().filter(|b| !b.is_ascii_whitespace()));
        let whole = self.pending.len() / 4 * 4;
        let quartets: Vec<u8> = self.pending.drain(..whole).collect();
        self.decode(&quartets);
    }

    /// The last chunk arrived: decode an unpadded tail.
    fn end(&mut self) {
        let tail = std::mem::take(&mut self.pending);
        if !tail.is_empty() && self.failed.is_none() {
            self.decode(&tail);
        }
    }

    fn decode(&mut self, b64: &[u8]) {
        match base64(b64) {
            Some(bytes) => self.data.extend(bytes),
            None => self.failed = Some("EINVAL:bad base64".into()),
        }
    }
}

/// Straight-alpha "over" of pixel `s` onto `d` (or a plain copy).
fn blend(d: &mut [u8], s: &[u8], replace: bool) {
    if replace || s[3] == 255 {
        d.copy_from_slice(s);
    } else if s[3] > 0 {
        let (sa, da) = (s[3] as f32 / 255.0, d[3] as f32 / 255.0);
        let oa = sa + da * (1.0 - sa);
        for c in 0..3 {
            let v = (s[c] as f32 * sa + d[c] as f32 * da * (1.0 - sa)) / oa.max(1e-6);
            d[c] = v.round() as u8;
        }
        d[3] = (oa * 255.0).round() as u8;
    }
}

/// Decode a finished transmission to (width, height, RGBA).
fn decode(cmd: &Cmd, data: Vec<u8>) -> Result<(u32, u32, Vec<u8>), String> {
    let data = match cmd.medium {
        b'd' => data,
        b'f' | b't' => read_file(cmd, &data)?,
        b's' => read_shm(cmd, &data)?,
        _ => return Err("EINVAL:unknown transmission medium".into()),
    };
    let data = match cmd.compression {
        0 => data,
        b'z' => miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&data, MAX_TRANSMISSION)
            .map_err(|_| "EINVAL:zlib data is corrupt".to_string())?,
        _ => return Err("EINVAL:unknown compression".into()),
    };
    let (w, h, rgba) = match cmd.format {
        100 => png_rgba(&data)?,
        24 | 32 => {
            let (w, h) = (cmd.width, cmd.height);
            if w == 0 || h == 0 {
                return Err("EINVAL:width and height (s, v) are required".into());
            }
            check_size(w, h)?;
            let bpp = if cmd.format == 24 { 3 } else { 4 };
            let need = w as usize * h as usize * bpp;
            if data.len() < need {
                return Err(format!("ENODATA:expected {need} bytes, got {}", data.len()));
            }
            let rgba = if bpp == 4 {
                data[..need].to_vec()
            } else {
                data[..need].chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect()
            };
            (w, h, rgba)
        }
        f => return Err(format!("EINVAL:unknown format {f}")),
    };
    Ok((w, h, rgba))
}

fn check_size(w: u32, h: u32) -> Result<(), String> {
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return Err(format!("EINVAL:image size {w}x{h} out of range"));
    }
    Ok(())
}

/// `t=f` / `t=t`: the payload is a path. Like kitty, refuse device and
/// pseudo files; temporary files must look like ours and are deleted.
fn read_file(cmd: &Cmd, path: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek};
    let path = std::str::from_utf8(path).map_err(|_| "EINVAL:path is not UTF-8")?;
    let path = std::path::Path::new(path);
    if ["/proc", "/sys", "/dev"].iter().any(|p| path.starts_with(p)) {
        return Err("EPERM:not a regular file".into());
    }
    if cmd.medium == b't' {
        let in_tmp = path.starts_with(std::env::temp_dir())
            || path.starts_with("/tmp")
            || path.starts_with("/dev/shm");
        if !in_tmp || !path.to_string_lossy().contains("tty-graphics-protocol") {
            return Err("EPERM:temporary file must be in the temp directory and named tty-graphics-protocol".into());
        }
    }
    let meta = std::fs::metadata(path).map_err(|e| format!("ENOENT:{e}"))?;
    if !meta.is_file() {
        return Err("EPERM:not a regular file".into());
    }
    let mut f = std::fs::File::open(path).map_err(|e| format!("ENOENT:{e}"))?;
    if cmd.file_offset > 0 {
        f.seek(std::io::SeekFrom::Start(cmd.file_offset as u64))
            .map_err(|e| format!("EINVAL:{e}"))?;
    }
    let limit = if cmd.size > 0 { cmd.size } else { MAX_TRANSMISSION };
    let mut data = Vec::new();
    f.take(limit as u64).read_to_end(&mut data).map_err(|e| format!("EIO:{e}"))?;
    if cmd.medium == b't' {
        let _ = std::fs::remove_file(path);
    }
    Ok(data)
}

/// `t=s`: the payload names a POSIX shared-memory object, which is read
/// and then unlinked (the client hands it over).
#[cfg(unix)]
fn read_shm(cmd: &Cmd, name: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek};
    use std::os::fd::FromRawFd;
    let name = std::str::from_utf8(name).map_err(|_| "EINVAL:name is not UTF-8")?;
    if name.is_empty() || name[1..].contains('/') || name.contains('\0') {
        return Err("EINVAL:bad shared memory name".into());
    }
    let c = std::ffi::CString::new(name).map_err(|_| "EINVAL:bad shared memory name")?;
    // SAFETY: c is a valid NUL-terminated string; the fd is owned below.
    let fd = unsafe { libc::shm_open(c.as_ptr(), libc::O_RDONLY, 0) };
    if fd < 0 {
        return Err(format!("ENOENT:{}", std::io::Error::last_os_error()));
    }
    // SAFETY: fd was just opened and is not used elsewhere.
    let mut f = unsafe { std::fs::File::from_raw_fd(fd) };
    // SAFETY: as above.
    unsafe { libc::shm_unlink(c.as_ptr()) };
    if cmd.file_offset > 0 {
        f.seek(std::io::SeekFrom::Start(cmd.file_offset as u64))
            .map_err(|e| format!("EINVAL:{e}"))?;
    }
    let limit = if cmd.size > 0 { cmd.size } else { MAX_TRANSMISSION };
    let mut data = Vec::new();
    f.take(limit as u64).read_to_end(&mut data).map_err(|e| format!("EIO:{e}"))?;
    Ok(data)
}

#[cfg(not(unix))]
fn read_shm(_: &Cmd, _: &[u8]) -> Result<Vec<u8>, String> {
    Err("EINVAL:shared memory transmission is not supported here".into())
}

fn png_rgba(data: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| format!("EBADPNG:{e}"))?;
    let (w, h) = (reader.info().width, reader.info().height);
    check_size(w, h)?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("EBADPNG:too large")?];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("EBADPNG:{e}"))?;
    let data = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => {
            data.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect()
        }
        png::ColorType::Grayscale => data.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("EBADPNG:indexed PNG was not expanded".into()),
    };
    Ok((info.width, info.height, rgba))
}

fn respond(cmd: &Cmd, msg: &str) -> Effect {
    Effect { response: reply(cmd, msg), cursor_move: None }
}

/// `ESC _ G i=..[,I=..][,p=..] ; msg ESC \`, honoring `q` and only when
/// the client gave an id or number.
fn reply(cmd: &Cmd, msg: &str) -> Vec<u8> {
    let ok = msg == "OK";
    if (cmd.id == 0 && cmd.number == 0) || (ok && cmd.quiet >= 1) || cmd.quiet >= 2 {
        return Vec::new();
    }
    let mut keys = vec![format!("i={}", cmd.id)];
    if cmd.number != 0 {
        keys.push(format!("I={}", cmd.number));
    }
    if cmd.placement != 0 {
        keys.push(format!("p={}", cmd.placement));
    }
    format!("\x1b_G{};{msg}\x1b\\", keys.join(",")).into_bytes()
}

/// Standard (or URL-safe) base64, decoded four symbols at a time so
/// padding may end any quartet (some clients pad every chunk).
pub(crate) fn base64(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    for quartet in input.chunks(4) {
        let (mut acc, mut bits) = (0u32, 0);
        for &b in quartet {
            let v = match b {
                b'A'..=b'Z' => b - b'A',
                b'a'..=b'z' => b - b'a' + 26,
                b'0'..=b'9' => b - b'0' + 52,
                b'+' | b'-' => 62,
                b'/' | b'_' => 63,
                b'=' => break,
                _ => return None,
            };
            acc = (acc << 6) | v as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
    }
    Some(out)
}
