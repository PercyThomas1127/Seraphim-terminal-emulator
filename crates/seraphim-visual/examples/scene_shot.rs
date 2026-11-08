//! Screenshot a scene behind terminal text:
//!   cargo run -p seraphim-visual --example scene_shot -- OUT.png [TIME] [ANSI_FILE]
use seraphim_render::headless::{Headless, FORMAT};
use seraphim_render::{FontConfig, Fonts, Theme, View};
use seraphim_terminal::Terminal;
use seraphim_visual::*;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let out = &a[0];
    let time: f64 = a.get(1).and_then(|t| t.parse().ok()).unwrap_or(4.0);
    let text = a
        .get(2)
        .map(|p| std::fs::read(p).unwrap())
        .unwrap_or_else(|| b"  Seraphim visual engine\r\n".to_vec());
    let fonts = Fonts::new(&FontConfig { family: None, size_px: 18.0 }).unwrap();
    let mut h = Headless::new(fonts, Theme::default()).unwrap();
    let mut v = VisualEngine::new(&h.device, &h.queue, FORMAT);
    v.set_fixed_time(Some(time));
    let errors = v.set_layers(
        &[
            LayerSpec::new(
                "sky",
                LayerKind::Gradient {
                    colors: vec![
                        [0.06, 0.05, 0.14, 1.0],
                        [0.16, 0.09, 0.26, 1.0],
                        [0.05, 0.06, 0.12, 1.0],
                    ],
                    angle: 90.0,
                },
            ),
            LayerSpec {
                opacity: 0.9,
                ..LayerSpec::new(
                    "stars",
                    LayerKind::Shader { name: "starfield".into(), source: None },
                )
            },
            LayerSpec {
                opacity: 0.75,
                blend: Blend::Screen,
                ..LayerSpec::new(
                    "aurora",
                    LayerKind::Shader { name: "aurora".into(), source: None },
                )
            },
        ],
        Transition::Instant,
    );
    assert!(errors.is_empty(), "{errors:?}");
    let mut t = Terminal::new(64, 14);
    t.advance(&text);
    t.advance(b"\x1b[?25l");
    let (width, height) = h.size_for(&t, 8.0);
    v.prepare(width, height);
    let img = h.render_with(
        &t,
        View { width, height, padding: 8.0, focused: true, cursor_on: true },
        &mut |p| v.render(p),
    );
    img.save_png(out).unwrap();
}
