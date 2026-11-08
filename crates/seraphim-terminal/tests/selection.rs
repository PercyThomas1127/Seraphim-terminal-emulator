use seraphim_terminal::input::{encode_mouse, Mods, MouseAction, MouseButton};
use seraphim_terminal::{Point, SelectionKind, Terminal};

fn term(cols: usize, rows: usize, input: &str) -> Terminal {
    let mut t = Terminal::new(cols, rows);
    t.advance(input.as_bytes());
    t
}

fn select(t: &mut Terminal, kind: SelectionKind, a: (i64, usize), b: (i64, usize)) -> String {
    t.start_selection(kind, Point::new(a.0, a.1));
    t.update_selection(Point::new(b.0, b.1));
    t.selection_text().unwrap_or_default()
}

// ---- viewport -----------------------------------------------------------

#[test]
fn viewport_scrolls_into_history_and_clamps() {
    let mut t = term(10, 3, "1\r\n2\r\n3\r\n4\r\n5");
    assert_eq!(t.display_offset(), 0);
    assert_eq!(t.visible_row(0).text(), "3");
    t.scroll_display(1);
    assert_eq!(t.visible_row(0).text(), "2");
    assert_eq!(t.visible_row(2).text(), "4");
    t.scroll_display(100);
    assert_eq!(t.display_offset(), 2, "clamped to scrollback length");
    assert_eq!(t.visible_row(0).text(), "1");
    t.scroll_display(-100);
    assert_eq!(t.display_offset(), 0);
}

#[test]
fn viewport_stays_on_the_same_text_while_output_arrives() {
    let mut t = term(10, 3, "1\r\n2\r\n3\r\n4\r\n5");
    t.scroll_display(2);
    assert_eq!(t.visible_row(0).text(), "1");
    t.advance(b"\r\n6\r\n7");
    assert_eq!(t.visible_row(0).text(), "1", "view must not drift");
    t.scroll_to_bottom();
    assert_eq!(t.screen_text(), "5\n6\n7");
}

#[test]
fn alternate_screen_has_no_viewport_scrolling() {
    let mut t = term(10, 3, "1\r\n2\r\n3\r\n4\r\n5");
    t.scroll_display(1);
    t.advance(b"\x1b[?1049h");
    assert_eq!(t.display_offset(), 0);
    t.scroll_display(5);
    assert_eq!(t.display_offset(), 0);
}

// ---- selection ----------------------------------------------------------

#[test]
fn simple_selection_across_lines() {
    let mut t = term(10, 3, "hello\r\nworld");
    assert_eq!(select(&mut t, SelectionKind::Simple, (0, 1), (1, 2)), "ello\nwor");
    // Reversed drag gives the same text.
    assert_eq!(select(&mut t, SelectionKind::Simple, (1, 2), (0, 1)), "ello\nwor");
    // Trailing blanks of a full-line selection are trimmed.
    assert_eq!(select(&mut t, SelectionKind::Simple, (0, 0), (0, 9)), "hello");
}

#[test]
fn soft_wrapped_lines_join_without_newline() {
    let mut t = term(5, 3, "abcdefgh");
    assert!(t.screen().row(0).wrapped);
    assert_eq!(select(&mut t, SelectionKind::Simple, (0, 0), (1, 4)), "abcdefgh");
    assert_eq!(select(&mut t, SelectionKind::Line, (1, 1), (1, 1)), "abcdefgh");
}

#[test]
fn word_selection() {
    let mut t = term(40, 2, "cd /usr/share/fonts && ls (foo) x");
    assert_eq!(select(&mut t, SelectionKind::Word, (0, 6), (0, 6)), "/usr/share/fonts");
    assert_eq!(select(&mut t, SelectionKind::Word, (0, 28), (0, 28)), "foo");
    // Dragging from one word to another selects both whole words.
    assert_eq!(select(&mut t, SelectionKind::Word, (0, 1), (0, 4)), "cd /usr/share/fonts");
}

#[test]
fn word_selection_follows_soft_wraps() {
    let mut t = term(6, 3, "aa foobarbaz zz");
    assert_eq!(select(&mut t, SelectionKind::Word, (1, 1), (1, 1)), "foobarbaz");
}

#[test]
fn wide_characters_are_selected_whole() {
    let mut t = term(10, 1, "a日本b");
    // Clicking the spacer half of 日 still selects the whole glyph.
    assert_eq!(select(&mut t, SelectionKind::Simple, (0, 2), (0, 3)), "日本");
}

#[test]
fn block_selection() {
    let mut t = term(10, 3, "abcdef\r\nghijkl\r\nmnopqr");
    assert_eq!(select(&mut t, SelectionKind::Block, (0, 1), (2, 3)), "bcd\nhij\nnop");
}

#[test]
fn selection_includes_scrollback_and_follows_scrolling() {
    let mut t = term(10, 2, "one\r\ntwo\r\nthree");
    // "one" is in scrollback at line -1.
    assert_eq!(select(&mut t, SelectionKind::Simple, (-1, 0), (0, 2)), "one\ntwo");
    t.advance(b"\r\nfour");
    let range = t.selection_range().expect("selection survives scrolling");
    assert_eq!((range.start.line, range.end.line), (-2, -1));
    assert_eq!(t.selection_text().unwrap(), "one\ntwo");
}

