use seraphim_terminal::input::{encode_key, encode_paste, Key, Mods};
use seraphim_terminal::{Color, Flags, Terminal, TerminalEvent};

fn term(cols: usize, rows: usize, input: &str) -> Terminal {
    let mut t = Terminal::new(cols, rows);
    t.advance(input.as_bytes());
    t
}

#[test]
fn plain_text_and_newlines() {
    let t = term(10, 3, "ab\r\ncd");
    assert_eq!(t.screen_text(), "ab\ncd");
    assert_eq!((t.cursor().row, t.cursor().col), (1, 2));
}

#[test]
fn autowrap_uses_pending_wrap() {
    let mut t = term(5, 3, "abcde");
    // Cursor sits on the last column; no wrap until the next printable.
    assert_eq!((t.cursor().row, t.cursor().col), (0, 4));
    t.advance(b"f");
    assert_eq!(t.screen_text(), "abcde\nf");
    assert!(t.screen().row(0).wrapped);
    // CR clears the pending wrap.
    let t = term(5, 3, "abcde\rX");
    assert_eq!(t.screen_text(), "Xbcde");
}

#[test]
fn autowrap_disabled_overwrites_last_column() {
    let t = term(5, 2, "\x1b[?7labcdefg");
    assert_eq!(t.screen_text(), "abcdg");
}

#[test]
fn scrolling_fills_scrollback() {
    let t = term(10, 2, "1\r\n2\r\n3\r\n4");
    assert_eq!(t.screen_text(), "3\n4");
    let sb: Vec<String> = t.scrollback().iter().map(|r| r.text()).collect();
    assert_eq!(sb, vec!["1", "2"]);
}

#[test]
fn scrollback_is_bounded() {
    let mut t = Terminal::with_scrollback(5, 1, 3);
    for i in 0..10 {
        t.advance(format!("{i}\r\n").as_bytes());
    }
    let sb: Vec<String> = t.scrollback().iter().map(|r| r.text()).collect();
    assert_eq!(sb, vec!["7", "8", "9"]);
}

#[test]
fn cursor_movement() {
    let t = term(10, 5, "\x1b[3;4HX\x1b[AY\x1b[2DZ\x1b[BW\x1b[GQ");
    // X at (2,3); Y at (1,4); Z two left of the cursor at (1,3); W below at
    // (2,4); Q at column 0 of row 2.
    assert_eq!(t.screen_text(), "\n   ZY\nQ  XW");
}

#[test]
fn cursor_movement_is_clamped() {
    let t = term(5, 3, "\x1b[99;99HX\x1b[99AY");
    assert_eq!(t.screen().row(2).text(), "    X");
    assert_eq!(t.screen().row(0).text(), "    Y");
}

#[test]
fn erase_in_line_and_display() {
    let t = term(6, 3, "aaaaaa\r\nbbbbbb\r\ncccccc\x1b[2;3H\x1b[K");
    assert_eq!(t.screen_text(), "aaaaaa\nbb\ncccccc");
    let t = term(6, 3, "aaaaaa\r\nbbbbbb\r\ncccccc\x1b[2;3H\x1b[1K");
    assert_eq!(t.screen_text(), "aaaaaa\n   bbb\ncccccc");
    let t = term(6, 3, "aaaaaa\r\nbbbbbb\r\ncccccc\x1b[2;3H\x1b[J");
    assert_eq!(t.screen_text(), "aaaaaa\nbb");
    let t = term(6, 3, "aaaaaa\r\nbbbbbb\r\ncccccc\x1b[2;3H\x1b[1J");
    assert_eq!(t.screen_text(), "\n   bbb\ncccccc");
    let t = term(6, 3, "aaaaaa\r\nbbbbbb\x1b[2J");
    assert_eq!(t.screen_text(), "");
}

#[test]
fn erase_uses_background_color() {
    let t = term(4, 1, "\x1b[44m\x1b[2K");
    assert_eq!(t.screen().cell(0, 3).attrs.bg, Color::Indexed(4));
}

#[test]
fn insert_and_delete_chars() {
    let t = term(6, 1, "abcdef\x1b[1;2H\x1b[2@");
    assert_eq!(t.screen_text(), "a  bcd");
    let t = term(6, 1, "abcdef\x1b[1;2H\x1b[2P");
    assert_eq!(t.screen_text(), "adef");
    let t = term(6, 1, "abcdef\x1b[1;2H\x1b[3X");
    assert_eq!(t.screen_text(), "a   ef");
}

