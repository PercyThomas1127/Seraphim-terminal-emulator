//! Frame preparation cost (CPU: building instances; plus the uploads).
//! Run: cargo test --release -p seraphim-render --test bench -- --ignored --nocapture

use std::time::Instant;

use seraphim_render::headless::Headless;
use seraphim_render::{FontConfig, Fonts, Theme, View};
use seraphim_terminal::Terminal;

fn screen(cols: usize, rows: usize) -> Terminal {
    let mut t = Terminal::with_scrollback(cols, rows, 1000);
    let mut s = String::new();
    for r in 0..rows * 2 {
        for c in 0..cols / 10 {
            s.push_str(&format!("\x1b[3{}m{:<9}\x1b[0m ", (r + c) % 8, format!("w{}x{}", r, c)));
        }
        s.push_str("\r\n");
    }
    t.advance(s.as_bytes());
    t
}

fn time(
    h: &mut Headless,
    t: &mut Terminal,
    view: View,
    frames: usize,
    mut change: impl FnMut(&mut Terminal, usize),
) -> (f64, usize, usize) {
    // Warm the glyph atlas.
    h.renderer.prepare(&h.device, &h.queue, t, view);
    let start = Instant::now();
    let (mut rebuilt, mut bytes) = (0, 0);
    for i in 0..frames {
        change(t, i);
        h.renderer.prepare(&h.device, &h.queue, t, view);
        rebuilt += h.renderer.stats().rows_rebuilt;
        bytes += h.renderer.stats().uploaded_bytes;
    }
    (start.elapsed().as_secs_f64() * 1e6 / frames as f64, rebuilt / frames, bytes / frames / 1024)
}

#[test]
#[ignore]
fn prepare_cost() {
    let Ok(fonts) = Fonts::new(&FontConfig { family: None, size_px: 16.0 }) else { return };
    let Ok(mut h) = Headless::new(fonts, Theme::default()) else { return };
    let (cols, rows) = (200, 60);
    let mut t = screen(cols, rows);
    let (w, ht) = h.size_for(&t, 4.0);
    let view = View { width: w, height: ht, padding: 4.0, focused: true, cursor_on: true };
    let _warm = time(&mut h, &mut t, view, 200, |_, _| {});
    let idle = time(&mut h, &mut t, view, 200, |_, _| {});
    let line = time(&mut h, &mut t, view, 200, |t, i| {
        t.advance(format!("\x1b[30;1H\x1b[2Kline {i}").as_bytes())
    });
    let scroll = time(&mut h, &mut t, view, 200, |t, i| {
        t.advance(format!("\x1b[60;1H\r\nscrolled {i}").as_bytes())
    });
    println!("{}", h.adapter_info.name);
    println!(
        "{cols}x{rows}: nothing changed {:>7.0} µs/frame ({} rows rebuilt, {} KiB uploaded)",
        idle.0, idle.1, idle.2
    );
    println!(
        "{cols}x{rows}: one line changed {:>6.0} µs/frame ({} rows rebuilt, {} KiB uploaded)",
        line.0, line.1, line.2
    );
    println!(
        "{cols}x{rows}: full scroll {:>11.0} µs/frame ({} rows rebuilt, {} KiB uploaded)",
        scroll.0, scroll.1, scroll.2
    );
}
