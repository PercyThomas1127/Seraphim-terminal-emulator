//! CPU cost of building one full frame: `cargo run --release -p seraphim-render --example frame_time`
use std::time::Instant;

use seraphim_render::headless::Headless;
use seraphim_render::{FontConfig, Fonts, Theme, View};
use seraphim_terminal::Terminal;

fn main() {
    let fonts = Fonts::new(&FontConfig { family: None, size_px: 16.0 }).expect("fonts");
    let mut h = Headless::new(fonts, Theme::default()).expect("adapter");
    let mut term = Terminal::new(200, 50);
    // Every cell occupied, varied colors and attributes.
    for row in 0..50 {
        for col in 0..200 {
            let c = (b'!' + ((row * 7 + col) % 90) as u8) as char;
            let sgr = format!("\x1b[{};{}m{c}", 31 + (col % 7), if col % 5 == 0 { 1 } else { 22 });
            term.advance(sgr.as_bytes());
        }
    }
    let view = View { width: 1700, height: 900, padding: 4.0, focused: true, cursor_on: true };
    h.renderer.prepare(&h.device, &h.queue, &term, view); // warm the glyph cache
    let n = 200;
    let start = Instant::now();
    for _ in 0..n {
        h.renderer.prepare(&h.device, &h.queue, &term, view);
    }
    let per = start.elapsed().as_secs_f64() * 1000.0 / n as f64;
    println!(
        "prepare(200x50, all cells) = {per:.3} ms/frame, {} instances  [{}]",
        h.renderer.stats().instances,
        h.adapter_info.name
    );
}