#[test]
fn insert_mode() {
    let t = term(6, 1, "abc\x1b[1;1H\x1b[4hXY");
    assert_eq!(t.screen_text(), "XYabc");
}

#[test]
fn scroll_region_and_line_ops() {
    // Region rows 2..=3; linefeed at bottom of region scrolls only the region.
    let mut t = term(5, 4, "1\r\n2\r\n3\r\n4\x1b[2;3r\x1b[3;1H\n");
    assert_eq!(t.screen_text(), "1\n3\n\n4");
    assert!(t.scrollback().is_empty(), "region scroll must not hit scrollback");
    t.advance(b"\x1b[r\x1b[2;1H\x1b[L");
    assert_eq!(t.screen_text(), "1\n\n3"); // last row now blank (trimmed)
    t.advance(b"\x1b[1;1H\x1b[2M");
    assert_eq!(t.screen_text(), "3");
}

#[test]
fn reverse_index_scrolls_down_at_top() {
    let t = term(5, 3, "a\r\nb\r\nc\x1b[H\x1bM");
    assert_eq!(t.screen_text(), "\na\nb");
}

#[test]
fn origin_mode() {
    let t = term(5, 5, "\x1b[2;4r\x1b[?6h\x1b[1;1HX\x1b[9;1HY");
    assert_eq!(t.screen_text(), "\nX\n\nY");
}

#[test]
fn alternate_screen_1049() {
    let mut t = term(10, 3, "shell$ ");
    t.advance(b"\x1b[?1049h\x1b[Hvim");
    assert!(t.is_alternate_screen());
    assert_eq!(t.screen_text(), "vim");
    t.advance(b"\x1b[?1049l");
    assert!(!t.is_alternate_screen());
    assert_eq!(t.screen_text(), "shell$");
    assert_eq!(t.cursor().col, 7, "cursor restored");
    let ev = t.take_events();
    assert!(ev.contains(&TerminalEvent::AlternateScreen(true)));
    assert!(ev.contains(&TerminalEvent::AlternateScreen(false)));
}

#[test]
fn alternate_screen_does_not_feed_scrollback() {
    let t = term(5, 2, "\x1b[?1049h1\r\n2\r\n3\r\n4");
    assert!(t.scrollback().is_empty());
}

#[test]
fn sgr_basic_and_reset() {
    let t = term(10, 1, "\x1b[1;3;4;31;42mA\x1b[0mB");
    let a = t.screen().cell(0, 0).attrs;
    assert!(a.flags.contains(Flags::BOLD | Flags::ITALIC | Flags::UNDERLINE));
    assert_eq!(a.fg, Color::Indexed(1));
    assert_eq!(a.bg, Color::Indexed(2));
    assert_eq!(t.screen().cell(0, 1).attrs, Default::default());
}

#[test]
fn sgr_extended_colors() {
    let t = term(10, 1, "\x1b[38;5;196mA\x1b[38;2;1;2;3mB\x1b[48:2::4:5:6mC\x1b[38:5:20;95mD");
    let c = |i| t.screen().cell(0, i).attrs;
    assert_eq!(c(0).fg, Color::Indexed(196));
    assert_eq!(c(1).fg, Color::Rgb(1, 2, 3));
    assert_eq!(c(2).bg, Color::Rgb(4, 5, 6));
    assert_eq!(c(3).fg, Color::Indexed(13), "95 after colon form applies");
}

#[test]
fn sgr_semicolon_rgb_followed_by_more() {
    let t = term(10, 1, "\x1b[38;2;10;20;30;1mA");
    let a = t.screen().cell(0, 0).attrs;
    assert_eq!(a.fg, Color::Rgb(10, 20, 30));
    assert!(a.flags.contains(Flags::BOLD));
}

#[test]
fn curly_underline() {
    let t = term(4, 1, "\x1b[4:3mA\x1b[4:0mB");
    assert!(t.screen().cell(0, 0).attrs.flags.contains(Flags::CURLY_UNDERLINE));
    assert!(!t.screen().cell(0, 1).attrs.flags.intersects(Flags::ALL_UNDERLINES));
}

