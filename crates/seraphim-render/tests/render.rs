//! Pixel tests through the real wgpu pipeline. Skipped (with a note) when no
//! GPU adapter is available; in CI, Mesa's lavapipe provides one.

use seraphim_render::headless::{Headless, Image};
use seraphim_render::{FontConfig, Fonts, Theme, View};
use seraphim_terminal::Terminal;

const PAD: f32 = 4.0;

fn headless() -> Option<Headless> {
    let fonts = Fonts::new(&FontConfig { family: None, size_px: 16.0 }).expect("fonts");
    match Headless::new(fonts, Theme::default()) {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("skipping: {e}");
            None
        }
    }
}

fn render(h: &mut Headless, term: &Terminal, focused: bool) -> Image {
    let (width, height) = h.size_for(term, PAD);
    h.renderer.prepare(
        &h.device,
        &h.queue,
        term,
        View { width, height, padding: PAD, focused, cursor_on: true },
    );
    h.render(term, View { width, height, padding: PAD, focused, cursor_on: true })
}

/// Pixel bounds of cell (row, col).
fn cell(h: &Headless, row: usize, col: usize) -> (u32, u32, u32, u32) {
    let m = h.renderer.cell_metrics();
    let x = PAD + col as f32 * m.width;
    let y = PAD + row as f32 * m.height;
    (x as u32, y as u32, m.width as u32, m.height as u32)
}

fn rgb(p: [u8; 4]) -> [u8; 3] {
    [p[0], p[1], p[2]]
}

