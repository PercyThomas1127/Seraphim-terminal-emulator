//! Grapheme clusters (mode 2027) and the new SGR attributes.

use seraphim_terminal::{Flags, Point, SelectionKind, Terminal};

fn term(cols: usize, input: &str) -> Terminal {
    let mut t = Terminal::new(cols, 3);
    t.advance(input.as_bytes());
    t
}

/// (text of cell 0, is wide, cursor col)
fn first_cell(t: &Terminal) -> (String, bool, usize) {
    let mut s = String::new();
    t.screen().row(0).push_cell_text(0, &mut s);
    (s, t.screen().cell(0, 0).is_wide(), t.cursor().col)
}

#[test]
fn emoji_sequences_become_one_wide_cell() {
    for seq in [
        "🇯🇵",
        "🇺🇦",
        "👩\u{200d}💻",
        "👍🏽",
        "1\u{fe0f}\u{20e3}",
        "❤\u{fe0f}",
        "☀\u{fe0f}",
        "👨\u{200d}👩\u{200d}👧\u{200d}👦",
        "🏴\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}",
    ] {
        let t = term(10, seq);
        assert_eq!(first_cell(&t), (seq.to_string(), true, 2), "{seq:?}");
        assert!(t.screen().cell(0, 1).is_wide_spacer(), "{seq:?}");
        t.check_invariants().unwrap();
    }
}

#[test]
fn text_presentation_stays_narrow_and_accents_join() {
    assert_eq!(first_cell(&term(10, "❤")), ("❤".into(), false, 1));
    assert_eq!(first_cell(&term(10, "e\u{301}")), ("e\u{301}".into(), false, 1));
    assert_eq!(term(10, "e\u{301}x").screen_text(), "e\u{301}x");
}

#[test]
fn clusters_are_separate_from_their_neighbours() {
    let t = term(20, "a🇯🇵🇺🇦b");
    assert_eq!(t.screen_text(), "a🇯🇵🇺🇦b");
    assert_eq!(t.cursor().col, 6, "a + 2 + 2 + b");
    // Three regional indicators: one flag plus a lone indicator.
    let t = term(20, "🇯🇵🇺");
    assert_eq!(t.cursor().col, 3);
    // A skin tone after a letter is not joined to it.
    let t = term(20, "a🏽");
    assert_eq!(t.cursor().col, 3);
}

#[test]
fn cursor_movement_breaks_a_cluster() {
    // VS16 after a cursor move does not reach back to the old character.
    let t = term(10, "❤\x1b[C\u{fe0f}");
    assert!(!t.screen().cell(0, 0).is_wide());
}

#[test]
fn widening_at_the_last_column_keeps_the_cell_narrow() {
    let t = term(4, "abc❤\u{fe0f}");
    assert!(!t.screen().cell(0, 3).is_wide(), "no room for a second column");
    t.check_invariants().unwrap();
}

#[test]
fn mode_2027_off_restores_per_codepoint_widths() {
    let mut t = Terminal::new(20, 2);
    t.advance(b"\x1b[?2027$p");
    assert_eq!(t.take_responses(), b"\x1b[?2027;1$y", "on by default, reported via DECRQM");
    t.advance(b"\x1b[?2027l");
    t.advance("🇯🇵👍🏽".as_bytes());
    assert_eq!(t.cursor().col, 6, "1 + 1 + 2 + 2 per codepoint");
    let mut t = Terminal::new(20, 2);
    t.set_grapheme_clustering(false);
    t.advance("🇯🇵\x1bc".as_bytes());
    assert!(!t.modes().grapheme_clusters, "RIS restores the configured default");
}

#[test]
fn clusters_survive_selection_and_reflow() {
    let mut t = term(6, "ab👩\u{200d}💻cd");
    t.start_selection(SelectionKind::Line, Point::new(0, 0));
    assert_eq!(t.selection_text().unwrap(), "ab👩\u{200d}💻cd");
    t.resize(3, 3);
    t.resize(6, 3);
    assert_eq!(t.screen_text(), "ab👩\u{200d}💻cd");
    t.check_invariants().unwrap();
}

#[test]
fn overline_and_underline_styles() {
    let t = term(20, "\x1b[53mo\x1b[55mn\x1b[4:4md\x1b[4:5me\x1b[4:3mc\x1b[4:0mx");
    let f = |i| t.screen().cell(0, i).attrs.flags;
    assert!(f(0).contains(Flags::OVERLINE));
    assert!(!f(1).contains(Flags::OVERLINE));
    assert!(f(2).contains(Flags::DOTTED_UNDERLINE));
    assert!(f(3).contains(Flags::DASHED_UNDERLINE));
    assert!(f(4).contains(Flags::CURLY_UNDERLINE));
    assert!(!f(5).intersects(Flags::ALL_UNDERLINES));
    let mut t = term(5, "\x1b[53;4:4m");
    t.advance(b"\x1bP$qm\x1b\\");
    assert_eq!(t.take_responses(), b"\x1bP1$r0;4:4;53m\x1b\\");
}
