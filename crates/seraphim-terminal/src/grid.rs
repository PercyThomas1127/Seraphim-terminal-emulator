//! The character grid.

use crate::cell::{Attrs, Cell, Flags};

/// One line of the grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// True when the line was soft-wrapped into the next one (needed for
    /// correct copy/paste and, later, reflow on resize).
    pub wrapped: bool,
}

impl Row {
    pub fn new(cols: usize, template: &Attrs) -> Self {
        Row { cells: vec![Cell::blank(template); cols], wrapped: false }
    }

    /// Visible text, with trailing blanks trimmed.
    pub fn text(&self) -> String {
        let mut s = String::with_capacity(self.cells.len());
        for c in &self.cells {
            c.push_text(&mut s);
        }
        let trimmed = s.trim_end_matches(' ').len();
        s.truncate(trimmed);
        s
    }

    pub(crate) fn resize(&mut self, cols: usize) {
        self.cells.resize(cols, Cell::default());
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

    pub(crate) fn row_mut(&mut self, i: usize) -> &mut Row {
        &mut self.lines[i]
    }
    pub(crate) fn cell_mut(&mut self, row: usize, col: usize) -> &mut Cell {
        &mut self.lines[row].cells[col]
    }

    /// Remove rows `top..=bottom`'s first `n` lines and append `n` blank lines
    /// at `bottom`. Returns the removed rows (oldest first).
    pub(crate) fn scroll_up(&mut self, top: usize, bottom: usize, n: usize, t: &Attrs) -> Vec<Row> {
        let n = n.min(bottom + 1 - top);
        let removed: Vec<Row> = self.lines.drain(top..top + n).collect();
        let at = bottom + 1 - n;
        for _ in 0..n {
            self.lines.insert(at, Row::new(self.cols, t));
        }
        removed
    }

    /// Insert `n` blank lines at `top`, discarding lines pushed past `bottom`.
    pub(crate) fn scroll_down(&mut self, top: usize, bottom: usize, n: usize, t: &Attrs) {
        let n = n.min(bottom + 1 - top);
        self.lines.drain(bottom + 1 - n..=bottom);
        for _ in 0..n {
            self.lines.insert(top, Row::new(self.cols, t));
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

    /// Erase a wide character pair if `col` is either half of one, so a
    /// partial overwrite never leaves an orphaned half on screen.
    pub(crate) fn split_wide(&mut self, row: usize, col: usize) {
        let cols = self.cols;
        let line = &mut self.lines[row].cells;
        if line[col].attrs.flags.contains(Flags::WIDE_SPACER) && col > 0 {
            line[col - 1] = Cell::blank(&line[col - 1].attrs);
            line[col] = Cell::blank(&line[col].attrs);
        } else if line[col].attrs.flags.contains(Flags::WIDE) && col + 1 < cols {
            line[col + 1] = Cell::blank(&line[col + 1].attrs);
            line[col] = Cell::blank(&line[col].attrs);
        }
    }
}