#[test]
fn wide_characters() {
    let t = term(6, 2, "a日b");
    assert_eq!(t.screen_text(), "a日b");
    assert!(t.screen().cell(0, 1).is_wide());
    assert!(t.screen().cell(0, 2).is_wide_spacer());
    assert_eq!(t.cursor().col, 4);
}

#[test]
fn wide_character_wraps_when_it_does_not_fit() {
    let t = term(4, 2, "abc日");
    assert_eq!(t.screen_text(), "abc\n日");
}

#[test]
fn overwriting_half_a_wide_char_clears_both_halves() {
    let t = term(6, 1, "日本\x1b[1;2HX");
    assert_eq!(t.screen_text(), " X本");
}

#[test]
fn combining_characters_attach_to_previous_cell() {
    let t = term(6, 1, "e\u{301}x");
    assert_eq!(t.screen_text(), "e\u{301}x");
    assert_eq!(t.cursor().col, 2);
}

#[test]
fn tabs() {
    let t = term(20, 1, "a\tb\tc");
    assert_eq!(t.screen_text(), "a       b       c");
    let t = term(20, 1, "\x1b[3g\x1b[5G\x1bH\x1b[1G\tX");
    assert_eq!(t.screen_text(), "    X");
}

#[test]
fn dec_special_graphics() {
    let t = term(5, 1, "\x1b(0lqk\x1b(Bq");
    assert_eq!(t.screen_text(), "┌─┐q");
}

#[test]
fn device_status_and_attributes() {
    let mut t = term(10, 5, "\x1b[3;4H\x1b[6n\x1b[5n\x1b[c");
    assert_eq!(t.take_responses(), b"\x1b[3;4R\x1b[0n\x1b[?62;4;22c");
    assert!(t.take_responses().is_empty());
}

#[test]
fn title_cwd_and_semantic_prompts() {
    let mut t = term(
        10,
        2,
        "\x1b]2;hello;world\x07\x1b]7;file://host/home/me/My%20Dir\x1b\\\
         \x1b]133;A\x07$ \x1b]133;B\x07ls\x1b]133;C;cmdline_url=pacman%20-S%20firefox\x07\x1b]133;D;2\x07",
    );
    assert_eq!(t.title(), "hello;world");
    assert_eq!(t.cwd(), Some("/home/me/My Dir"));
    assert_eq!(
        t.take_events(),
        vec![
            TerminalEvent::TitleChanged("hello;world".into()),
            TerminalEvent::CwdChanged("/home/me/My Dir".into()),
            TerminalEvent::PromptStarted,
            TerminalEvent::CommandInputStarted,
            TerminalEvent::CommandExecuted { command: Some("pacman -S firefox".into()) },
            TerminalEvent::CommandFinished { exit_code: Some(2) },
        ]
    );
}

#[test]
fn hyperlinks() {
    let t = term(20, 1, "\x1b]8;;https://example.com/a;b\x1b\\link\x1b]8;;\x1b\\ no");
    let id = t.screen().cell(0, 0).attrs.link;
    assert_ne!(id, 0);
    assert_eq!(t.hyperlink(id).unwrap().uri, "https://example.com/a;b");
    assert_eq!(t.screen().cell(0, 5).attrs.link, 0);
}

#[test]
fn clipboard_is_reported_not_applied() {
    let mut t = term(5, 1, "\x1b]52;c;aGk=\x07\x1b]52;c;?\x07");
    assert_eq!(
        t.take_events(),
        vec![
            TerminalEvent::ClipboardStore { selection: "c".into(), data: "aGk=".into() },
            TerminalEvent::ClipboardLoad { selection: "c".into() },
        ]
    );
}

#[test]
fn modes_affect_input_encoding() {
    let mut t = Terminal::new(5, 1);
    let none = Mods::default();
    assert_eq!(encode_key(Key::Up, none, t.modes()), b"\x1b[A");
    t.advance(b"\x1b[?1h\x1b[?2004h");
    assert_eq!(encode_key(Key::Up, none, t.modes()), b"\x1bOA");
    let ctrl = Mods { ctrl: true, ..none };
    assert_eq!(encode_key(Key::Up, ctrl, t.modes()), b"\x1b[1;5A");
    assert_eq!(encode_key(Key::Char('c'), ctrl, t.modes()), b"\x03");
    assert_eq!(encode_key(Key::F(5), none, t.modes()), b"\x1b[15~");
    assert_eq!(
        encode_paste("rm -rf /\x1b[201~\nboom", t.modes()),
        b"\x1b[200~rm -rf /\rboom\x1b[201~"
    );
}

