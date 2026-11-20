//! Trimming the scrollback (freeing the blank cells past each row's
//! content) changes nothing that can be observed.

use seraphim_terminal::{Point, SelectionKind, Terminal};

/// Varied history: short and long lines, colors, wide characters, a
/// combining mark, soft-wrapped lines, a background-colored scroll fill.
fn input() -> Vec<u8> {
    let mut s = String::new();
    for i in 0..300 {
        match i % 7 {
            0 => s.push_str(&format!("line {i}\r\n")),
            1 => s.push_str(&format!("\x1b[31mred {i}\x1b[0m and 漢字 e\u{301}\r\n")),
            2 => s.push_str(&format!("{}\r\n", "long text that wraps ".repeat(5))),
            3 => s.push_str("\r\n"),
            4 => s.push_str(&format!("\x1b[44mblue fill {i}\r\n\x1b[0m")),
            5 => s.push_str(&format!("\x1b]8;;https://x/{i}\x1b\\link\x1b]8;;\x1b\\ tail\r\n")),
            _ => s.push_str(&format!("{i:>30}\r\n")),
        }
    }
    s.into_bytes()
}

fn history(t: &Terminal) -> Vec<String> {
    t.scrollback().iter().map(|r| r.text()).collect()
}

fn select(t: &mut Terminal, kind: SelectionKind, a: Point, b: Point) -> Option<String> {
    t.start_selection(kind, a);
    t.update_selection(b);
    let s = t.selection_text();
    t.clear_selection();
    s
}

fn same(a: &mut Terminal, b: &mut Terminal, what: &str) {
    assert_eq!(history(a), history(b), "{what}: history");
    assert_eq!(a.screen_text(), b.screen_text(), "{what}: screen");
    let first = -(a.scrollback().len() as i64);
    for kind in
        [SelectionKind::Simple, SelectionKind::Word, SelectionKind::Line, SelectionKind::Block]
    {
        for (from, to) in [(first, -1), (first + 5, first + 40), (-30, 3)] {
            let (p, q) = (Point::new(from, 3), Point::new(to, 50));
            assert_eq!(select(a, kind, p, q), select(b, kind, p, q), "{what}: {kind:?} selection");
        }
    }
    for (row, (ra, rb)) in a.scrollback().iter().zip(b.scrollback()).enumerate() {
        let n = ra.cells.len().max(rb.cells.len());
        let blank = seraphim_terminal::Cell::default();
        for c in 0..n {
            let (x, y) = (ra.cells.get(c).unwrap_or(&blank), rb.cells.get(c).unwrap_or(&blank));
            assert_eq!(x, y, "{what}: history row {row} col {c}");
        }
        assert_eq!(ra.wrapped, rb.wrapped, "{what}: history row {row} wrap");
    }
}

#[test]
fn trimmed_scrollback_behaves_the_same() {
    let mut plain = Terminal::with_scrollback(60, 10, 1000);
    let mut trimmed = Terminal::with_scrollback(60, 10, 1000);
    for t in [&mut plain, &mut trimmed] {
        t.advance(&input());
    }
    while trimmed.trim_scrollback(64) {}
    assert!(trimmed.scrollback().iter().any(|r| r.cells.len() < 60), "rows were trimmed");
    same(&mut plain, &mut trimmed, "after trimming");

    // Resizes reflow history; rows pulled back onto the screen must be
    // full width again.
    for (cols, rows) in [(40, 10), (90, 10), (90, 30), (60, 8), (61, 8)] {
        plain.resize(cols, rows);
        trimmed.resize(cols, rows);
        same(&mut plain, &mut trimmed, &format!("resized to {cols}x{rows}"));
        while trimmed.trim_scrollback(1000) {}
        same(&mut plain, &mut trimmed, &format!("trimmed at {cols}x{rows}"));
    }

    // More output: the scrollback is full, so trimmed rows are recycled.
    let mut small_a = Terminal::with_scrollback(60, 10, 50);
    let mut small_b = Terminal::with_scrollback(60, 10, 50);
    for t in [&mut small_a, &mut small_b] {
        t.advance(&input());
    }
    while small_b.trim_scrollback(10) {}
    for t in [&mut small_a, &mut small_b] {
        t.advance(&input());
        t.advance(b"\x1b[2J\x1b[Hafter");
    }
    same(&mut small_a, &mut small_b, "recycled");
}
