//! Several terminals drawn by one renderer, each in its own slot and
//! viewport, sharing the glyph atlas.

use seraphim_render::headless::Headless;
use seraphim_render::{FontConfig, Fonts, Theme, View};
use seraphim_terminal::Terminal;

#[test]
fn two_panes_side_by_side() {
    let Ok(fonts) = Fonts::new(&FontConfig { family: None, size_px: 16.0 }) else { return };
    let Ok(mut h) = Headless::new(fonts, Theme::default()) else { return };
    let mut left = Terminal::new(8, 3);
    left.advance(b"\x1b[41m        \x1b[0m\x1b[?25l"); // red row 0
    let mut right = Terminal::new(8, 3);
    right.advance(b"\x1b[2;1H\x1b[44m        \x1b[0m\x1b[?25l"); // blue row 1
    let (w, ht) = h.size_for(&left, 0.0);
    let view = View { width: w, height: ht, padding: 0.0, focused: true, cursor_on: true };
    let m = h.renderer.cell_metrics();
    let img = h.render_panes(
        w * 2,
        ht,
        &[
            (0, &left, view, [0.0, 0.0, w as f32, ht as f32]),
            (7, &right, view, [w as f32, 0.0, w as f32, ht as f32]),
        ],
    );
    let red = img.pixel(2, (m.height / 2.0) as u32);
    let blue = img.pixel(w + 2, (m.height * 1.5) as u32);
    let left_row1 = img.pixel(2, (m.height * 1.5) as u32);
    let right_row0 = img.pixel(w + 2, (m.height / 2.0) as u32);
    assert!(red[0] > 200 && red[0] > red[2] + 60, "{red:?}");
    assert!(blue[2] > blue[0] + 60, "{blue:?}");
    assert_eq!(left_row1, right_row0, "both backgrounds elsewhere");
    assert_ne!(left_row1, red);
    // Again, the other way round: slots keep their own state.
    let img2 = h.render_panes(
        w * 2,
        ht,
        &[
            (7, &right, view, [0.0, 0.0, w as f32, ht as f32]),
            (0, &left, view, [w as f32, 0.0, w as f32, ht as f32]),
        ],
    );
    assert_eq!(img2.pixel(2, (m.height * 1.5) as u32), blue);
    assert_eq!(img2.pixel(w + 2, (m.height / 2.0) as u32), red);
    h.renderer.drop_slot(7);
}
