use seraphim_terminal::{Point, SelectionKind, Terminal};

fn term(cols: usize, rows: usize, input: &str) -> Terminal {
    let mut t = Terminal::new(cols, rows);
    t.advance(input.as_bytes());
    t
}

fn history(t: &Terminal) -> Vec<String> {
    t.scrollback().iter().map(|r| r.text()).collect()
}

#[test]
fn narrowing_rewraps_and_widening_rejoins() {
    let mut t = term(20, 5, "a long line of text\r\n$ ");
    t.resize(10, 5);
    assert_eq!(t.screen_text(), "a long lin\ne of text\n$");
    assert_eq!((t.cursor().row, t.cursor().col), (2, 2), "cursor stays after the prompt");
    t.resize(20, 5);
    assert_eq!(t.screen_text(), "a long line of text\n$");
    assert_eq!((t.cursor().row, t.cursor().col), (1, 2));
    // Typing continues where it should.
    t.advance(b"ls");
    assert_eq!(t.screen_text(), "a long line of text\n$ ls");
}

#[test]
fn wrapped_output_from_the_program_rejoins() {
    // The program printed one long line that the terminal wrapped itself.
    let mut t = term(5, 4, "abcdefghij\r\n");
    assert_eq!(t.screen_text(), "abcde\nfghij");
    t.resize(12, 4);
    assert_eq!(t.screen_text(), "abcdefghij");
}

#[test]
fn hard_newlines_are_never_joined() {
    let mut t = term(10, 4, "ab\r\ncd\r\n");
    t.resize(20, 4);
    assert_eq!(t.screen_text(), "ab\ncd");
}

#[test]
fn shrinking_moves_lines_to_history_and_growing_brings_them_back() {
    let mut t = term(10, 4, "1\r\n2\r\n3\r\n4");
    t.resize(10, 2);
    assert_eq!(t.screen_text(), "3\n4");
    assert_eq!(history(&t), ["1", "2"]);
    t.resize(10, 4);
    assert_eq!(t.screen_text(), "1\n2\n3\n4", "taller window pulls history back");
    assert!(history(&t).is_empty());
    assert_eq!(t.cursor().row, 3);
}

#[test]
fn narrowing_keeps_the_cursor_visible() {
    let mut t = term(20, 3, "0123456789012345678\r\nxxxxxxxxxxxxxxxxxxx\r\n$ ");
    t.resize(5, 3);
    let c = t.cursor();
    assert_eq!(t.visible_row(c.row).text(), "$");
    assert_eq!(c.col, 2);
    // Everything else went to history, rewrapped.
    assert_eq!(history(&t).first().map(String::as_str), Some("01234"));
}

#[test]
fn wide_characters_reflow_without_splitting_or_gaining_spaces() {
    let mut t = term(6, 4, "ab日本\r\n");
    t.resize(3, 4);
    assert_eq!(t.screen_text(), "ab\n日\n本");
    t.resize(6, 4);
    assert_eq!(t.screen_text(), "ab日本");
    t.check_invariants().unwrap();
}

#[test]
fn copying_across_a_wide_char_wrap_has_no_padding_space() {
    // "abcd" then 日 does not fit in 5 columns and wraps, leaving padding.
    let mut t = term(5, 3, "abcd日");
    t.start_selection(SelectionKind::Line, Point::new(0, 0));
    assert_eq!(t.selection_text().unwrap(), "abcd日");
}

#[test]
fn line_that_exactly_fills_the_width_keeps_its_pending_wrap() {
    let mut t = term(5, 3, "abcde");
    t.resize(10, 3);
    t.advance(b"f");
    assert_eq!(t.screen_text(), "abcdef", "the next char continues the same line");
}

#[test]
fn colored_blank_lines_are_preserved() {
    let mut t = term(6, 3, "\x1b[44m\x1b[2K\x1b[0m\r\nx");
    t.resize(12, 3);
    let bg = t.screen().cell(0, 3).attrs.bg;
    assert_eq!(bg, seraphim_terminal::Color::Indexed(4));
}

#[test]
fn primary_screen_reflows_while_an_app_uses_the_alternate_screen() {
    let mut t = term(20, 4, "a long line of text\r\n$ \x1b[?1049h\x1b[Hfull screen app");
    t.resize(10, 4);
    assert!(t.is_alternate_screen());
    t.advance(b"\x1b[?1049l");
    assert_eq!(t.screen_text(), "a long lin\ne of text\n$");
    assert_eq!((t.cursor().row, t.cursor().col), (2, 2), "saved cursor follows the reflow");
}

