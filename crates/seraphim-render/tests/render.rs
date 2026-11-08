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

// ---- grapheme clusters, emoji, icons, decorations --------------------------

fn render_text(h: &mut Headless, cols: usize, s: &str) -> Image {
    let mut t = Terminal::new(cols, 1);
    t.advance(format!("{s}\x1b[?25l").as_bytes());
    render(h, &t, true)
}

/// Number of strongly colored (saturated) pixels in a pixel rectangle.
fn colorful(img: &Image, (x, y, w, h): (u32, u32, u32, u32)) -> usize {
    let mut n = 0;
    for yy in y..y + h {
        for xx in x..x + w {
            let p = img.pixel(xx, yy);
            let (mx, mn) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
            // Theme text and its antialiasing stay near-grey (spread < 20).
            if mx as i32 - mn as i32 > 30 {
                n += 1;
            }
        }
    }
    n
}

fn have_font(word: &str) -> bool {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let ok = db.faces().any(|f| f.families.iter().any(|(n, _)| n.to_lowercase().contains(word)));
    if !ok {
        eprintln!("skipping: no {word} font installed");
    }
    ok
}

#[test]
fn combining_accent_renders_like_the_precomposed_character() {
    let Some(mut h) = headless() else { return };
    let decomposed = render_text(&mut h, 4, "e\u{301}");
    let precomposed = render_text(&mut h, 4, "é");
    let diff = decomposed.rgba.iter().zip(&precomposed.rgba).filter(|(a, b)| a != b).count();
    assert_eq!(diff, 0, "e + U+0301 must look exactly like é ({diff} bytes differ)");
}

#[test]
fn invisible_codepoints_are_never_drawn() {
    let Some(mut h) = headless() else { return };
    let plain = render_text(&mut h, 4, "ab");
    for s in ["a\u{200d}b", "a\u{fe0f}b", "a\u{200b}b"] {
        let img = render_text(&mut h, 4, s);
        let diff = img.rgba.iter().zip(&plain.rgba).filter(|(a, b)| a != b).count();
        assert_eq!(diff, 0, "{s:?} differs from \"ab\" in {diff} bytes");
    }
}

#[test]
fn emoji_sequences_draw_one_color_glyph_in_two_cells() {
    if !have_font("emoji") {
        return;
    }
    let Some(mut h) = headless() else { return };
    let m = h.renderer.cell_metrics();
    let two = |h: &Headless, col| {
        let (x, y, _, ch) = cell(h, 0, col);
        (x, y, (2.0 * m.width) as u32, ch)
    };
    for seq in ["🇯🇵", "👩\u{200d}💻", "👍🏽", "❤\u{fe0f}", "1\u{fe0f}\u{20e3}"] {
        // Sequence in cells 0-1, then nothing: cells 2-5 must stay empty
        // (no second emoji, no box for a joiner or selector).
        let img = render_text(&mut h, 6, seq);
        assert!(colorful(&img, two(&h, 0)) > 20, "{seq:?} should be a color glyph");
        let theme = Theme::default();
        for col in 2..6 {
            assert_eq!(
                ink(&img, cell(&h, 0, col), theme.background),
                0,
                "{seq:?}: stray glyph in col {col}"
            );
        }
    }
    // Text presentation stays monochrome.
    let img = render_text(&mut h, 6, "❤");
    assert_eq!(colorful(&img, cell(&h, 0, 0)), 0);
}

#[test]
fn nerd_font_icons_fill_the_cell_height() {
    if !have_font("nerd") {
        return;
    }
    let Some(mut h) = headless() else { return };
    let theme = Theme::default();
    let m = h.renderer.cell_metrics();
    for icon in ['\u{f004}', '\u{f113}', '\u{e702}'] {
        let img = render_text(&mut h, 4, &format!("{icon} x"));
        // Vertical extent of the icon's ink within cells 0-1.
        let (x0, y0, _, ch) = cell(&h, 0, 0);
        let rows: Vec<u32> = (y0..y0 + ch)
            .filter(|&yy| {
                (x0..x0 + 2 * m.width as u32).any(|xx| rgb(img.pixel(xx, yy)) != theme.background)
            })
            .collect();
        let extent = rows.last().unwrap_or(&0).saturating_sub(*rows.first().unwrap_or(&0)) + 1;
        assert!(
            extent as f32 >= 0.6 * m.height,
            "U+{:X} only {extent}px of {}px",
            icon as u32,
            m.height
        );
    }
}