#[test]
fn resize_keeps_cursor_on_screen() {
    let mut t = term(10, 4, "1\r\n2\r\n3\r\n4");
    t.resize(5, 2);
    assert_eq!(t.screen_text(), "3\n4");
    assert_eq!(t.cursor().row, 1);
    let sb: Vec<String> = t.scrollback().iter().map(|r| r.text()).collect();
    assert_eq!(sb, vec!["1", "2"]);
    t.resize(8, 3);
    t.advance(b"\r\nabcdefgh");
    assert_eq!(t.screen().row(2).text(), "abcdefgh");
}

#[test]
fn damage_tracking() {
    let mut t = Terminal::new(5, 3);
    t.take_damage();
    t.advance(b"\x1b[2;1Hx");
    assert_eq!(t.take_damage(), vec![1]);
    assert!(t.take_damage().is_empty());
}

#[test]
fn full_reset() {
    let mut t = term(5, 2, "\x1b[31mabc\x1b[?7l\x1b[2;2r");
    t.advance(b"\x1bcX");
    assert_eq!(t.screen_text(), "X");
    assert!(t.modes().autowrap);
    assert_eq!(t.screen().cell(0, 0).attrs.fg, Color::Default);
}

#[test]
fn repeat_last_char() {
    let t = term(10, 1, "a\x1b[4b");
    assert_eq!(t.screen_text(), "aaaaa");
}

#[test]
fn garbage_does_not_panic() {
    // Deterministic pseudo-random byte soup, including huge parameters.
    let mut t = Terminal::new(7, 3);
    let mut x: u32 = 0x1234_5678;
    let mut buf = Vec::new();
    for _ in 0..200_000 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        buf.push(match x % 7 {
            0 => 0x1b,
            1 => b'[',
            2 => b';',
            3 => b'0' + (x >> 8) as u8 % 10,
            _ => (x >> 16) as u8,
        });
    }
    buf.extend_from_slice(b"\x1b[99999;99999H\x1b[65535@\x1b[65535L\x1b[65535b\x1b[0;65535r");
    for chunk in buf.chunks(97) {
        t.advance(chunk);
        t.resize(1 + (chunk[0] as usize % 9), 1 + (chunk[1 % chunk.len()] as usize % 5));
    }
}

#[test]
fn combining_marks_follow_their_cell() {
    // Overwrite clears the mark.
    let t = term(6, 1, "e\u{301}x\x1b[1GE");
    assert_eq!(t.screen_text(), "Ex");
    // Insert/delete characters move the mark with its base character.
    let t = term(8, 1, "ab\u{308}c\x1b[1G\x1b[2@");
    assert_eq!(t.screen_text(), "  ab\u{308}c");
    let t = term(8, 1, "xyab\u{308}c\x1b[1G\x1b[2P");
    assert_eq!(t.screen_text(), "ab\u{308}c");
    // Erase removes it.
    let t = term(8, 1, "ab\u{308}c\x1b[2G\x1b[X");
    assert_eq!(t.screen_text(), "a c");
    // Scrolling keeps it attached to its line.
    let t = term(4, 2, "a\u{301}\r\nb\r\nc");
    assert_eq!(t.scrollback()[0].text(), "a\u{301}");
    assert_eq!(t.screen_text(), "b\nc");
}

// Found by xterm-refereed differential testing (crates/seraphim-conformance).

#[test]
fn deleting_a_wide_char_keeps_the_next_one() {
    let t = term(8, 1, "日本\x1b[1;1H\x1b[2P");
    assert_eq!(t.screen_text(), "本");
    // Inserting before a wide char moves it intact.
    let t = term(8, 1, "日本\x1b[1;1H\x1b[2@");
    assert_eq!(t.screen_text(), "  日本");
    // A deletion boundary through the middle of a wide char breaks it.
    let t = term(8, 1, "日本\x1b[1;1H\x1b[3P");
    assert_eq!(t.screen_text(), "");
}

