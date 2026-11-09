//! Re-wrapping soft-wrapped lines to a new width (resize reflow).
//!
//! Rows joined by `Row::wrapped` form one *logical line*. Each logical line
//! is concatenated, trailing blanks dropped, and wrapped again at the new
//! width. Positions that must survive (the cursor, the first row of the
//! old screen) are passed in as [`Anchor`]s and mapped to their new rows.

use crate::cell::{Cell, Flags};
use crate::grid::Row;

/// A position to track through the reflow: row index into the input and a
/// column within that row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Anchor {
    pub row: usize,
    pub col: usize,
}

type Marks = Option<Box<[char]>>;

/// One logical line: its cells (spacers and wrap padding removed) and the
/// anchors that fall inside it, as offsets into `cells`.
struct Logical {
    cells: Vec<(Cell, Marks)>,
    anchors: Vec<(usize, usize)>, // (anchor index, offset)
}

/// Number of cells of `row` that carry content (trailing default blanks
/// dropped, and the padding of a wrapped wide character). On a row whose
/// blanks are a scroll fill, only what was printed counts.
fn content_len(row: &Row) -> usize {
    // Everything past `occupied()` is blank (or scroll fill) by invariant;
    // start there.
    let mut n = row.occupied().min(row.cells.len());
    while n > 0 && (row.cells[n - 1] == Cell::default() || row.cells[n - 1].is_wide_padding()) {
        n -= 1;
    }
    n
}

fn logical_lines(rows: Vec<Row>, anchors: &[Anchor]) -> Vec<Logical> {
    let mut out = Vec::new();
    let mut cur = Logical { cells: Vec::new(), anchors: Vec::new() };
    for (i, mut row) in rows.into_iter().enumerate() {
        let marks = row.take_combining();
        let mut take = if row.wrapped {
            // Interior blanks of a wrapped row are real spaces; only a
            // trailing wide-char padding cell is dropped.
            let n = row.cells.len();
            if n > 0 && row.cells[n - 1].is_wide_padding() {
                n - 1
            } else {
                n
            }
        } else {
            content_len(&row)
        };
        for (k, a) in anchors.iter().enumerate() {
            if a.row == i {
                // Keep blanks up to an anchor (e.g. the space after a prompt).
                take = take.max(a.col.min(row.cells.len()));
                cur.anchors.push((k, cur.cells.len() + a.col));
            }
        }
        for (col, cell) in row.cells.drain(..take.min(row.cells.len())).enumerate() {
            if cell.is_wide_spacer() {
                // Re-created after its lead cell when wrapping again; keep
                // offsets aligned by recording it as a placeholder.
                cur.cells.push((cell, None));
                continue;
            }
            let m = marks.iter().find(|(c, _)| *c as usize == col).map(|(_, m)| m.clone());
            cur.cells.push((cell, m));
        }
        if !row.wrapped {
            out.push(std::mem::replace(
                &mut cur,
                Logical { cells: Vec::new(), anchors: Vec::new() },
            ));
        }
    }
    if !cur.cells.is_empty() || !cur.anchors.is_empty() {
        out.push(cur);
    }
    out
}

/// Reflow `rows` to `cols` columns. Returns the new rows and, for each
/// anchor, its new (row, col).
pub(crate) fn reflow(
    rows: Vec<Row>,
    cols: usize,
    anchors: &[Anchor],
) -> (Vec<Row>, Vec<(usize, usize)>) {
    let cols = cols.max(1);
    let mut out: Vec<Row> = Vec::with_capacity(rows.len());
    let mut mapped = vec![(0, 0); anchors.len()];
    let mut pending: Vec<Row> = Vec::new(); // rows of an unfinished logical line
    let mut pending_start = 0; // input index of pending[0]

    let mut input = rows.into_iter().enumerate().peekable();
    while let Some((i, mut row)) = input.next() {
        // Fast path (almost all history): a standalone row whose content
        // fits the new width, with no anchor past the edge, is resized in
        // place — no per-cell work, no new allocation.
        if pending.is_empty()
            && !row.wrapped
            && content_len(&row) <= cols
            && !anchors.iter().any(|a| a.row == i && a.col >= cols)
        {
            for (k, a) in anchors.iter().enumerate() {
                if a.row == i {
                    mapped[k] = (out.len(), a.col);
                }
            }
            row.resize(cols);
            out.push(row);
            continue;
        }
        if pending.is_empty() {
            pending_start = i;
        }
        let ends = !row.wrapped || input.peek().is_none();
        pending.push(row);
        if ends {
            // Anchors relative to this logical line's rows.
            let local: Vec<(usize, Anchor)> = anchors
                .iter()
                .enumerate()
                .filter(|(_, a)| a.row >= pending_start && a.row < pending_start + pending.len())
                .map(|(k, a)| (k, Anchor { row: a.row - pending_start, col: a.col }))
                .collect();
            let only: Vec<Anchor> = local.iter().map(|(_, a)| *a).collect();
            let base = out.len();
            let (mut rows, m) = rewrap(std::mem::take(&mut pending), cols, &only);
            for ((k, _), (r, c)) in local.iter().zip(m) {
                mapped[*k] = (base + r, c);
            }
            out.append(&mut rows);
        }
    }
    (out, mapped)
}

