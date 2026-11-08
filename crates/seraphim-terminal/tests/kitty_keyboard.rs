//! Kitty keyboard protocol: flag stack and key encoding.
//! <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>

use seraphim_terminal::input::{encode_key_input, Key, KeyInput, KeyKind, ModifierKey, Mods};
use seraphim_terminal::Terminal;

fn query(t: &mut Terminal) -> String {
    t.advance(b"\x1b[?u");
    String::from_utf8(t.take_responses()).unwrap()
}

#[test]
fn flag_stack_push_pop_set_query() {
    let mut t = Terminal::new(10, 2);
    assert_eq!(query(&mut t), "\x1b[?0u", "legacy by default");
    t.advance(b"\x1b[>1u");
    assert_eq!(query(&mut t), "\x1b[?1u");
    t.advance(b"\x1b[>11u");
    assert_eq!(query(&mut t), "\x1b[?11u");
    t.advance(b"\x1b[<u");
    assert_eq!(query(&mut t), "\x1b[?1u", "pop restores the previous level");
    t.advance(b"\x1b[=4;2u");
    assert_eq!(query(&mut t), "\x1b[?5u", "mode 2 ORs");
    t.advance(b"\x1b[=1;3u");
    assert_eq!(query(&mut t), "\x1b[?4u", "mode 3 clears");
    t.advance(b"\x1b[=2u");
    assert_eq!(query(&mut t), "\x1b[?2u", "mode 1 (default) sets");
    t.advance(b"\x1b[<5u");
    assert_eq!(query(&mut t), "\x1b[?0u", "popping past the bottom empties the stack");
}

#[test]
fn main_and_alternate_screens_have_separate_stacks() {
    let mut t = Terminal::new(10, 2);
    t.advance(b"\x1b[>1u\x1b[?1049h");
    assert_eq!(query(&mut t), "\x1b[?0u");
    t.advance(b"\x1b[>8u");
    assert_eq!(t.modes().kitty_keyboard, 8);
    t.advance(b"\x1b[?1049l");
    assert_eq!(t.modes().kitty_keyboard, 1);
    t.advance(b"\x1bc");
    assert_eq!(t.modes().kitty_keyboard, 0, "RIS resets");
}

#[test]
fn stack_is_bounded() {
    let mut t = Terminal::new(10, 2);
    for _ in 0..1000 {
        t.advance(b"\x1b[>1u");
    }
    t.advance(b"\x1b[<16u");
    assert_eq!(t.modes().kitty_keyboard, 0);
}

fn enc(
    flags: u8,
    key: Key,
    mods: Mods,
    kind: KeyKind,
    shifted: Option<char>,
    text: Option<&str>,
) -> String {
    let mut t = Terminal::new(10, 2);
    t.advance(format!("\x1b[>{flags}u").as_bytes());
    let ev = KeyInput { key, mods, kind, shifted, text };
    String::from_utf8(encode_key_input(&ev, t.modes())).unwrap()
}

const NONE: Mods = Mods { shift: false, alt: false, ctrl: false, super_key: false };
const SHIFT: Mods = Mods { shift: true, ..NONE };
const CTRL: Mods = Mods { ctrl: true, ..NONE };
const ALT: Mods = Mods { alt: true, ..NONE };
const CTRL_SHIFT: Mods = Mods { ctrl: true, shift: true, ..NONE };
const SUPER: Mods = Mods { super_key: true, ..NONE };
use KeyKind::{Press, Release, Repeat};

