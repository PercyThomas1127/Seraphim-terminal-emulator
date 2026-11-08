//! Differential conformance testing.
//!
//! The same byte stream is fed to Seraphim's Terminal Core and to
//! `alacritty_terminal` (a mature, independently written emulator) and the
//! resulting screens are compared: text, cursor position and the main
//! attributes. Disagreements are not automatically Seraphim bugs — each is
//! checked against xterm, the reference implementation, and either fixed
//! or recorded in [`KNOWN_DIFFERENCES`] with a reason.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags as AFlags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Color as AColor, NamedColor, Processor};
use seraphim_terminal::{Color, Flags, Terminal};

pub mod gen;
pub mod golden;
pub mod xterm;

/// Intentional or reference-confirmed behaviour differences.
pub const KNOWN_DIFFERENCES: &[(&str, &str)] = &[(
    "CSI 0 T",
    "xterm treats an explicit 0 count for SD as a no-op (bare CSI T scrolls one line). vte \
     reports both forms as the parameter 0, so Seraphim (like alacritty) scrolls one line.",
)];

/// Normalized color for comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum C {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Attributes compared per cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attr {
    pub fg: C,
    pub bg: C,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub strike: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub lines: Vec<String>,
    pub attrs: Vec<Vec<Attr>>,
    pub cursor: (usize, usize),
}

impl Snapshot {
    pub fn text(&self) -> String {
        let mut v = self.lines.clone();
        while v.last().is_some_and(|l| l.is_empty()) {
            v.pop();
        }
        v.join("\n")
    }
}

pub fn seraphim(cols: usize, rows: usize, bytes: &[u8]) -> Snapshot {
    let mut t = Terminal::new(cols, rows);
    t.advance(bytes);
    snapshot_seraphim(&t)
}

pub fn snapshot_seraphim(t: &Terminal) -> Snapshot {
    let g = t.screen();
    let mut lines = Vec::new();
    let mut attrs = Vec::new();
    for row in g.rows_iter() {
        let mut s = String::new();
        let mut a = Vec::new();
        for (col, cell) in row.cells.iter().enumerate() {
            if cell.is_wide_spacer() {
                continue;
            }
            row.push_cell_text(col, &mut s);
            let f = cell.attrs.flags;
            let norm = |c: Color| match c {
                Color::Default => C::Default,
                Color::Indexed(i) => C::Indexed(i),
                Color::Rgb(r, g, b) => C::Rgb(r, g, b),
            };
            a.push(Attr {
                fg: norm(cell.attrs.fg),
                bg: norm(cell.attrs.bg),
                bold: f.contains(Flags::BOLD),
                italic: f.contains(Flags::ITALIC),
                underline: f.intersects(Flags::ALL_UNDERLINES),
                inverse: f.contains(Flags::INVERSE),
                strike: f.contains(Flags::STRIKETHROUGH),
            });
        }
        trim(&mut s, &mut a);
        lines.push(s);
        attrs.push(a);
    }
    let c = t.cursor();
    Snapshot { lines, attrs, cursor: (c.row, c.col) }
}

struct Size(usize, usize);
impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.1
    }
    fn screen_lines(&self) -> usize {
        self.1
    }
    fn columns(&self) -> usize {
        self.0
    }
}

/// A persistent alacritty instance (for streaming input).
pub struct Reference {
    term: Term<VoidListener>,
    parser: Processor,
}

impl Reference {
    pub fn new(cols: usize, rows: usize) -> Self {
        let term = Term::new(Config::default(), &Size(cols, rows), VoidListener);
        Reference { term, parser: Processor::new() }
    }