/// Full reflow of one or more rows forming logical lines.
fn rewrap(rows: Vec<Row>, cols: usize, anchors: &[Anchor]) -> (Vec<Row>, Vec<(usize, usize)>) {
    let mut out: Vec<Row> = Vec::new();
    let mut mapped = vec![(0, 0); anchors.len()];

    for line in logical_lines(rows, anchors) {
        let first_row = out.len();
        let mut cells: Vec<Cell> = Vec::with_capacity(cols);
        let mut marks: Vec<(u16, Box<[char]>)> = Vec::new();
        // Where each input offset landed: (row relative to first_row, col).
        let mut place: Vec<(usize, usize)> = Vec::with_capacity(line.cells.len());
        let mut r = 0;
        let finish = |cells: &mut Vec<Cell>,
                      marks: &mut Vec<(u16, Box<[char]>)>,
                      wrapped: bool,
                      out: &mut Vec<Row>| {
            let mut c = std::mem::take(cells);
            c.resize(cols, Cell::default());
            out.push(Row::from_parts(c, std::mem::take(marks), wrapped));
        };
        for (cell, m) in line.cells {
            if cell.is_wide_spacer() {
                // Its lead has already been placed together with a spacer.
                let lead = place.last().copied().unwrap_or((r, cells.len()));
                place.push((lead.0, lead.1 + 1));
                continue;
            }
            let width = if cell.is_wide() { 2 } else { 1 };
            if width > cols {
                // Cannot be shown at all in a 1-column terminal.
                place.push((r, cells.len()));
                continue;
            }
            if cells.len() + width > cols {
                if cells.len() < cols {
                    let mut pad = Cell::default();
                    pad.attrs.flags.insert(Flags::WIDE_PADDING);
                    cells.push(pad);
                }
                finish(&mut cells, &mut marks, true, &mut out);
                r += 1;
            }
            place.push((r, cells.len()));
            if let Some(m) = m {
                marks.push((cells.len() as u16, m));
            }
            cells.push(cell);
            if width == 2 {
                let mut spacer = Cell { ch: ' ', attrs: cell.attrs };
                spacer.attrs.flags.remove(Flags::WIDE);
                spacer.attrs.flags.insert(Flags::WIDE_SPACER);
                cells.push(spacer);
            }
        }
        let end = (r, cells.len());
        finish(&mut cells, &mut marks, false, &mut out);

        for (k, offset) in line.anchors {
            let (mut rr, mut cc) = place.get(offset).copied().unwrap_or_else(|| {
                // Past the content (e.g. the cursor after a prompt).
                let extra = offset - place.len();
                (end.0, end.1 + extra)
            });
            // Positions past the right edge continue on following rows.
            while cc >= cols {
                cc -= cols;
                rr += 1;
                if first_row + rr >= out.len() {
                    out.last_mut().unwrap().wrapped = true;
                    out.push(Row::from_parts(vec![Cell::default(); cols], Vec::new(), false));
                }
            }
            mapped[k] = (first_row + rr, cc);
        }
    }
    (out, mapped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::Attrs;

    fn row(text: &str, cols: usize, wrapped: bool) -> Row {
        let mut cells = Vec::new();
        for ch in text.chars() {
            let wide = unicode_width::UnicodeWidthChar::width(ch) == Some(2);
            let mut c = Cell { ch, attrs: Attrs::default() };
            if wide {
                c.attrs.flags.insert(Flags::WIDE);
                cells.push(c);
                let mut s = Cell::default();
                s.attrs.flags.insert(Flags::WIDE_SPACER);
                cells.push(s);
            } else {
                cells.push(c);
            }
        }
        cells.resize(cols, Cell::default());
        Row::from_parts(cells, Vec::new(), wrapped)
    }

    fn texts(rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| r.text()).collect()
    }

    #[test]
    fn narrows_and_widens() {
        let rows = vec![row("hello world", 12, false)];
        let (out, _) = reflow(rows, 5, &[]);
        assert_eq!(texts(&out), ["hello", " worl", "d"]);
        assert!(out[0].wrapped && out[1].wrapped && !out[2].wrapped);
        let (back, _) = reflow(out, 12, &[]);
        assert_eq!(texts(&back), ["hello world"]);
        assert!(!back[0].wrapped);
    }

    #[test]
    fn hard_line_breaks_are_kept() {
        let rows = vec![row("ab", 4, false), row("cd", 4, false)];
        let (out, _) = reflow(rows, 10, &[]);
        assert_eq!(texts(&out), ["ab", "cd"]);
    }

    #[test]
    fn wide_chars_never_split() {
        let (out, _) = reflow(vec![row("ab日本", 6, false)], 3, &[]);
        assert_eq!(texts(&out), ["ab", "日", "本"]);
        assert!(out[0].cells[2].is_wide_padding());
        let (back, _) = reflow(out, 6, &[]);
        assert_eq!(texts(&back), ["ab日本"], "padding is not a real space");
    }

    #[test]
    fn anchors_follow_their_text() {
        // Cursor after a prompt with a trailing space, on the second line.
        let rows = vec![row("line one here", 13, false), row("$ ", 13, false)];
        let anchors = [Anchor { row: 1, col: 2 }, Anchor { row: 0, col: 5 }];
        let (out, m) = reflow(rows, 5, &anchors);
        assert_eq!(texts(&out), ["line", "one h", "ere", "$"]);
        assert_eq!(m[0], (3, 2), "cursor keeps the space after the prompt");
        assert_eq!(m[1], (1, 0), "anchor on 'o' of 'one'");
    }

    #[test]
    fn combining_marks_move_with_their_cell() {
        let mut r = row("abcde", 5, false);
        r.push_combining(3, '\u{301}');
        let (out, _) = reflow(vec![r], 3, &[]);
        assert_eq!(texts(&out), ["abc", "d\u{301}e"]);
    }
}