#[test]
fn rep_only_repeats_an_immediately_preceding_graphic_char() {
    let t = term(14, 1, "本\t\x1b[3b");
    assert_eq!(t.screen_text(), "本");
    // Verified against xterm: a second REP repeats nothing, and SGR or CR
    // in between also end the repeatable character.
    let t = term(14, 1, "ab\x1b[2b\x1b[2b");
    assert_eq!(t.screen_text(), "abbb");
    let t = term(14, 1, "a\x1b[31m\x1b[2b");
    assert_eq!(t.screen_text(), "a");
}

#[test]
fn wide_char_wrapping_at_last_column_keeps_that_cell() {
    let t = term(14, 2, "\t\tx\t本");
    assert_eq!(t.screen_text(), "             x\n本");
}

#[test]
fn scroll_down_default_count() {
    let t = term(6, 3, "a\r\nb\r\nc\x1b[2;1H\x1b[T");
    assert_eq!(t.screen_text(), "\na\nb");
    let t = term(6, 3, "a\r\nb\r\nc\x1b[2;1H\x1b[0S");
    assert_eq!(t.screen_text(), "b\nc", "SU 0 still means 1");
}

// Each expectation below was verified against xterm (see
// crates/seraphim-conformance).

#[test]
fn wide_char_without_autowrap_follows_xterm() {
    let t = |s: &str| term(6, 1, s).screen_text();
    assert_eq!(t("\x1b[?7lab日本"), "ab日本", "fits exactly");
    assert_eq!(t("\x1b[?7labc日本"), "abc本", "replaces the wide char just printed");
    assert_eq!(t("\x1b[?7labcd日本"), "abcd本");
    assert_eq!(t("\x1b[?7labcde日"), "abcde", "no room: dropped");
    assert_eq!(t("\x1b[?7labcdef日"), "abcdef");
    assert_eq!(t("\x1b[?7labcd日\x1b[1;6H本"), "abcd日", "cursor moved in between: dropped");
    assert_eq!(t("\x1b[?7labcd日x"), "abcd x", "narrow char splits the wide one");
}

#[test]
fn alternate_screen_clearing_follows_xterm() {
    let t = |s: &str| term(6, 1, s).screen_text();
    assert_eq!(t("main\x1b[?1049hq\x1b[?1049h"), "", "1049h always clears");
    assert_eq!(t("main\x1b[?1047hq\x1b[?1047h"), "    q", "1047h does not clear");
    assert_eq!(t("main\x1b[?47hq\x1b[?47h"), "    q");
    assert_eq!(t("main\x1b[?1047hq\x1b[?1047l\x1b[?47h"), "", "1047l clears on the way out");
    assert_eq!(t("main\x1b[?47hq\x1b[?47l\x1b[?47h"), "    q", "47 never clears");
}

#[test]
fn tab_at_margin_keeps_pending_wrap() {
    assert_eq!(term(6, 2, "abcdef\tg").screen_text(), "abcdef\ng");
    assert_eq!(term(6, 2, "abcde\tg").screen_text(), "abcdeg");
}

#[test]
fn resize_forgets_the_last_wide_char_position() {
    // Found by the fuzzer: a remembered wide-char column survived a shrink.
    let mut t = term(10, 2, "\x1b[?7labcdefgh日");
    t.resize(3, 2);
    t.advance("\x1b[?7l本".as_bytes());
    t.check_invariants().unwrap();
}

#[test]
fn vertical_movement_and_margins_follow_xterm() {
    // Region rows 3..=5 (1-based) in an 8-row screen.
    let at = |seq: &str| {
        let t = term(8, 8, &format!("\x1b[3;5r{seq}"));
        (t.cursor().row + 1, t.cursor().col + 1)
    };
    assert_eq!(at("\x1b[2;1H\x1b[9B"), (5, 1), "CUD from above stops at the bottom margin");
    assert_eq!(at("\x1b[2;1H\x1b[9e"), (8, 1), "VPR ignores margins");
    assert_eq!(at("\x1b[8;1H\x1b[9A"), (3, 1), "CUU from below stops at the top margin");
    assert_eq!(at("\x1b[8;1H\x1b[9F"), (3, 1));
    assert_eq!(at("\x1b[1;1H\x1b[9E"), (5, 1));
}