/// Count pixels in a cell that differ from `bg`.
fn ink(img: &Image, (x, y, w, h): (u32, u32, u32, u32), bg: [u8; 3]) -> usize {
    let mut n = 0;
    for yy in y..y + h {
        for xx in x..x + w {
            if rgb(img.pixel(xx, yy)) != bg {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn metrics_are_whole_pixels() {
    let fonts = Fonts::new(&FontConfig { family: None, size_px: 16.0 }).unwrap();
    let m = fonts.metrics();
    for v in [m.width, m.height, m.baseline, m.underline_y, m.stroke] {
        assert_eq!(v, v.round(), "{m:?}");
    }
    assert!(m.width >= 7.0 && m.height > m.width, "{m:?}");
    assert!(m.baseline < m.height && m.underline_y < m.height);
}

#[test]
fn background_text_and_colors() {
    let Some(mut h) = headless() else { return };
    let theme = Theme::default();
    let mut t = Terminal::new(6, 2);
    t.advance(b"A \x1b[41m \x1b[0m\x1b[38;2;0;255;0mB\x1b[?25l");
    let img = render(&mut h, &t, true);

    // Padding and empty cells are the theme background.
    assert_eq!(rgb(img.pixel(0, 0)), theme.background);
    assert_eq!(ink(&img, cell(&h, 1, 3), theme.background), 0, "empty cell");
    // 'A' leaves ink; the space after it does not.
    assert!(ink(&img, cell(&h, 0, 0), theme.background) > 10);
    assert_eq!(ink(&img, cell(&h, 0, 1), theme.background), 0);
    // A red-background space fills its whole cell with ANSI red.
    let (x, y, w, hh) = cell(&h, 0, 2);
    for (px, py) in [(x, y), (x + w - 1, y + hh - 1), (x + w / 2, y + hh / 2)] {
        assert_eq!(rgb(img.pixel(px, py)), theme.ansi[1]);
    }
    // Truecolor glyph: its most intense pixel is pure green.
    let (x, y, w, hh) = cell(&h, 0, 3);
    let greenest = (y..y + hh)
        .flat_map(|yy| (x..x + w).map(move |xx| (xx, yy)))
        .map(|(xx, yy)| img.pixel(xx, yy))
        .max_by_key(|p| p[1])
        .unwrap();
    assert_eq!(rgb(greenest), [0, 255, 0]);
}

#[test]
fn block_cursor_inverts_its_cell() {
    let Some(mut h) = headless() else { return };
    let theme = Theme::default();
    let mut t = Terminal::new(4, 1);
    t.advance(b"x\x1b[1G");
    let img = render(&mut h, &t, true);
    let (x, y, _, _) = cell(&h, 0, 0);
    assert_eq!(rgb(img.pixel(x, y)), theme.cursor, "cursor corner");
    // Unfocused: hollow box, interior stays background at the corner+1.
    let img = render(&mut h, &t, false);
    assert_eq!(rgb(img.pixel(x, y)), theme.cursor);
    assert_eq!(rgb(img.pixel(x + 1, y + 1)), theme.background);
}

#[test]
fn wide_glyph_spans_two_cells() {
    let Some(mut h) = headless() else { return };
    let theme = Theme::default();
    let mut t = Terminal::new(6, 1);
    t.advance("日\x1b[?25l".as_bytes());
    let img = render(&mut h, &t, true);
    let left = ink(&img, cell(&h, 0, 0), theme.background);
    let right = ink(&img, cell(&h, 0, 1), theme.background);
    if left + right == 0 {
        eprintln!("skipping: no CJK font installed");
        return;
    }
    assert!(left > 5 && right > 5, "glyph should cover both halves: {left} {right}");
}

#[test]
fn underline_and_bold_render_differently() {
    let Some(mut h) = headless() else { return };
    let theme = Theme::default();
    let mut t = Terminal::new(6, 1);
    t.advance(b"m\x1b[1mm\x1b[0;4m \x1b[?25l");
    let img = render(&mut h, &t, true);
    let regular = ink(&img, cell(&h, 0, 0), theme.background);
    let bold = ink(&img, cell(&h, 0, 1), theme.background);
    assert!(bold > regular, "bold {bold} vs regular {regular}");
    // Underlined space: ink only on the underline rows.
    let m = h.renderer.cell_metrics();
    let (x, y, w, _) = cell(&h, 0, 2);
    let uy = y + m.underline_y as u32;
    assert_eq!(rgb(img.pixel(x + w / 2, uy)), theme.foreground);
    assert_eq!(rgb(img.pixel(x + w / 2, y + 1)), theme.background);
}

#[test]
fn glyph_cache_is_reused_across_frames() {
    let Some(mut h) = headless() else { return };
    let mut t = Terminal::new(20, 2);
    t.advance(b"hello hello");
    let view = View { width: 400, height: 80, padding: PAD, focused: true, cursor_on: true };
    h.renderer.prepare(&h.device, &h.queue, &t, view);
    let first = h.renderer.stats().glyphs_rasterized;
    assert_eq!(first, 4, "h e l o");
    h.renderer.prepare(&h.device, &h.queue, &t, view);
    assert_eq!(h.renderer.stats().glyphs_rasterized, 0);
}

#[test]
fn atlas_overflow_recovers() {
    let Some(mut h) = headless() else { return };
    // Many distinct glyphs at a large size to exhaust the 2048² atlas.
    let fonts = Fonts::new(&FontConfig { family: None, size_px: 120.0 }).unwrap();
    h.renderer.set_fonts(fonts);
    let mut t = Terminal::new(40, 20);
    let text: String = (0x21u8..0x7f).map(|b| b as char).cycle().take(800).collect();
    t.advance(text.as_bytes());
    let view = View { width: 100, height: 100, padding: 0.0, focused: true, cursor_on: true };
    h.renderer.prepare(&h.device, &h.queue, &t, view);
    assert!(h.renderer.stats().instances > 0);
}

#[test]
#[ignore = "writes a PNG; run with --ignored to refresh target/screenshot.png"]
fn screenshot() {
    let Some(mut h) = headless() else { return };
    let mut t = Terminal::new(64, 14);
    t.advance(include_bytes!("demo.ansi"));
    let img = render(&mut h, &t, true);
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screenshot.png");
    img.save_png(&out).unwrap();
    eprintln!(
        "wrote {} ({}, {})",
        out.display(),
        h.adapter_info.name,
        h.renderer.fonts().family_name
    );
}

#[test]
fn selection_is_highlighted() {
    let Some(mut h) = headless() else { return };
    let theme = Theme::default();
    let mut t = Terminal::new(8, 2);
    t.advance(b"abcdef\x1b[?25l");
    t.start_selection(
        seraphim_terminal::SelectionKind::Simple,
        seraphim_terminal::Point::new(0, 1),
    );
    t.update_selection(seraphim_terminal::Point::new(0, 2));
    let img = render(&mut h, &t, true);
    let (x, y, _, _) = cell(&h, 0, 1);
    assert_eq!(rgb(img.pixel(x, y)), theme.selection_bg, "selected cell");
    let (x, y, _, _) = cell(&h, 0, 3);
    assert_eq!(rgb(img.pixel(x, y)), theme.background, "unselected cell");
}

#[test]
fn scrolled_viewport_shows_history_and_moves_cursor() {
    let Some(mut h) = headless() else { return };
    let theme = Theme::default();
    let mut t = Terminal::new(8, 3);
    t.advance(b"\x1b[41mR\x1b[0m\r\n2\r\n3\r\n4\r\n5"); // red 'R' line scrolls into history
    let img = render(&mut h, &t, true);
    let (x, y, w, _) = cell(&h, 0, 0);
    assert_ne!(rgb(img.pixel(x + w - 1, y)), theme.ansi[1], "red line not visible yet");
    // Cursor is on the last row while live.
    let (cx, cy, _, _) = cell(&h, 2, 1);
    assert_eq!(rgb(img.pixel(cx, cy)), theme.cursor);

    t.scroll_display(2);
    let img = render(&mut h, &t, true);
    assert_eq!(rgb(img.pixel(x + w - 1, y)), theme.ansi[1], "history row 0 shown");
    // Cursor moved off the bottom of the viewport: not drawn anywhere.
    assert_ne!(rgb(img.pixel(cx, cy)), theme.cursor);
    // Scroll indicator appears in the right padding.
    let right = img.width - 2;
    let any_thumb = (0..img.height).any(|yy| rgb(img.pixel(right, yy)) == theme.selection_bg);
    assert!(any_thumb, "scroll indicator drawn");
}

#[test]
fn application_color_overrides_are_rendered() {
    let Some(mut h) = headless() else { return };
    let mut t = Terminal::new(6, 2);
    // OSC 11 sets the background; OSC 4 redefines palette red.
    t.advance(b"\x1b]11;#204060\x07\x1b]4;1;#00ff00\x07\x1b[41m \x1b[0m\x1b[?25l");
    let img = render(&mut h, &t, true);
    assert_eq!(rgb(img.pixel(0, 0)), [0x20, 0x40, 0x60], "padding uses the new background");
    let (x, y, _, _) = cell(&h, 0, 0);
    assert_eq!(rgb(img.pixel(x, y)), [0, 255, 0], "palette entry 1 redefined");
    // Resetting returns to the theme.
    t.advance(b"\x1b]111\x07\x1b]104\x07");
    let img = render(&mut h, &t, true);
    assert_eq!(rgb(img.pixel(0, 0)), Theme::default().background);
    assert_eq!(rgb(img.pixel(x, y)), Theme::default().ansi[1]);
}
