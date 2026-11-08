//! Render one moment of the animated background, with sample text on top.
//!
//! cargo run --release -p seraphim-render --example background -- \
//!     [--style aurora|nebula|starfield] [--time S] [--privileged] \
//!     [--install PHASE] [--intensity X] [--size WxH] [--font FAMILY] OUT.png
use seraphim_render::background::INSTALL_WAVE;
use seraphim_render::headless::Headless;
use seraphim_render::{BackgroundConfig, BackgroundStyle, FontConfig, Fonts, Theme, View};
use seraphim_terminal::Terminal;

const SAMPLE: &str = "\x1b[1;38;5;213m✦ seraphim\x1b[0m \x1b[2m~/projects/divinity\x1b[0m\r\n\
\x1b[38;5;111m❯\x1b[0m gabriel bestow firefox\r\n\
  resolving dependencies... \x1b[32mdone\x1b[0m\r\n\
  downloading firefox-148.0 \x1b[38;5;222m[########----------]\x1b[0m 46%\r\n\
\r\n\
\x1b[38;5;111m❯\x1b[0m ls -la\r\n\
drwxr-xr-x  \x1b[1;34mcrates\x1b[0m   \x1b[1;34mdocs\x1b[0m   README.md   Cargo.toml   \x1b[32mrun.sh\x1b[0m\r\n\
\r\n\
\x1b[38;5;111m❯\x1b[0m cat notes.txt\r\n\
The quick brown fox jumps over the lazy seraph. 0123456789 {}[]()<>=+-*/\r\n\
\x1b[3mitalic\x1b[0m \x1b[1mbold\x1b[0m \x1b[4:3;58:5:204mcurly\x1b[0m \x1b[7m inverse \x1b[0m \x1b[48;5;236m explicit background \x1b[0m 日本語\r\n\
\r\n\
\x1b[38;5;111m❯\x1b[0m ";

fn main() {
    let mut args = std::env::args().skip(1);
    let mut cfg = BackgroundConfig { style: BackgroundStyle::Aurora, ..Default::default() };
    let (mut time, mut privileged, mut install) = (12.0f32, false, None::<f32>);
    let (mut w, mut h) = (1400u32, 820u32);
    let mut family = None;
    let mut out = None;
    while let Some(a) = args.next() {
        let mut val = || args.next().expect("missing value");
        match a.as_str() {
            "--style" => {
                cfg.style = match val().as_str() {
                    "aurora" => BackgroundStyle::Aurora,
                    "nebula" => BackgroundStyle::Nebula,
                    "starfield" => BackgroundStyle::Starfield,
                    "none" => BackgroundStyle::None,
                    s => panic!("unknown style {s}"),
                }
            }
            "--time" => time = val().parse().unwrap(),
            "--intensity" => cfg.intensity = val().parse().unwrap(),
            "--privileged" => privileged = true,
            "--install" => install = Some(val().parse().unwrap()),
            "--size" => {
                let v = val();
                let (a, b) = v.split_once('x').expect("WxH");
                (w, h) = (a.parse().unwrap(), b.parse().unwrap());
            }
            "--font" => family = Some(val()),
            s => out = Some(s.to_owned()),
        }
    }
    let out = out.expect("usage: background [options] OUT.png");

    let fonts = Fonts::new(&FontConfig { family, size_px: 22.0 }).expect("fonts");
    let mut hl = Headless::new(fonts, Theme::default()).expect("adapter");
    hl.renderer.set_background(cfg);
    let m = hl.renderer.cell_metrics();
    let pad = 16.0;
    let cols = ((w as f32 - 2.0 * pad) / m.width) as usize;
    let rows = ((h as f32 - 2.0 * pad) / m.height) as usize;
    let mut term = Terminal::new(cols, rows);
    term.advance(SAMPLE.as_bytes());
    let view = View { width: w, height: h, padding: pad, focused: true, cursor_on: true };

    // Walk the clock up to `time` so moods have settled (or are mid-wave).
    let step = |hl: &mut Headless, t: f32| {
        hl.renderer.set_time(t);
        hl.renderer.prepare(&hl.device, &hl.queue, &term, view);
    };
    step(&mut hl, (time - 5.0).max(0.0));
    if privileged {
        hl.renderer.background_mut().set_privileged(true);
    }
    if let Some(p) = install {
        step(&mut hl, time - p * INSTALL_WAVE);
        hl.renderer.background_mut().pulse_install();
    }
    hl.renderer.set_time(time);
    let img = hl.render(&term, view);
    img.save_png(&out).expect("write png");
    println!("wrote {out} ({w}x{h}) [{}]", hl.adapter_info.name);
}
