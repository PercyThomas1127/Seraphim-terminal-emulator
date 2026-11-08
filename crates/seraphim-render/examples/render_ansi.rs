//! Render an ANSI byte file to a PNG through the real renderer:
//!   cargo run -p seraphim-render --example render_ansi -- IN.ansi OUT.png [COLS ROWS]
use seraphim_render::headless::Headless;
use seraphim_render::{FontConfig, Fonts, Theme, View};
use seraphim_terminal::Terminal;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (input, out) = (&a[0], &a[1]);
    let cols = a.get(2).and_then(|v| v.parse().ok()).unwrap_or(60);
    let rows = a.get(3).and_then(|v| v.parse().ok()).unwrap_or(12);
    let fonts = Fonts::new(&FontConfig { family: None, size_px: 24.0 }).expect("fonts");
    let mut h = Headless::new(fonts, Theme::default()).expect("adapter");
    let mut t = Terminal::new(cols, rows);
    t.advance(&std::fs::read(input).expect("input"));
    let (width, height) = h.size_for(&t, 6.0);
    let img = h.render(&t, View { width, height, padding: 6.0, focused: true, cursor_on: false });
    img.save_png(out).unwrap();
    println!("{}", t.screen_text());
}
