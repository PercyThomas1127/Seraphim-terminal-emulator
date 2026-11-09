//! Partial redraw (spec §4.4): reusing cached rows must draw exactly what
//! a full rebuild draws, through edits, scrolling, selection, cursor moves
//! and color changes.

use seraphim_render::headless::Headless;
use seraphim_render::{FontConfig, Fonts, Theme, View};
use seraphim_terminal::{Point, SelectionKind, Terminal};

#[test]
fn cached_rows_draw_like_a_full_rebuild() {
    let Ok(fonts) = Fonts::new(&FontConfig { family: None, size_px: 14.0 }) else { return };
    let Ok(mut h) = Headless::new(fonts, Theme::default()) else { return };
    let mut t = Terminal::with_scrollback(40, 10, 100);
    let (w, ht) = h.size_for(&t, 3.0);
    let view = View { width: w, height: ht, padding: 3.0, focused: true, cursor_on: true };
    let rect = [0.0, 0.0, w as f32, ht as f32];
    type Step = Box<dyn Fn(&mut Terminal)>;
    let steps: Vec<(&str, Step)> = vec![
        (
            "text",
            Box::new(|t| {
                t.advance(b"\x1b[31mred\x1b[0m plain \x1b[1;4mbold under\x1b[0m\r\nline two")
            }),
        ),
        ("nothing", Box::new(|_| {})),
        ("edit one row", Box::new(|t| t.advance(b"\x1b[1;5Hxx"))),
        ("wide and emoji", Box::new(|t| t.advance("\x1b[3;1H漢字 ✓ é\u{301}".as_bytes()))),
        ("scroll", Box::new(|t| t.advance(b"\x1b[10;1H\r\n\r\nafter scroll\r\n"))),
        ("cursor moves", Box::new(|t| t.advance(b"\x1b[2;3H"))),
        (
            "selection",
            Box::new(|t| {
                t.start_selection(SelectionKind::Simple, Point::new(1, 2));
                t.update_selection(Point::new(3, 10));
            }),
        ),
        ("selection cleared", Box::new(|t| t.clear_selection())),
        ("palette change", Box::new(|t| t.advance(b"\x1b]4;1;rgb:00/ff/00\x1b\\"))),
        ("reverse video", Box::new(|t| t.advance(b"\x1b[?5h"))),
        ("history view", Box::new(|t| t.scroll_display(2))),
        ("back to live", Box::new(|t| t.scroll_to_bottom())),
        ("clear", Box::new(|t| t.advance(b"\x1b[2J\x1b[Hfresh"))),
    ];
    for (name, step) in steps {
        step(&mut t);
        // Slot 0 keeps its cache between frames; slot 1 starts empty.
        let incremental = h.render_panes(w, ht, &[(0, &t, view, rect)]);
        h.renderer.drop_slot(1);
        let full = h.render_panes(w, ht, &[(1, &t, view, rect)]);
        assert!(incremental.rgba == full.rgba, "{name}: cached rows differ from a full rebuild");
    }
}

#[test]
fn unchanged_rows_are_reused() {
    let Ok(fonts) = Fonts::new(&FontConfig { family: None, size_px: 14.0 }) else { return };
    let Ok(mut h) = Headless::new(fonts, Theme::default()) else { return };
    let mut t = Terminal::with_scrollback(40, 10, 100);
    t.advance(b"one\r\ntwo\r\nthree");
    let (w, ht) = h.size_for(&t, 3.0);
    let view = View { width: w, height: ht, padding: 3.0, focused: true, cursor_on: true };
    h.renderer.prepare(&h.device, &h.queue, &t, view);
    assert_eq!(h.renderer.stats().rows_rebuilt, 10, "first frame builds everything");
    h.renderer.prepare(&h.device, &h.queue, &t, view);
    assert_eq!(h.renderer.stats().rows_rebuilt, 0);
    assert_eq!(h.renderer.stats().uploaded_bytes, 0);
    t.advance(b"\x1b[1;1Hx");
    h.renderer.prepare(&h.device, &h.queue, &t, view);
    assert_eq!(h.renderer.stats().rows_rebuilt, 2, "the edited row and the one the cursor left");
    t.advance(b"y");
    h.renderer.prepare(&h.device, &h.queue, &t, view);
    assert_eq!(h.renderer.stats().rows_rebuilt, 1, "typing on the cursor's row");
    t.advance(b"\x1b[10;1H\r\n");
    h.renderer.prepare(&h.device, &h.queue, &t, view);
    assert!(
        h.renderer.stats().rows_rebuilt <= 2,
        "scrolled rows are reused: {}",
        h.renderer.stats().rows_rebuilt
    );
}
