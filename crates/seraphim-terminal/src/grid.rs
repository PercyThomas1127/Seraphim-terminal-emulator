//! The character grid.

use crate::cell::{Attrs, Cell, Color, Flags};

/// Whether `Cell::blank(t)` is the all-default cell.
#[inline]
fn default_blank(t: &Attrs) -> bool {
    t.bg == Color::Default
}

/// One line of the grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// True when the line was soft-wrapped into the next one (needed for
    /// correct copy/paste and, later, reflow on resize).
    pub wrapped: bool,
    /// Zero-width combining marks, as `(column, marks)`. Almost always
    /// empty, so it costs one empty `Vec` per row.
    combining: Vec<(u16, Box<[char]>)>,
    /// Invariant: `cells[occ..]` are all `Cell::default()`. Lets `reset`
    /// clear only the written prefix (a 70-char line in a 200-col terminal
    /// clears 70 cells, not 200).
    occ: usize,
}

impl Row {
    pub fn new(cols: usize, template: &Attrs) -> Self {
        let occ = if default_blank(template) { 0 } else { cols };
        Row { cells: vec![Cell::blank(template); cols], wrapped: false, combining: Vec::new(), occ }
    }

    /// Build a row from cells and their combining marks (used by reflow).
    pub(crate) fn from_parts(
        cells: Vec<Cell>,
        combining: Vec<(u16, Box<[char]>)>,
        wrapped: bool,
    ) -> Self {
        let occ = cells.iter().rposition(|c| *c != Cell::default()).map_or(0, |i| i + 1);
        Row { cells, wrapped, combining, occ }
    }

    /// Cells beyond this index are all `Cell::default()`.
    #[inline]
    pub(crate) fn occupied(&self) -> usize {
        self.occ
    }

    /// Take the combining marks out (used by reflow).
    pub(crate) fn take_combining(&mut self) -> Vec<(u16, Box<[char]>)> {
        std::mem::take(&mut self.combining)
    }

    /// An empty row (no cells) used as a reusable buffer.
    pub(crate) fn empty(capacity: usize) -> Self {
        Row { cells: Vec::with_capacity(capacity), wrapped: false, combining: Vec::new(), occ: 0 }
    }

    /// Wide-character pairing and the `occ` invariant.
    pub(crate) fn check_invariants(&self) -> Result<(), String> {
        let n = self.cells.len();
        for (i, c) in self.cells.iter().enumerate() {
            if c.is_wide() && !(i + 1 < n && self.cells[i + 1].is_wide_spacer()) {
                return Err(format!("wide char at {i} without spacer"));
            }
            if c.is_wide_spacer() && !(i > 0 && self.cells[i - 1].is_wide()) {
                return Err(format!("orphan spacer at {i}"));
            }
            if i >= self.occ && *c != Cell::default() {
                return Err(format!("cell {i} written beyond occ {}", self.occ));
            }
        }
        if self.combining.iter().any(|(c, _)| *c as usize >= n) {
            return Err("combining mark beyond row end".into());
        }
        Ok(())
    }

    /// Record a write to `cells[..to]` (keeps the `occ` invariant).
    #[inline]
    pub(crate) fn touch(&mut self, to: usize) {
        if to > self.occ {
            self.occ = to.min(self.cells.len());
        }
    }

    /// Combining marks attached to the cell at `col`.
    pub fn combining(&self, col: usize) -> &[char] {
        self.combining.iter().find(|(c, _)| *c as usize == col).map_or(&[], |(_, m)| m)
    }

    /// Append the visible text of cell `col` (wide spacers produce nothing).
    pub fn push_cell_text(&self, col: usize, out: &mut String) {
        let cell = &self.cells[col];
        if cell.is_wide_spacer() || cell.is_wide_padding() {
            return;
        }
        out.push(cell.ch);
        if !self.combining.is_empty() {
            out.extend(self.combining(col).iter());
        }
    }

    pub(crate) fn push_combining(&mut self, col: usize, mark: char) {
        match self.combining.iter_mut().find(|(c, _)| *c as usize == col) {
            // Long enough for the longest emoji sequences (tag flags,
            // families), bounded against "zalgo" floods.
            Some((_, marks)) if marks.len() < 32 => {
                let mut v = marks.to_vec();
                v.push(mark);
                *marks = v.into_boxed_slice();
            }
            Some(_) => {} // bound pathological "zalgo" input
            None => self.combining.push((col as u16, Box::new([mark]))),
        }
    }

