//! Text selection over the screen and scrollback (spec §3.3 "Selection
//! state").
//!
//! Points use *absolute line numbers*: screen row `r` is line `r`, and
//! scrollback lines are negative (`-1` is the most recent one). When lines
//! scroll into history the Terminal shifts the selection with them, so a
//! selection stays on the text it was made on.

use crate::grid::Row;

/// A position in the screen + scrollback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Point {
    /// Absolute line: `0..rows` is the screen, negative is scrollback.
    pub line: i64,
    pub col: usize,
}

impl Point {
    pub fn new(line: i64, col: usize) -> Self {
        Point { line, col }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionKind {
    /// Character-wise, following text flow across lines.
    Simple,
    /// Expanded to whole words (double click).
    Word,
    /// Expanded to whole (soft-wrapped) lines (triple click).
    Line,
    /// Rectangular block.
    Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub kind: SelectionKind,
    /// Where the selection started.
    pub anchor: Point,
    /// Where it currently ends (follows the mouse).
    pub head: Point,
}

/// A normalized, expanded selection: `start <= end`, both inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionRange {
    pub start: Point,
    pub end: Point,
    pub block: bool,
}

impl SelectionRange {
    #[inline]
    pub fn contains(&self, p: Point) -> bool {
        if p.line < self.start.line || p.line > self.end.line {
            return false;
        }
        if self.block {
            let (l, r) = (self.start.col.min(self.end.col), self.start.col.max(self.end.col));
            return p.col >= l && p.col <= r;
        }
        (p.line > self.start.line || p.col >= self.start.col)
            && (p.line < self.end.line || p.col <= self.end.col)
    }

    pub fn intersects_lines(&self, from: i64, to: i64) -> bool {
        self.start.line <= to && self.end.line >= from
    }
}

/// Characters that end a word for double-click selection (in addition to
/// whitespace). Matches common terminal defaults.
pub const WORD_SEPARATORS: &str = ",│`|:\"' ()[]{}<>\t";

/// Read access to lines by absolute number.
pub(crate) trait Lines {
    fn row(&self, line: i64) -> Option<&Row>;
    fn first_line(&self) -> i64;
    fn last_line(&self) -> i64;
    fn cols(&self) -> usize;
}

fn is_word_char(c: char) -> bool {
    !c.is_whitespace() && !WORD_SEPARATORS.contains(c)
}

fn char_at(lines: &impl Lines, p: Point) -> Option<char> {
    lines.row(p.line).and_then(|r| r.cells.get(p.col)).map(|c| c.ch)
}

/// Move a point onto the lead cell if it sits on a wide-char spacer.
fn snap_wide(lines: &impl Lines, mut p: Point) -> Point {
    if p.col > 0 {
        if let Some(cell) = lines.row(p.line).and_then(|r| r.cells.get(p.col)) {
            if cell.is_wide_spacer() {
                p.col -= 1;
            }
        }
    }
    p
}

/// Extend `p` to cover the trailing spacer of a wide char.
fn extend_wide(lines: &impl Lines, mut p: Point) -> Point {
    if let Some(row) = lines.row(p.line) {
        if row.cells.get(p.col).is_some_and(|c| c.is_wide()) && p.col + 1 < row.cells.len() {
            p.col += 1;
        }
    }
    p
}

fn word_start(lines: &impl Lines, mut p: Point) -> Point {
    if !char_at(lines, p).is_some_and(is_word_char) {
        return p;
    }
    loop {
        let prev = if p.col > 0 {
            Point::new(p.line, p.col - 1)
        } else if lines.row(p.line - 1).is_some_and(|r| r.wrapped) {
            Point::new(
                p.line - 1,
                lines.row(p.line - 1).map_or(0, |r| r.cells.len().saturating_sub(1)),
            )
        } else {
            return p;
        };
        let prev = snap_wide(lines, prev);
        if !char_at(lines, prev).is_some_and(is_word_char) {
            return p;
        }
        p = prev;
    }
}

fn word_end(lines: &impl Lines, mut p: Point) -> Point {
    if !char_at(lines, p).is_some_and(is_word_char) {
        return extend_wide(lines, p);
    }
    loop {
        let p2 = extend_wide(lines, p);
        let len = lines.row(p2.line).map_or(0, |r| r.cells.len());
        let next = if p2.col + 1 < len {
            Point::new(p2.line, p2.col + 1)
        } else if lines.row(p2.line).is_some_and(|r| r.wrapped) && p2.line < lines.last_line() {
            Point::new(p2.line + 1, 0)
        } else {
            return p2;
        };
        if !char_at(lines, next).is_some_and(is_word_char) {
            return p2;
        }
        p = next;
    }
}

fn line_start(lines: &impl Lines, mut line: i64) -> i64 {
    while line > lines.first_line() && lines.row(line - 1).is_some_and(|r| r.wrapped) {
        line -= 1;
    }
    line
}

fn line_end(lines: &impl Lines, mut line: i64) -> i64 {
    while line < lines.last_line() && lines.row(line).is_some_and(|r| r.wrapped) {
        line += 1;
    }
    line
}

impl Selection {
    /// Normalize and expand according to the selection kind.
    pub(crate) fn range(&self, lines: &impl Lines) -> SelectionRange {
        let (a, b) = if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        };
        let last_col = lines.cols().saturating_sub(1);
        match self.kind {
            SelectionKind::Simple => SelectionRange {
                start: snap_wide(lines, a),
                end: extend_wide(lines, b),
                block: false,
            },
            SelectionKind::Block => {
                let (l, r) = (a.col.min(b.col), a.col.max(b.col));
                SelectionRange {
                    start: Point::new(a.line, l),
                    end: Point::new(b.line, r),
                    block: true,
                }
            }
            SelectionKind::Word => SelectionRange {
                start: word_start(lines, snap_wide(lines, a)),
                end: word_end(lines, snap_wide(lines, b)),
                block: false,
            },
            SelectionKind::Line => SelectionRange {
                start: Point::new(line_start(lines, a.line), 0),
                end: Point::new(line_end(lines, b.line), last_col),
                block: false,
            },
        }
    }
}

/// Extract the text of `range`. Soft-wrapped lines are joined without a
/// newline; trailing blanks on each line are dropped.
pub(crate) fn text(lines: &impl Lines, range: &SelectionRange) -> String {
    let mut out = String::new();
    let mut line = range.start.line.max(lines.first_line());
    let last = range.end.line.min(lines.last_line());
    while line <= last {
        let Some(row) = lines.row(line) else { break };
        let len = row.cells.len();
        let (from, to) = if range.block {
            (range.start.col.min(range.end.col), range.start.col.max(range.end.col))
        } else {
            (
                if line == range.start.line { range.start.col } else { 0 },
                if line == range.end.line { range.end.col } else { len.saturating_sub(1) },
            )
        };
        let mut piece = String::new();
        for col in from..=to.min(len.saturating_sub(1)) {
            row.push_cell_text(col, &mut piece);
        }
        let joined = !range.block && row.wrapped && line != last && to + 1 >= len;
        if joined {
            out.push_str(&piece);
        } else {
            out.push_str(piece.trim_end_matches(' '));
            if line != last {
                out.push('\n');
            }
        }
        line += 1;
    }
    out
}
