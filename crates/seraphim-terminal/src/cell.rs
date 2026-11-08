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

    pub const ALL_UNDERLINES: Flags =
        Flags(Self::UNDERLINE.0 | Self::DOUBLE_UNDERLINE.0 | Self::CURLY_UNDERLINE.0);

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

/// Graphic rendition applied to a cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub struct Attrs {
    pub fg: Color,
    pub bg: Color,
    pub underline_color: Color,
    pub flags: Flags,
    /// OSC 8 hyperlink id; 0 means "no link". Resolve with
    /// [`crate::Terminal::hyperlink`].
    pub link: u32,
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
}