    pub fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    pub fn snapshot(&self) -> Snapshot {
        let grid = self.term.grid();
        let (cols, rows) = (grid.columns(), grid.screen_lines());
        let mut lines = Vec::new();
        let mut attrs = Vec::new();
        for r in 0..rows {
            let row = &grid[Line(r as i32)];
            let mut s = String::new();
            let mut a = Vec::new();
            for c in 0..cols {
                let cell = &row[Column(c)];
                if cell.flags.intersects(AFlags::WIDE_CHAR_SPACER) {
                    continue;
                }
                if cell.flags.contains(AFlags::LEADING_WIDE_CHAR_SPACER) {
                    s.push(' ');
                } else {
                    // Alacritty records tabs as '\t' cells (for copying).
                    s.push(if cell.c == '\t' { ' ' } else { cell.c });
                    if let Some(z) = cell.zerowidth() {
                        s.extend(z.iter());
                    }
                }
                let f = cell.flags;
                a.push(Attr {
                    fg: norm_alacritty(cell.fg, true),
                    bg: norm_alacritty(cell.bg, false),
                    bold: f.contains(AFlags::BOLD),
                    italic: f.contains(AFlags::ITALIC),
                    underline: f.intersects(
                        AFlags::UNDERLINE | AFlags::DOUBLE_UNDERLINE | AFlags::UNDERCURL,
                    ),
                    inverse: f.contains(AFlags::INVERSE),
                    strike: f.contains(AFlags::STRIKEOUT),
                });
            }
            trim(&mut s, &mut a);
            lines.push(s);
            attrs.push(a);
        }
        let p = grid.cursor.point;
        Snapshot { lines, attrs, cursor: (p.line.0 as usize, p.column.0) }
    }
}

pub fn alacritty(cols: usize, rows: usize, bytes: &[u8]) -> Snapshot {
    let mut r = Reference::new(cols, rows);
    r.advance(bytes);
    r.snapshot()
}

fn norm_alacritty(c: AColor, fg: bool) -> C {
    match c {
        AColor::Spec(rgb) => C::Rgb(rgb.r, rgb.g, rgb.b),
        AColor::Indexed(i) => C::Indexed(i),
        AColor::Named(n) => {
            let i = n as usize;
            match n {
                NamedColor::Foreground if fg => C::Default,
                NamedColor::Background if !fg => C::Default,
                NamedColor::Foreground | NamedColor::Background => C::Default,
                _ if i < 16 => C::Indexed(i as u8),
                // Dim/bright foreground variants alacritty creates.
                _ => C::Default,
            }
        }
    }
}

/// Drop trailing default blanks so both sides agree on line length.
fn trim(s: &mut String, a: &mut Vec<Attr>) {
    let blank = |x: &Attr| x.bg == C::Default && !x.inverse && !x.underline && !x.strike;
    while s.ends_with(' ') && a.last().is_some_and(blank) {
        s.pop();
        a.pop();
    }
}

/// Human-readable description of the first differences, or `None`.
pub fn diff(ours: &Snapshot, reference: &Snapshot) -> Option<String> {
    let mut out = Vec::new();
    if ours.cursor != reference.cursor {
        out.push(format!("cursor: seraphim {:?} vs alacritty {:?}", ours.cursor, reference.cursor));
    }
    for (i, (a, b)) in ours.lines.iter().zip(&reference.lines).enumerate() {
        if a != b {
            out.push(format!("row {i:2} text:\n  seraphim : {a:?}\n  alacritty: {b:?}"));
        } else if ours.attrs[i] != reference.attrs[i] {
            let col = ours.attrs[i].iter().zip(&reference.attrs[i]).position(|(x, y)| x != y);
            match col {
                Some(c) => out.push(format!(
                    "row {i:2} col {c} attrs:\n  seraphim : {:?}\n  alacritty: {:?}",
                    ours.attrs[i][c], reference.attrs[i][c]
                )),
                None => out.push(format!(
                    "row {i:2} attr run length: {} vs {}",
                    ours.attrs[i].len(),
                    reference.attrs[i].len()
                )),
            }
        }
        if out.len() >= 6 {
            break;
        }
    }
    (!out.is_empty()).then(|| out.join("\n"))
}