#[test]
fn disambiguate_escape_codes() {
    let e = |k, m, s, t| enc(1, k, m, Press, s, t);
    assert_eq!(e(Key::Escape, NONE, None, None), "\x1b[27u");
    assert_eq!(e(Key::Char('a'), NONE, Some('a'), Some("a")), "a", "plain text stays text");
    assert_eq!(e(Key::Char('a'), SHIFT, Some('A'), Some("A")), "A");
    assert_eq!(e(Key::Char('a'), CTRL, Some('a'), None), "\x1b[97;5u");
    assert_eq!(e(Key::Char('a'), ALT, Some('a'), Some("a")), "\x1b[97;3u");
    assert_eq!(e(Key::Char('a'), CTRL_SHIFT, Some('A'), None), "\x1b[97;6u");
    assert_eq!(e(Key::Char('i'), CTRL, Some('i'), None), "\x1b[105;5u", "Ctrl+I is not Tab");
    assert_eq!(e(Key::Char('a'), SUPER, Some('a'), None), "\x1b[97;9u");
    assert_eq!(e(Key::Enter, NONE, None, None), "\r", "Enter stays legacy");
    assert_eq!(e(Key::Tab, NONE, None, None), "\t");
    assert_eq!(e(Key::Backspace, NONE, None, None), "\x7f");
    assert_eq!(e(Key::Enter, CTRL, None, None), "\x1b[13;5u");
    assert_eq!(e(Key::Tab, SHIFT, None, None), "\x1b[9;2u");
    assert_eq!(e(Key::Up, NONE, None, None), "\x1b[A");
    assert_eq!(e(Key::Up, CTRL, None, None), "\x1b[1;5A");
    assert_eq!(e(Key::F(1), NONE, None, None), "\x1b[P");
    assert_eq!(e(Key::F(3), NONE, None, None), "\x1b[13~");
    assert_eq!(e(Key::F(5), SHIFT, None, None), "\x1b[15;2~");
    assert_eq!(e(Key::PageUp, NONE, None, None), "\x1b[5~");
    assert_eq!(e(Key::Char('a'), NONE, Some('a'), Some("a")), "a");
}

#[test]
fn event_types() {
    let e = |k, m, kind, s, t| enc(1 | 2, k, m, kind, s, t);
    assert_eq!(e(Key::Char('a'), NONE, Release, Some('a'), None), "\x1b[97;1:3u");
    assert_eq!(e(Key::Up, NONE, Repeat, None, None), "\x1b[1;1:2A");
    assert_eq!(e(Key::Up, CTRL, Release, None, None), "\x1b[1;5:3A");
    assert_eq!(e(Key::Enter, NONE, Release, None, None), "", "no Enter release unless all keys");
    // Without the event-type flag, releases are not reported at all.
    assert_eq!(enc(1, Key::Char('a'), NONE, Release, Some('a'), None), "");
    assert_eq!(enc(1, Key::Up, NONE, Repeat, None, None), "\x1b[A", "repeat looks like a press");
}

#[test]
fn report_all_keys_alternates_and_text() {
    assert_eq!(enc(8, Key::Char('a'), NONE, Press, Some('a'), Some("a")), "\x1b[97u");
    assert_eq!(enc(8, Key::Enter, NONE, Press, None, None), "\x1b[13u");
    assert_eq!(enc(8, Key::Char('a'), SHIFT, Press, Some('A'), Some("A")), "\x1b[97;2u");
    assert_eq!(enc(8 | 4, Key::Char('a'), SHIFT, Press, Some('A'), Some("A")), "\x1b[97:65;2u");
    assert_eq!(enc(8 | 4, Key::Char('1'), SHIFT, Press, Some('!'), Some("!")), "\x1b[49:33;2u");
    assert_eq!(enc(8 | 16, Key::Char('a'), NONE, Press, Some('a'), Some("a")), "\x1b[97;;97u");
    assert_eq!(enc(8 | 16, Key::Char('e'), NONE, Press, Some('e'), Some("é")), "\x1b[101;;233u");
    assert_eq!(enc(8 | 2, Key::Enter, NONE, Release, None, None), "\x1b[13;1:3u");
    assert_eq!(
        enc(8, Key::Modifier(ModifierKey::LeftShift), SHIFT, Press, None, None),
        "\x1b[57441;2u"
    );
    assert_eq!(enc(1, Key::Modifier(ModifierKey::LeftShift), SHIFT, Press, None, None), "");
}

#[test]
fn legacy_mode_is_unchanged() {
    let e = |k, m, kind, s, t| enc(0, k, m, kind, s, t);
    assert_eq!(e(Key::Char('a'), NONE, Press, Some('a'), Some("a")), "a");
    assert_eq!(e(Key::Char('c'), CTRL, Press, Some('c'), None), "\x03");
    assert_eq!(
        e(Key::Char('a'), ALT, Press, Some('A'), Some("A")),
        "\x1bA",
        "Alt uses the shifted char"
    );
    assert_eq!(e(Key::Char('a'), NONE, Release, Some('a'), None), "");
    assert_eq!(e(Key::Modifier(ModifierKey::LeftCtrl), CTRL, Press, None, None), "");
    assert_eq!(e(Key::Up, NONE, Press, None, None), "\x1b[A");
}