#[test]
fn resize_round_trips_with_scrollback() {
    // Enough room that narrowing (which multiplies rows) loses nothing.
    let mut t = Terminal::with_scrollback(30, 5, 1000);
    for i in 0..40 {
        t.advance(format!("line {i:02} with some words in it\r\n").as_bytes());
    }
    let before: Vec<String> = history(&t);
    let screen_before = t.screen_text();
    for (c, r) in [(10, 5), (7, 3), (50, 9), (30, 5)] {
        t.resize(c, r);
        t.check_invariants().unwrap();
    }
    assert_eq!(history(&t), before);
    assert_eq!(t.screen_text(), screen_before);
}

#[test]
fn scrollback_limit_counts_rows_after_reflow() {
    // Narrowing multiplies rows; the oldest rows beyond the limit are lost.
    let mut t = Terminal::with_scrollback(10, 2, 3);
    t.advance(b"aaaaaaaaaa\r\nbbbbbbbbbb\r\nc\r\nd");
    t.resize(5, 2);
    assert_eq!(t.screen_text(), "c\nd", "the old top line stays on top");
    // 2 history lines became 4 rows; with a limit of 3 the oldest goes.
    assert_eq!(history(&t), ["aaaaa", "bbbbb", "bbbbb"]);
    t.check_invariants().unwrap();
}

/// Colored art wider than the terminal, each line reset before its newline
/// (like ANSI art printed with `cat`): `lines` lines of `width` cells.
fn art(width: usize, lines: usize) -> String {
    let mut s = String::new();
    for l in 0..lines {
        for c in 0..width {
            s.push_str(&format!(
                "\x1b[4{}m{}",
                (l + c) % 7 + 1,
                if c % 3 == 0 { '#' } else { ' ' }
            ));
        }
        s.push_str("\x1b[0m\r\n");
    }
    s
}

/// Every line's cells (history first), as (char, bg), trailing default
/// blanks dropped.
fn cells(t: &Terminal) -> Vec<Vec<(char, seraphim_terminal::Color)>> {
    t.scrollback()
        .iter()
        .chain(t.screen().rows_iter())
        .map(|r| {
            let mut v: Vec<_> = r.cells.iter().map(|c| (c.ch, c.attrs.bg)).collect();
            while v.last().is_some_and(|c| c.0 == ' ' && c.1 == seraphim_terminal::Color::Default) {
                v.pop();
            }
            v
        })
        .filter(|v| !v.is_empty())
        .collect()
}

/// A line that autowraps in the middle of colored text scrolls in a new
/// row filled with that color (background color erase, as in xterm).
/// Widening must not make that fill part of the line: the art, printed at
/// 40 columns and widened to 80, is exactly the art printed at 80.
#[test]
fn widening_does_not_spread_the_scroll_fill_of_a_wrapped_line() {
    let input = art(50, 12);
    let mut narrow = Terminal::with_scrollback(40, 6, 100);
    narrow.advance(input.as_bytes());
    // At 40 columns the fill is visible, as in xterm.
    let cont = narrow.screen().row(narrow.rows() - 2);
    assert_ne!(
        cont.cells[39].attrs.bg,
        seraphim_terminal::Color::Default,
        "BCE fill on the wrapped row"
    );
    narrow.resize(80, 6);
    let mut wide = Terminal::with_scrollback(80, 6, 100);
    wide.advance(input.as_bytes());
    let (a, b) = (cells(&narrow), cells(&wide));
    assert!(a.iter().all(|l| l.len() == 50), "{:?}", a.iter().map(|l| l.len()).collect::<Vec<_>>());
    assert_eq!(a, b);
    // And back: narrowing again rewraps to two rows per line, not more.
    narrow.resize(40, 6);
    assert!(cells(&narrow).iter().all(|l| l.len() <= 40));
}

#[test]
fn narrowing_a_scroll_filled_row_does_not_wrap_the_fill() {
    // A full-width colored fill from scrolling, with a short prompt on it.
    let mut t = Terminal::with_scrollback(40, 3, 100);
    t.advance(b"a\r\nb\r\n\x1b[44m\r\n\x1b[0m$ ");
    t.resize(20, 3);
    let prompt = t.screen().row(t.cursor().row);
    assert_eq!(prompt.text().trim_end(), "$");
    assert!(!prompt.wrapped, "the fill must not wrap into a second row");
}

#[test]
fn explicitly_colored_lines_keep_their_color_after_a_scroll_fill() {
    // A line wraps onto a scroll-filled row, and then the program colors
    // the rest of that row on purpose (erase to end of line): that is
    // content and survives the rejoin.
    let mut t = Terminal::with_scrollback(10, 2, 100);
    t.advance(b"x\r\n\x1b[45m0123456789ab\x1b[K\x1b[0m");
    t.resize(20, 2);
    let row = t.screen().row(t.cursor().row);
    assert_eq!(row.text().trim_end(), "0123456789ab");
    for col in 12..20 {
        assert_eq!(row.cells[col].attrs.bg, seraphim_terminal::Color::Indexed(5), "col {col}");
    }
}
