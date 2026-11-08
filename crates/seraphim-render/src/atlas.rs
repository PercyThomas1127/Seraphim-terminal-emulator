//! Shelf-packed glyph atlas backed by one RGBA8 texture.

pub const ATLAS_SIZE: u32 = 2048;
const PAD: u32 = 1;

/// Location of a glyph in the atlas, in texels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtlasRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

struct Shelf {
    y: u32,
    height: u32,
    cursor: u32,
}

pub struct Atlas {
    size: u32,
    shelves: Vec<Shelf>,
    next_y: u32,
}

impl Atlas {
    pub fn new(size: u32) -> Self {
        Atlas { size, shelves: Vec::new(), next_y: 0 }
    }

    /// Reserve space for a `w` x `h` image; `None` when the atlas is full
    /// (the caller then clears and rebuilds the cache).
    pub fn allocate(&mut self, w: u32, h: u32) -> Option<AtlasRect> {
        if w + PAD > self.size || h + PAD > self.size {
            return None;
        }
        // Best fit: the shortest shelf that is tall enough and has room.
        let best = self
            .shelves
            .iter_mut()
            .filter(|s| {
                s.height >= h && s.height <= h + h / 2 + 2 && s.cursor + w + PAD <= self.size
            })
            .min_by_key(|s| s.height);
        if let Some(shelf) = best {
            let r = AtlasRect { x: shelf.cursor, y: shelf.y, w, h };
            shelf.cursor += w + PAD;
            return Some(r);
        }
        if self.next_y + h + PAD > self.size {
            return None;
        }
        let shelf = Shelf { y: self.next_y, height: h, cursor: w + PAD };
        self.next_y += h + PAD;
        let r = AtlasRect { x: 0, y: shelf.y, w, h };
        self.shelves.push(shelf);
        Some(r)
    }

    pub fn clear(&mut self) {
        self.shelves.clear();
        self.next_y = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_without_overlap_and_reports_full() {
        let mut a = Atlas::new(64);
        let mut rects = Vec::new();
        while let Some(r) = a.allocate(7, 9) {
            rects.push(r);
        }
        assert!(rects.len() >= 40, "{}", rects.len());
        for (i, r) in rects.iter().enumerate() {
            assert!(r.x + r.w <= 64 && r.y + r.h <= 64);
            for s in &rects[i + 1..] {
                let overlap =
                    r.x < s.x + s.w && s.x < r.x + r.w && r.y < s.y + s.h && s.y < r.y + r.h;
                assert!(!overlap, "{r:?} overlaps {s:?}");
            }
        }
        a.clear();
        assert!(a.allocate(7, 9).is_some());
        assert!(a.allocate(100, 1).is_none());
    }
}