#[test]
fn selection_is_cleared_when_its_text_changes() {
    let mut t = term(10, 3, "hello\r\nworld\r\n");
    select(&mut t, SelectionKind::Simple, (0, 0), (0, 4));
    t.advance(b"elsewhere");
    assert!(t.selection().is_some(), "writes on other lines keep it");
    t.advance(b"\x1b[1;1HX");
    assert!(t.selection().is_none(), "overwriting the selected line drops it");

    select(&mut t, SelectionKind::Simple, (1, 0), (1, 4));
    t.advance(b"\x1b[?25l\x1b[2 q\x1b[?25h");
    assert!(t.selection().is_some(), "cursor changes are not content changes");
    t.advance(b"\x1b[2J");
    assert!(t.selection().is_none());
}

#[test]
fn selection_points_from_viewport() {
    let mut t = term(10, 2, "a\r\nb\r\nc");
    t.scroll_display(1);
    assert_eq!(t.viewport_point(0, 3), Point::new(-1, 3));
    assert_eq!(t.viewport_point(1, 99), Point::new(0, 9), "column clamped");
}

// ---- mouse reporting ----------------------------------------------------

fn mouse(
    t: &Terminal,
    b: MouseButton,
    a: MouseAction,
    col: usize,
    row: usize,
    mods: Mods,
) -> Option<Vec<u8>> {
    encode_mouse(b, a, col, row, mods, t.modes())
}

#[test]
fn mouse_off_reports_nothing() {
    let t = term(80, 24, "");
    assert_eq!(mouse(&t, MouseButton::Left, MouseAction::Press, 0, 0, Mods::default()), None);
}

#[test]
fn mouse_normal_tracking_default_encoding() {
    let t = term(80, 24, "\x1b[?1000h");
    let none = Mods::default();
    assert_eq!(
        mouse(&t, MouseButton::Left, MouseAction::Press, 4, 2, none),
        Some(b"\x1b[M %#".to_vec())
    );
    // Release is button 3 in the legacy encoding.
    assert_eq!(
        mouse(&t, MouseButton::Left, MouseAction::Release, 4, 2, none),
        Some(b"\x1b[M#%#".to_vec())
    );
    assert_eq!(
        mouse(&t, MouseButton::Left, MouseAction::Motion, 5, 2, none),
        None,
        "1000 has no motion"
    );
    assert_eq!(
        mouse(&t, MouseButton::WheelUp, MouseAction::Press, 0, 0, none),
        Some(b"\x1b[M`!!".to_vec())
    );
    assert_eq!(
        mouse(&t, MouseButton::Left, MouseAction::Press, 300, 0, none),
        None,
        "too far for 1 byte"
    );
}

#[test]
fn mouse_sgr_encoding_with_modifiers_and_motion() {
    let t = term(80, 24, "\x1b[?1002h\x1b[?1006h");
    let ctrl = Mods { ctrl: true, ..Mods::default() };
    assert_eq!(
        mouse(&t, MouseButton::Right, MouseAction::Press, 299, 9, ctrl),
        Some(b"\x1b[<18;300;10M".to_vec())
    );
    assert_eq!(
        mouse(&t, MouseButton::Right, MouseAction::Release, 299, 9, Mods::default()),
        Some(b"\x1b[<2;300;10m".to_vec())
    );
    assert_eq!(
        mouse(&t, MouseButton::Left, MouseAction::Motion, 1, 1, Mods::default()),
        Some(b"\x1b[<32;2;2M".to_vec())
    );
    assert_eq!(
        mouse(&t, MouseButton::None, MouseAction::Motion, 1, 1, Mods::default()),
        None,
        "1002: motion only with a button held"
    );
    let t = term(80, 24, "\x1b[?1003h\x1b[?1006h");
    assert_eq!(
        mouse(&t, MouseButton::None, MouseAction::Motion, 1, 1, Mods::default()),
        Some(b"\x1b[<35;2;2M".to_vec())
    );
    assert_eq!(
        mouse(&t, MouseButton::WheelDown, MouseAction::Release, 1, 1, Mods::default()),
        None
    );
}

#[test]
fn mouse_utf8_encoding_extends_range() {
    let t = term(80, 24, "\x1b[?1000h\x1b[?1005h");
    let got = mouse(&t, MouseButton::Left, MouseAction::Press, 299, 0, Mods::default()).unwrap();
    let mut want = b"\x1b[M ".to_vec();
    want.extend_from_slice('\u{14c}'.to_string().as_bytes()); // 32 + 300
    want.push(b'!');
    assert_eq!(got, want);
}

#[test]
fn alternate_scroll_mode() {
    let mut t = term(10, 2, "");
    assert!(t.modes().alternate_scroll, "on by default");
    t.advance(b"\x1b[?1007l");
    assert!(!t.modes().alternate_scroll);
}
