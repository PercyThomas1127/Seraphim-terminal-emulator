use seraphim_terminal::palette::ColorScheme;
use seraphim_terminal::Terminal;

fn reply(t: &mut Terminal, input: &str) -> String {
    t.advance(input.as_bytes());
    String::from_utf8(t.take_responses()).unwrap()
}

fn scheme() -> ColorScheme {
    ColorScheme {
        foreground: [0xe6, 0xe1, 0xf0],
        background: [0x14, 0x12, 0x1f],
        ..ColorScheme::default()
    }
}

#[test]
fn osc_10_11_12_query_set_reset() {
    let mut t = Terminal::new(10, 2);
    t.set_color_scheme(scheme());
    // BEL-terminated request gets a BEL-terminated reply, ST gets ST.
    assert_eq!(reply(&mut t, "\x1b]11;?\x07"), "\x1b]11;rgb:1414/1212/1f1f\x07");
    assert_eq!(reply(&mut t, "\x1b]10;?\x1b\\"), "\x1b]10;rgb:e6e6/e1e1/f0f0\x1b\\");
    // One OSC 10 can address 10 and 11 in turn.
    assert_eq!(
        reply(&mut t, "\x1b]10;?;?\x07"),
        "\x1b]10;rgb:e6e6/e1e1/f0f0\x07\x1b]11;rgb:1414/1212/1f1f\x07"
    );
    t.advance(b"\x1b]11;#102030\x07");
    assert_eq!(t.color_overrides().background, Some([0x10, 0x20, 0x30]));
    assert_eq!(reply(&mut t, "\x1b]11;?\x07"), "\x1b]11;rgb:1010/2020/3030\x07");
    t.advance(b"\x1b]111\x07");
    assert_eq!(t.color_overrides().background, None);
    assert_eq!(reply(&mut t, "\x1b]11;?\x07"), "\x1b]11;rgb:1414/1212/1f1f\x07");
}

#[test]
fn osc_4_palette() {
    let mut t = Terminal::new(10, 2);
    t.set_color_scheme(scheme());
    assert_eq!(reply(&mut t, "\x1b]4;1;?\x07"), "\x1b]4;1;rgb:cdcd/0000/0000\x07");
    assert_eq!(reply(&mut t, "\x1b]4;196;?\x07"), "\x1b]4;196;rgb:ffff/0000/0000\x07");
    t.advance(b"\x1b]4;1;rgb:12/34/56;200;#abcdef\x07");
    assert_eq!(t.color_overrides().indexed[1], Some([0x12, 0x34, 0x56]));
    assert_eq!(t.color_overrides().indexed[200], Some([0xab, 0xcd, 0xef]));
    t.advance(b"\x1b]104;1\x07");
    assert_eq!(t.color_overrides().indexed[1], None);
    assert_eq!(t.color_overrides().indexed[200], Some([0xab, 0xcd, 0xef]));
    t.advance(b"\x1b]104\x07");
    assert!(t.color_overrides().is_empty());
    // Junk is ignored, not applied.
    t.advance(b"\x1b]4;999;#fff\x07\x1b]4;3;notacolor\x07");
    assert!(t.color_overrides().is_empty());
}

#[test]
fn full_reset_keeps_scheme_clears_overrides() {
    let mut t = Terminal::new(10, 2);
    t.set_color_scheme(scheme());
    t.advance(b"\x1b]10;#ffffff\x07\x1bc");
    assert!(t.color_overrides().is_empty());
    assert_eq!(t.color_scheme(), &scheme());
}

#[test]
fn decrqm() {
    let mut t = Terminal::new(10, 2);
    assert_eq!(reply(&mut t, "\x1b[?7$p"), "\x1b[?7;1$y", "autowrap on");
    assert_eq!(reply(&mut t, "\x1b[?2004$p"), "\x1b[?2004;2$y");
    t.advance(b"\x1b[?2004h\x1b[?1049h");
    assert_eq!(reply(&mut t, "\x1b[?2004$p"), "\x1b[?2004;1$y");
    assert_eq!(reply(&mut t, "\x1b[?1049$p"), "\x1b[?1049;1$y");
    assert_eq!(reply(&mut t, "\x1b[?2026$p"), "\x1b[?2026;2$y", "sync output recognized");
    assert_eq!(reply(&mut t, "\x1b[?31337$p"), "\x1b[?31337;0$y", "unknown");
    assert_eq!(reply(&mut t, "\x1b[4$p"), "\x1b[4;2$y", "ANSI insert mode");
}

#[test]
fn xtversion_and_size_report() {
    let mut t = Terminal::new(33, 7);
    assert_eq!(
        reply(&mut t, "\x1b[>q"),
        format!("\x1bP>|Seraphim {}\x1b\\", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(reply(&mut t, "\x1b[18t"), "\x1b[8;7;33t");
    assert_eq!(reply(&mut t, "\x1b[14t"), "", "pixel size not reported");
}

#[test]
fn decrqss() {
    let mut t = Terminal::new(10, 5);
    t.advance(b"\x1b[1;4;38;2;1;2;3;44m\x1b[5 q\x1b[2;4r");
    assert_eq!(reply(&mut t, "\x1bP$qm\x1b\\"), "\x1bP1$r0;1;4;38;2;1;2;3;44m\x1b\\");
    assert_eq!(reply(&mut t, "\x1bP$q q\x1b\\"), "\x1bP1$r5 q\x1b\\");
    assert_eq!(reply(&mut t, "\x1bP$qr\x1b\\"), "\x1bP1$r2;4r\x1b\\");
    assert_eq!(reply(&mut t, "\x1bP$qxyz\x1b\\"), "\x1bP0$r\x1b\\");
    // A plain (non-DECRQSS) DCS produces nothing.
    assert_eq!(reply(&mut t, "\x1bPqsixel-ish\x1b\\"), "");
}

#[test]
fn oversized_osc_is_dropped_and_parsing_continues() {
    let mut t = Terminal::new(20, 2);
    t.set_max_osc_len(1000);
    let mut big = b"\x1b]2;".to_vec();
    big.extend(std::iter::repeat_n(b'x', 50_000));
    big.extend_from_slice(b"\x07after\x1b]2;ok\x07");
    for chunk in big.chunks(4096) {
        t.advance(chunk);
    }
    assert_eq!(t.dropped_osc_count(), 1);
    assert_eq!(t.title(), "ok", "the next OSC still works");
    assert_eq!(t.screen_text(), "after");
}