#[test]
fn overline_dotted_and_dashed_are_drawn() {
    let Some(mut h) = headless() else { return };
    let theme = Theme::default();
    let m = h.renderer.cell_metrics();
    let img = render_text(&mut h, 8, "\x1b[53m \x1b[0;4:4m    \x1b[0;4:5m   ");
    let (x, y, w, _) = cell(&h, 0, 0);
    assert_eq!(rgb(img.pixel(x + w / 2, y)), theme.foreground, "overline at the top of the cell");
    // Dotted: the underline row alternates ink and gaps.
    let uy = y + m.underline_y as u32;
    let (dx, _, _, _) = cell(&h, 0, 1);
    let row: Vec<bool> =
        (dx..dx + 4 * w).map(|xx| rgb(img.pixel(xx, uy)) != theme.background).collect();
    let transitions = row.windows(2).filter(|p| p[0] != p[1]).count();
    assert!(transitions > 8, "dotted underline should be broken ({transitions} transitions)");
    // Dashed: fewer, longer pieces than dotted, but still broken.
    let (sx, _, _, _) = cell(&h, 0, 5);
    let row: Vec<bool> =
        (sx..sx + 3 * w).map(|xx| rgb(img.pixel(xx, uy)) != theme.background).collect();
    let t2 = row.windows(2).filter(|p| p[0] != p[1]).count();
    assert!(t2 >= 3 && t2 < transitions, "dashed: {t2} transitions");
}

mod background {
    use super::*;
    use seraphim_render::{BackgroundConfig, BackgroundStyle};

    fn frame(h: &mut Headless, term: &Terminal, t: f32) -> Image {
        let (width, height) = (480, 320);
        h.renderer.set_time(t);
        h.render(term, View { width, height, padding: PAD, focused: true, cursor_on: true })
    }

    fn mean(img: &Image) -> [f32; 3] {
        let mut sum = [0.0f32; 3];
        for p in img.rgba.chunks(4) {
            for i in 0..3 {
                sum[i] += p[i] as f32;
            }
        }
        let n = (img.rgba.len() / 4) as f32;
        sum.map(|s| s / n)
    }

    #[test]
    fn styles_draw_behind_text_and_animate() {
        let Some(mut h) = headless() else { return };
        let mut term = Terminal::new(20, 4);
        term.advance(b"seraphim");
        let bg = Theme::default().background;
        for style in [BackgroundStyle::Aurora, BackgroundStyle::Nebula, BackgroundStyle::Starfield]
        {
            h.renderer.set_background(BackgroundConfig { style, ..Default::default() });
            let a = frame(&mut h, &term, 10.0);
            let b = frame(&mut h, &term, 14.0);
            let changed = a.rgba.iter().zip(&b.rgba).filter(|(x, y)| x != y).count();
            assert!(changed > a.rgba.len() / 50, "{style:?} should animate ({changed} bytes)");
            let off_theme =
                a.rgba.chunks(4).filter(|p| rgb([p[0], p[1], p[2], p[3]]) != bg).count();
            assert!(off_theme > a.rgba.len() / 8, "{style:?} should paint the background");
            // Text still composites on top: the glyph cell holds bright ink.
            let (x, y, w, hh) = cell(&h, 0, 0);
            let bright = (y..y + hh)
                .flat_map(|yy| (x..x + w).map(move |xx| (xx, yy)))
                .filter(|&(xx, yy)| a.pixel(xx, yy)[0] > 200)
                .count();
            assert!(bright > 5, "{style:?}: text should stay visible over the background");
        }
    }

    #[test]
    fn none_is_the_plain_theme() {
        let Some(mut h) = headless() else { return };
        let term = Terminal::new(20, 4);
        h.renderer.set_background(BackgroundConfig::default());
        let img = frame(&mut h, &term, 3.0);
        assert_eq!(rgb(img.pixel(200, 200)), Theme::default().background);
    }

    #[test]
    fn privileged_mood_warms_the_scene() {
        let Some(mut h) = headless() else { return };
        let term = Terminal::new(20, 4);
        h.renderer.set_background(BackgroundConfig {
            style: BackgroundStyle::Aurora,
            ..Default::default()
        });
        let calm = mean(&frame(&mut h, &term, 20.0));
        h.renderer.background_mut().set_privileged(true);
        assert!(h.renderer.background().needs_frames());
        let _ = frame(&mut h, &term, 20.5); // mid-fade
        let root = mean(&frame(&mut h, &term, 22.0));
        assert!(root[0] > calm[0] + 3.0, "red should rise: {calm:?} -> {root:?}");
        assert!(root[0] - root[2] > calm[0] - calm[2], "scene should shift warm");
    }

    #[test]
    fn install_wave_runs_once() {
        let Some(mut h) = headless() else { return };
        let term = Terminal::new(20, 4);
        h.renderer.set_background(BackgroundConfig {
            style: BackgroundStyle::Aurora,
            speed: 0.0,
            ..Default::default()
        });
        let _ = frame(&mut h, &term, 5.0);
        assert!(!h.renderer.background().needs_frames(), "speed 0 is a still image");
        h.renderer.background_mut().pulse_install();
        let _ = frame(&mut h, &term, 6.0);
        assert!(h.renderer.background().install_phase().is_some());
        assert!(h.renderer.background().needs_frames(), "the wave animates even when still");
        let _ = frame(&mut h, &term, 9.0);
        assert!(h.renderer.background().install_phase().is_none());
        assert!(!h.renderer.background().needs_frames());
    }
}
