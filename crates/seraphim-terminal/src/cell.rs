//! Cells and their presentation attributes.

/// A terminal color as requested by the application.
///
/// The Terminal Core never resolves colors to RGB; that is the renderer's
/// job (it owns the palette and the active profile).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum Color {
    #[default]
    Default,
    /// Palette index 0-255 (0-7 normal, 8-15 bright, 16-255 xterm cube/greys).
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Bit set of cell flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub struct Flags(u16);

impl Flags {
    pub const BOLD: Flags = Flags(1 << 0);
    pub const DIM: Flags = Flags(1 << 1);
    pub const ITALIC: Flags = Flags(1 << 2);
    pub const UNDERLINE: Flags = Flags(1 << 3);
    pub const DOUBLE_UNDERLINE: Flags = Flags(1 << 4);
    pub const CURLY_UNDERLINE: Flags = Flags(1 << 5);
    pub const BLINK: Flags = Flags(1 << 6);
    pub const INVERSE: Flags = Flags(1 << 7);
    pub const HIDDEN: Flags = Flags(1 << 8);
    pub const STRIKETHROUGH: Flags = Flags(1 << 9);
    /// Leading half of a double-width character.
    pub const WIDE: Flags = Flags(1 << 10);
    /// Trailing placeholder cell of a double-width character.
    pub const WIDE_SPACER: Flags = Flags(1 << 11);
    /// Blank left at the end of a row because a wide character did not fit
    /// and wrapped to the next row. Not part of the text (copying or
    /// reflowing the line skips it).
    pub const WIDE_PADDING: Flags = Flags(1 << 12);
    /// SGR 53.
    pub const OVERLINE: Flags = Flags(1 << 13);
    /// SGR 4:4.
    pub const DOTTED_UNDERLINE: Flags = Flags(1 << 14);
    /// SGR 4:5.
    pub const DASHED_UNDERLINE: Flags = Flags(1 << 15);

    pub const ALL_UNDERLINES: Flags = Flags(
        Self::UNDERLINE.0
            | Self::DOUBLE_UNDERLINE.0
            | Self::CURLY_UNDERLINE.0
            | Self::DOTTED_UNDERLINE.0
            | Self::DASHED_UNDERLINE.0,
    );

    pub const fn empty() -> Self {
        Flags(0)
    }
    pub const fn bits(self) -> u16 {
        self.0
    }
    pub const fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }
    pub const fn intersects(self, other: Flags) -> bool {
        self.0 & other.0 != 0
    }
    pub fn insert(&mut self, other: Flags) {
        self.0 |= other.0;
    }
    pub fn remove(&mut self, other: Flags) {
        self.0 &= !other.0;
    }
}

impl std::ops::BitOr for Flags {
    type Output = Flags;
    fn bitor(self, rhs: Flags) -> Flags {
        Flags(self.0 | rhs.0)
    }
}

/// The underline color and hyperlink of a cell, which few cells have:
/// an index into a table the terminal keeps, so that a cell is 16 bytes
/// instead of 24. Read them with [`crate::Terminal::underline_color`] and
/// [`crate::Terminal::link`]. The default means neither. Equal attributes
/// have equal indexes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub struct Extra(pub(crate) u16);

/// Graphic rendition applied to a cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub struct Attrs {
    pub fg: Color,
    pub bg: Color,
    pub flags: Flags,
    /// Underline color and hyperlink.
    pub extra: Extra,
}

/// The table behind [`Extra`]: entry 0 is (no color, no link).
#[derive(Clone, Debug)]
pub(crate) struct Extras {
    table: Vec<(Color, u32)>,
    index: std::collections::HashMap<(Color, u32), u16>,
    /// Bumped when entries move (see [`Extras::compact`]).
    pub generation: u64,
}

impl Default for Extras {
    fn default() -> Self {
        Extras { table: vec![(Color::Default, 0)], index: Default::default(), generation: 0 }
    }
}

impl Extras {
    /// Underline color and hyperlink id of `e`.
    #[inline]
    pub fn get(&self, e: Extra) -> (Color, u32) {
        self.table.get(e.0 as usize).copied().unwrap_or((Color::Default, 0))
    }

    /// The index for (`underline`, `link`); `None` when the table is full.
    pub fn intern(&mut self, underline: Color, link: u32) -> Option<Extra> {
        if (underline, link) == (Color::Default, 0) {
            return Some(Extra(0));
        }
        if let Some(&i) = self.index.get(&(underline, link)) {
            return Some(Extra(i));
        }
        let i = u16::try_from(self.table.len()).ok()?;
        self.table.push((underline, link));
        self.index.insert((underline, link), i);
        Some(Extra(i))
    }

    /// Keep only the entries `used` marks (indexed like the table) and
    /// renumber them: returns the new index of each old one.
    pub fn compact(&mut self, used: &[bool]) -> Vec<u16> {
        let mut map = vec![0u16; self.table.len()];
        let old = std::mem::take(&mut self.table);
        self.table.push((Color::Default, 0));
        self.index.clear();
        for (i, entry) in old.into_iter().enumerate().skip(1) {
            if used.get(i).copied().unwrap_or(false) {
                map[i] = self.table.len() as u16;
                self.index.insert(entry, self.table.len() as u16);
                self.table.push(entry);
            }
        }
        self.generation += 1;
        map
    }

    pub fn len(&self) -> usize {
        self.table.len()
    }
}

/// One character cell of the grid.
///
/// Deliberately `Copy`: blanking, scrolling and overwriting cells must be
/// plain memory operations (see the throughput example). The rare zero-width
/// combining marks live beside the cells, in [`crate::Row`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub attrs: Attrs,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { ch: ' ', attrs: Attrs::default() }
    }
}

impl Cell {
    /// A blank cell for erase operations. Following xterm's
    /// background-color-erase behaviour, only the background survives.
    pub fn blank(template: &Attrs) -> Self {
        Cell { ch: ' ', attrs: Attrs { bg: template.bg, ..Attrs::default() } }
    }

    pub fn is_wide(&self) -> bool {
        self.attrs.flags.contains(Flags::WIDE)
    }

    pub fn is_wide_spacer(&self) -> bool {
        self.attrs.flags.contains(Flags::WIDE_SPACER)
    }

    pub fn is_wide_padding(&self) -> bool {
        self.attrs.flags.contains(Flags::WIDE_PADDING)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cell_is_16_bytes() {
        assert_eq!(std::mem::size_of::<Cell>(), 16);
    }

    #[test]
    fn extras_intern_and_compact() {
        let mut e = Extras::default();
        let red = Color::Indexed(1);
        assert_eq!(e.intern(Color::Default, 0), Some(Extra(0)));
        let a = e.intern(red, 0).unwrap();
        let b = e.intern(Color::Default, 7).unwrap();
        assert_eq!(e.intern(red, 0), Some(a));
        assert_eq!(e.get(b), (Color::Default, 7));
        // Drop `a`: `b` moves down.
        let mut used = vec![false; e.len()];
        used[b.0 as usize] = true;
        let map = e.compact(&used);
        assert_eq!(e.get(Extra(map[b.0 as usize])), (Color::Default, 7));
        assert_eq!(e.len(), 2);
    }
}
