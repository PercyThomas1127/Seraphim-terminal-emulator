//! Inline images (Kitty graphics, Sixel) through the real wgpu pipeline.

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

fn term(h: &Headless, cols: usize, rows: usize) -> Terminal {
    let mut t = Terminal::new(cols, rows);
    let m = h.renderer.cell_metrics();
    t.set_cell_pixel_size(m.width, m.height);
    t.advance(b"\x1b[?25l");
    t
}

fn render(h: &mut Headless, t: &Terminal) -> Image {
    let (width, height) = h.size_for(t, PAD);
    h.render(t, View { width, height, padding: PAD, focused: true, cursor_on: true })
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

fn kitty(t: &mut Terminal, control: &str, rgba: &[u8]) {
    t.advance(format!("\x1b_G{control};{}\x1b\\", b64(rgba)).as_bytes());
}

fn rgb(p: [u8; 4]) -> [u8; 3] {
    [p[0], p[1], p[2]]
}

#[test]
fn kitty_image_is_drawn_at_its_cell() {
    let Some(mut h) = headless() else { return };
    let mut t = term(&h, 20, 6);
    let m = h.renderer.cell_metrics();
    t.advance(b"\x1b[2;4H");
    // 2-color image: left half red, right half blue, 20x10 px.
    let mut px = Vec::new();
    for _y in 0..10 {
        for x in 0..20 {
            px.extend_from_slice(if x < 10 { &[255, 0, 0, 255] } else { &[0, 0, 255, 255] });
        }
    }
    kitty(&mut t, "a=T,f=32,s=20,v=10,q=2", &px);
    let img = render(&mut h, &t);
    let (x0, y0) = ((PAD + 3.0 * m.width) as u32, (PAD + m.height) as u32);
    assert_eq!(rgb(img.pixel(x0 + 2, y0 + 2)), [255, 0, 0]);
    assert_eq!(rgb(img.pixel(x0 + 17, y0 + 7)), [0, 0, 255]);
    assert_eq!(
        rgb(img.pixel(x0 + 25, y0 + 2)),
        Theme::default().background,
        "natural size: 20 px wide"
    );
    assert_ne!(rgb(img.pixel(x0 - 2, y0 + 2)), [255, 0, 0], "starts at its column");
}

#[test]
fn sized_in_cells_and_z_order() {
    let Some(mut h) = headless() else { return };
    let mut t = term(&h, 10, 3);
    let m = h.renderer.cell_metrics();
    let green = [0u8, 200, 0, 255].repeat(4);
    // A 2x2 px image stretched over 4x2 cells, under the text (z<0).
    t.advance(b"WWWW");
    t.advance(b"\x1b[1;1H");
    kitty(&mut t, "a=T,f=32,s=2,v=2,c=4,r=2,z=-1,C=1,q=2", &green);
    let img = render(&mut h, &t);
    let cx = (PAD + 3.5 * m.width) as u32;
    let cy = (PAD + 1.5 * m.height) as u32;
    assert_eq!(rgb(img.pixel(cx, cy)), [0, 200, 0], "stretched to 4x2 cells");
    // Glyphs of 'W' still drawn on top: some pixels in cell 0 aren't green.
    let (x0, y0) = (PAD as u32, PAD as u32);
    let mut text = 0;
    for y in y0..y0 + m.height as u32 {
        for x in x0..x0 + m.width as u32 {
            if rgb(img.pixel(x, y)) != [0, 200, 0] {
                text += 1;
            }
        }
    }
    assert!(text > 5, "text over a z<0 image ({text} px)");
    // z >= 0 covers the text.
    t.advance(b"\x1b_Ga=d\x1b\\");
    kitty(&mut t, "a=T,f=32,s=2,v=2,c=4,r=2,C=1,q=2", &green);
    let img = render(&mut h, &t);
    for y in y0..y0 + m.height as u32 {
        for x in x0..x0 + m.width as u32 {
            assert_eq!(rgb(img.pixel(x, y)), [0, 200, 0], "image over text at {x},{y}");
        }
    }
    // Deleted: plain again.
    t.advance(b"\x1b_Ga=d\x1b\\");
    let img = render(&mut h, &t);
    assert_eq!(rgb(img.pixel(cx, cy)), Theme::default().background);
}

#[test]
fn transparency_and_scrolling() {
    let Some(mut h) = headless() else { return };
    let mut t = term(&h, 10, 3);
    let m = h.renderer.cell_metrics();
    // Half-transparent white over the theme background.
    kitty(&mut t, "a=T,f=32,s=1,v=1,c=1,r=1,C=1,q=2", &[255, 255, 255, 128]);
    let img = render(&mut h, &t);
    let bg = Theme::default().background;
    let p = rgb(img.pixel(PAD as u32 + 2, PAD as u32 + 2));
    for i in 0..3 {
        let want = (bg[i] as f32 * (1.0 - 128.0 / 255.0) + 128.0) as i32;
        assert!((p[i] as i32 - want).abs() <= 2, "{p:?} vs {want}");
    }
    // Scroll it up a line: it moves with the text.
    t.advance(b"\x1b[3;1H\n");
    let img = render(&mut h, &t);
    assert_eq!(rgb(img.pixel(PAD as u32 + 2, PAD as u32 + 2)), bg, "scrolled off the top");
    t.scroll_display(1);
    let img = render(&mut h, &t);
    assert_ne!(rgb(img.pixel(PAD as u32 + 2, PAD as u32 + 2)), bg, "back when viewing history");
    let _ = m;
}

#[test]
fn sixel_image_renders() {
    let Some(mut h) = headless() else { return };
    let mut t = term(&h, 10, 4);
    t.advance(b"\x1bPq#1;2;0;100;0#1!16~-!16~\x1b\\");
    let img = render(&mut h, &t);
    assert_eq!(rgb(img.pixel(PAD as u32 + 5, PAD as u32 + 5)), [0, 255, 0]);
    assert_eq!(rgb(img.pixel(PAD as u32 + 5, PAD as u32 + 11)), [0, 255, 0]);
    assert_ne!(rgb(img.pixel(PAD as u32 + 20, PAD as u32 + 5)), [0, 255, 0], "16 px wide");
}

#[test]
fn unicode_placeholders_draw_their_slice() {
    use seraphim_terminal::graphics::{DIACRITICS, PLACEHOLDER};
    let Some(mut h) = headless() else { return };
    let mut t = term(&h, 10, 3);
    let m = h.renderer.cell_metrics();
    // 2x1 image (red | blue) shown over 2x1 cells.
    let px = [[255u8, 0, 0, 255], [0, 0, 255, 255]].concat();
    kitty(&mut t, "a=T,U=1,f=32,s=2,v=1,i=7,c=2,r=1,q=2", &px);
    let d = |i: usize| char::from_u32(DIACRITICS[i]).unwrap();
    // Explicit row/column on the first cell, inferred column on the second.
    t.advance(
        format!("\x1b[2;3H\x1b[38;5;7m{PLACEHOLDER}{}{}{PLACEHOLDER}\x1b[m", d(0), d(0)).as_bytes(),
    );
    let img = render(&mut h, &t);
    let at = |col: f32| {
        let x = (PAD + col * m.width) as u32;
        let y = (PAD + 1.5 * m.height) as u32;
        rgb(img.pixel(x, y))
    };
    // The 2:1 image fills the 2x1-cell box's width; vertically centered.
    // (Linear filtering of a 2-pixel image: allow a little bleed.)
    let near =
        |a: [u8; 3], b: [u8; 3]| a.iter().zip(&b).all(|(x, y)| (*x as i32 - *y as i32).abs() <= 24);
    assert!(near(at(2.5), [255, 0, 0]), "first cell: left half {:?}", at(2.5));
    assert!(near(at(3.5), [0, 0, 255]), "second cell (column inferred): right half {:?}", at(3.5));
    assert_eq!(at(4.5), Theme::default().background, "nothing past the placeholders");
}

#[test]
fn kitty_animation_plays() {
    let Some(mut h) = headless() else { return };
    let mut t = term(&h, 10, 3);
    kitty(&mut t, "a=T,f=32,s=1,v=1,c=2,r=1,C=1,i=3,q=2", &[255, 0, 0, 255]);
    kitty(&mut t, "a=f,i=3,f=32,s=1,v=1,z=100,q=2", &[0, 0, 255, 255]);
    t.advance(b"\x1b_Ga=a,i=3,r=1,z=100,s=3,v=1,q=2\x1b\\");
    let epoch = t.graphics().image(3).unwrap().anim.epoch;
    let at = |h: &mut Headless, ms: u64| {
        h.renderer.set_effects_clock(Some(epoch + std::time::Duration::from_millis(ms)));
        let img = render(h, &t);
        rgb(img.pixel(PAD as u32 + 3, PAD as u32 + 3))
    };
    assert_eq!(at(&mut h, 50), [255, 0, 0]);
    assert!(h.renderer.effects_animating(), "keeps drawing while it plays");
    assert_eq!(at(&mut h, 150), [0, 0, 255]);
    assert_eq!(at(&mut h, 250), [255, 0, 0], "loops");
}