    /// Drop marks attached to columns `from..to`.
    #[inline]
    pub(crate) fn clear_combining(&mut self, from: usize, to: usize) {
        if !self.combining.is_empty() {
            self.combining.retain(|(c, _)| (*c as usize) < from || (*c as usize) >= to);
        }
    }

    /// Move marks at or right of `from` by `delta` columns (insert/delete
    /// characters), dropping any that leave `0..cols`.
    pub(crate) fn shift_combining(&mut self, from: usize, delta: isize, cols: usize) {
        if self.combining.is_empty() {
            return;
        }
        self.combining.retain_mut(|(c, _)| {
            if (*c as usize) < from {
                return true;
            }
            let n = *c as isize + delta;
            if n < 0 || n >= cols as isize {
                return false;
            }
            *c = n as u16;
            true
        });
    }

    /// Visible text, with trailing blanks trimmed.
    pub fn text(&self) -> String {
        let mut s = String::with_capacity(self.cells.len());
        for col in 0..self.cells.len() {
            self.push_cell_text(col, &mut s);
        }
        let trimmed = s.trim_end_matches(' ').len();
        s.truncate(trimmed);
        s
    }

    /// Blank the row in place, reusing its allocation.
    pub(crate) fn reset(&mut self, cols: usize, template: &Attrs) {
        if default_blank(template) && self.cells.len() == cols {
            self.cells[..self.occ].fill(Cell::default());
            self.occ = 0;
        } else {
            self.cells.clear();
            self.cells.resize(cols, Cell::blank(template));
            self.occ = if default_blank(template) { 0 } else { cols };
        }
        self.wrapped = false;
        self.combining.clear();
    }

    pub(crate) fn resize(&mut self, cols: usize) {
        self.cells.resize(cols, Cell::default());
        self.occ = self.occ.min(cols);
        self.clear_combining(cols, usize::MAX);
        // Never leave a dangling half of a wide character at the edge.
        if let Some(last) = self.cells.last_mut() {
            if last.is_wide() {
                *last = Cell::default();
            }
        }
    }
}

/// A screen buffer: `rows` lines of `cols` cells.
#[derive(Clone, Debug)]
pub struct Grid {
    cols: usize,
    lines: Vec<Row>,
}

impl Grid {
    pub fn new(cols: usize, rows: usize) -> Self {
        let blank = Attrs::default();
        Grid { cols, lines: (0..rows).map(|_| Row::new(cols, &blank)).collect() }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }
    pub fn rows(&self) -> usize {
        self.lines.len()
    }
    pub fn row(&self, i: usize) -> &Row {
        &self.lines[i]
    }
    pub fn rows_iter(&self) -> impl Iterator<Item = &Row> {
        self.lines.iter()
    }
    pub fn cell(&self, row: usize, col: usize) -> &Cell {
        &self.lines[row].cells[col]
    }

    /// Mutable row access. Conservatively marks the whole row as written,
    /// since the caller may touch any cell.
    pub(crate) fn row_mut(&mut self, i: usize) -> &mut Row {
        let r = &mut self.lines[i];
        r.occ = r.cells.len();
        r
    }

    /// Mutable access to one cell (marks it written).
    #[inline]
    pub(crate) fn cell_mut(&mut self, row: usize, col: usize) -> &mut Cell {
        let r = &mut self.lines[row];
        r.touch(col + 1);
        &mut r.cells[col]
    }

    /// Fast path for printing one narrow character onto a narrow cell.
    /// Returns false (and changes nothing) if the target is half of a wide
    /// character, so the caller must take the general path.
    #[inline(always)]
    pub(crate) fn put_narrow(&mut self, row: usize, col: usize, cell: Cell) -> bool {
        let r = &mut self.lines[row];
        let Some(target) = r.cells.get_mut(col) else { return false };
        if target.attrs.flags.intersects(Flags::WIDE | Flags::WIDE_SPACER) {
            return false;
        }
        *target = cell;
        if col >= r.occ {
            r.occ = col + 1;
        }
        if !r.combining.is_empty() {
            r.clear_combining(col, col + 1);
        }
        true
    }

    /// Write access that does not bump `occ`: only for writing
    /// `Cell::default()`.
    #[inline]
    pub(crate) fn cell_mut_untracked(&mut self, row: usize, col: usize) -> &mut Cell {
        &mut self.lines[row].cells[col]
    }

    /// Mutable access to a row's combining marks without disturbing `occ`.
    #[inline]
    pub(crate) fn clear_combining(&mut self, row: usize, from: usize, to: usize) {
        self.lines[row].clear_combining(from, to);
    }

    /// Scroll rows `top..=bottom` up by `n`. Each row leaving the top is
    /// handed to `evict` (which may swap it out, e.g. into scrollback, and
    /// leave a recycled buffer behind) before being reset to blank at the
    /// bottom. No allocation in the steady state.
    pub(crate) fn scroll_up(
        &mut self,
        top: usize,
        bottom: usize,
        n: usize,
        t: &Attrs,
        mut evict: impl FnMut(&mut Row),
    ) {
        let n = n.min(bottom + 1 - top);
        // Evict the departing rows first, while they are still at the top.
        for row in &mut self.lines[top..top + n] {
            evict(row);
            row.reset(self.cols, t);
        }
        self.lines[top..=bottom].rotate_left(n);
    }

    /// Scroll rows `top..=bottom` down by `n`, discarding rows pushed past
    /// `bottom` and inserting blanks at `top`.
    pub(crate) fn scroll_down(&mut self, top: usize, bottom: usize, n: usize, t: &Attrs) {
        let n = n.min(bottom + 1 - top);
        self.lines[top..=bottom].rotate_right(n);
        for row in &mut self.lines[top..top + n] {
            row.reset(self.cols, t);
        }
    }

    pub(crate) fn clear(&mut self, t: &Attrs) {
        for row in &mut self.lines {
            *row = Row::new(self.cols, t);
        }
    }

    /// Resize without reflow. Rows removed from the top are returned so the
    /// caller can move them into scrollback.
    pub(crate) fn resize(&mut self, cols: usize, rows: usize, drop_top: usize) -> Vec<Row> {
        let removed: Vec<Row> = self.lines.drain(..drop_top.min(self.lines.len())).collect();
        self.lines.truncate(rows);
        while self.lines.len() < rows {
            self.lines.push(Row::new(cols, &Attrs::default()));
        }
        for row in &mut self.lines {
            row.resize(cols);
        }
        self.cols = cols;
        removed
    }

    /// Remove all lines (for reflow); the grid is left with no rows.
    pub(crate) fn take_lines(&mut self) -> Vec<Row> {
        std::mem::take(&mut self.lines)
    }

    /// Install reflowed lines (exactly `rows` of `cols` cells).
    pub(crate) fn set_lines(&mut self, cols: usize, lines: Vec<Row>) {
        debug_assert!(lines.iter().all(|r| r.cells.len() == cols));
        self.cols = cols;
        self.lines = lines;
    }

    /// Erase a wide character pair if `col` is either half of one, so a
    /// partial overwrite never leaves an orphaned half on screen.
    /// Like [`Grid::split_wide`], but for an edit *boundary* between
    /// `col - 1` and `col` (insert/delete characters): only a wide
    /// character that straddles the boundary is broken. One that starts at
    /// `col` stays intact and simply moves.
    #[inline]
    pub(crate) fn split_wide_boundary(&mut self, row: usize, col: usize) {
        if self.lines[row].cells[col].attrs.flags.contains(Flags::WIDE_SPACER) {
            self.split_wide_slow(row, col);
        }
    }

    #[inline]
    pub(crate) fn split_wide(&mut self, row: usize, col: usize) {
        let flags = self.lines[row].cells[col].attrs.flags;
        if flags.intersects(Flags::WIDE | Flags::WIDE_SPACER) {
            self.split_wide_slow(row, col);
        }
    }

    #[cold]
    fn split_wide_slow(&mut self, row: usize, col: usize) {
        let cols = self.cols;
        let r = &mut self.lines[row];
        let line = &mut r.cells;
        if line[col].attrs.flags.contains(Flags::WIDE_SPACER) && col > 0 {
            line[col - 1] = Cell::blank(&line[col - 1].attrs);
            line[col] = Cell::blank(&line[col].attrs);
            r.clear_combining(col - 1, col + 1);
        } else if line[col].attrs.flags.contains(Flags::WIDE) && col + 1 < cols {
            line[col + 1] = Cell::blank(&line[col + 1].attrs);
            line[col] = Cell::blank(&line[col].attrs);
            r.clear_combining(col, col + 2);
        }
    }
}
