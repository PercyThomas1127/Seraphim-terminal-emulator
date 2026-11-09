//! `seraphim bench`: performance numbers for this machine's GPU, without a
//! window. Prints a report (or JSON with `--json`) to compare hardware and
//! to catch regressions:
//!
//! * parser throughput (terminal output processed per second);
//! * CPU time to prepare a frame (nothing changed, one line, a scroll);
//! * GPU frame time: text alone, over animated shader backgrounds, and
//!   with a backdrop blur.

use std::time::{Duration, Instant};

use seraphim_render::headless::{Headless, FORMAT};
use seraphim_render::{FontConfig, Fonts, Theme, View};
use seraphim_terminal::Terminal;
use seraphim_visual::{LayerKind, LayerSpec, Transition, VisualEngine};

fn output(bytes: usize) -> Vec<u8> {
    // Typical output: colored listings, plain text, some cursor movement.
    let mut out = Vec::with_capacity(bytes + 256);
    let mut i = 0u64;
    while out.len() < bytes {
        i += 1;
        let line = match i % 4 {
            0 => format!("\x1b[1;34mdrwxr-xr-x\x1b[0m  2 user user 4096 Nov  9 {:02}:{:02} \x1b[1;34mdir{i}\x1b[0m\r\n", i % 24, i % 60),
            1 => format!("{} compiling crate number {i} with some ordinary words in it\r\n", i),
            2 => format!("\x1b[32m   Compiling\x1b[0m seraphim-{i} v0.1.0 (/home/user/seraphim/crates/x)\r\n"),
            _ => format!("\x1b[2K\r[{:>3}%] building {i}\x1b[K\r\n", i % 100),
        };
        out.extend_from_slice(line.as_bytes());
    }
    out
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

fn p95(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[(v.len() * 95 / 100).min(v.len() - 1)]
}

struct Row {
    name: String,
    value: f64,
    unit: &'static str,
    detail: String,
}

/// `seraphim bench --list-gpus`.
pub fn list_gpus() {
    let all = seraphim_render::gpu::list();
    if all.is_empty() {
        println!("No GPU adapters found.");
    }
    for a in &all {
        println!("{}", seraphim_render::gpu::describe(a));
    }
    println!();
    println!("Choose one with SERAPHIM_GPU=<part of its name> (or bench --gpu). OpenGL");
    println!("adapters are used only when no Vulkan one fits (or with WGPU_BACKEND=gl).");
}

/// `seraphim bench --window`: open a window with the built-in defaults
/// (and an optional preset), running `cat`, and let the probe type into it.
pub fn window(gpu: Option<String>, preset: Option<String>, keys: usize) -> Result<i32, String> {
    if let Some(g) = &gpu {
        // Read by the window's GPU setup; nothing else runs yet.
        std::env::set_var(seraphim_render::gpu::ENV, g);
    }
    let (mut o, warnings) = crate::profile::load(None, None, preset.as_deref())?;
    if let Some(w) = warnings.first() {
        return Err(w.clone());
    }
    // `cat` in a fresh PTY: the kernel echoes each key, so this measures
    // the terminal, not a shell's line editor.
    o.command = Some(seraphim_session::Command::new("cat"));
    o.probe = Some(keys.max(1));
    // Animations keep running even if the window isn't focused.
    o.visual.animate_unfocused = true;
    o.restore_session = false;
    o.startup_layout = None;
    o.live_reload = false;
    eprintln!("seraphim: bench --window: typing {keys} keys into `cat`, then closing");
    crate::window::run(o)
}

pub fn run(json: bool, frames: usize, gpu: Option<&str>) -> Result<(), String> {
    let mut rows: Vec<Row> = Vec::new();
    let mut push = |name: &str, value: f64, unit: &'static str, detail: String| {
        rows.push(Row { name: name.into(), value, unit, detail });
    };

    // Parser.
    let data = output(32 << 20);
    let mut t = Terminal::with_scrollback(200, 60, 10_000);
    let start = Instant::now();
    for chunk in data.chunks(64 << 10) {
        t.advance(chunk);
    }
    let secs = start.elapsed().as_secs_f64();
    push(
        "parser throughput",
        data.len() as f64 / secs / (1 << 20) as f64,
        "MiB/s",
        "200x60, 10k lines of history".into(),
    );

    let fonts =
        Fonts::new(&FontConfig { family: None, size_px: 16.0 }).map_err(|e| e.to_string())?;
    let mut h = Headless::with_gpu(fonts, Theme::default(), gpu).map_err(|e| e.to_string())?;
    let info = h.adapter_info.clone();

    // CPU: preparing frames.
    for (cols, lines) in [(80usize, 24usize), (200, 60)] {
        let mut t = Terminal::with_scrollback(cols, lines, 1000);
        t.advance(&output(cols * lines * 4));
        let (w, ht) = h.size_for(&t, 4.0);
        let view = View { width: w, height: ht, padding: 4.0, focused: true, cursor_on: true };
        h.renderer.prepare(&h.device, &h.queue, &t, view);
        type Change = fn(&mut Terminal, usize);
        let cases: [(&str, Change); 3] = [
            ("nothing changed", |_, _| {}),
            ("one line changed", |t, i| t.advance(format!("\x1b[5;1H\x1b[2Kline {i}").as_bytes())),
            ("scroll by a line", |t, i| {
                t.advance(format!("\x1b[999;1H\r\nscrolled {i}").as_bytes())
            }),
        ];
        for (name, change) in cases {
            let mut times = Vec::with_capacity(frames);
            for i in 0..frames {
                change(&mut t, i);
                let s = Instant::now();
                h.renderer.prepare(&h.device, &h.queue, &t, view);
                times.push(s.elapsed().as_secs_f64() * 1e6);
            }
            push(
                &format!("prepare {cols}x{lines}: {name}"),
                median(times.clone()),
                "µs",
                format!("p95 {:.0} µs", p95(times)),
            );
        }
    }

    // GPU: whole frames at a typical window size.
    let mut t = Terminal::with_scrollback(200, 60, 0);
    t.advance(&output(200 * 60 * 2));
    let (w, ht) = h.size_for(&t, 4.0);
    let view = View { width: w, height: ht, padding: 4.0, focused: true, cursor_on: true };
    h.renderer.prepare(&h.device, &h.queue, &t, view);
    let scenes: Vec<(&str, Vec<LayerSpec>)> = vec![
        ("text only", vec![]),
        (
            "text over gradient",
            vec![LayerSpec::new(
                "g",
                LayerKind::Gradient {
                    colors: vec![[0.1, 0.0, 0.2, 1.0], [0.0, 0.1, 0.3, 1.0]],
                    angle: 90.0,
                },
            )],
        ),
        (
            "text over aurora shader",
            vec![LayerSpec::new("a", LayerKind::Shader { name: "aurora".into(), source: None })],
        ),
        (
            "text over heavens shader",
            vec![LayerSpec::new("h", LayerKind::Shader { name: "heavens".into(), source: None })],
        ),
        (
            "aurora + backdrop blur",
            vec![
                LayerSpec::new("a", LayerKind::Shader { name: "aurora".into(), source: None }),
                LayerSpec {
                    params: vec![("radius".into(), 12.0)],
                    ..LayerSpec::new("b", LayerKind::Blur)
                },
            ],
        ),
    ];
    for (name, layers) in scenes {
        let mut v = VisualEngine::new(&h.device, &h.queue, FORMAT);
        if let Some(e) = v.set_layers(&layers, Transition::Instant).into_iter().next() {
            return Err(format!("{name}: {e}"));
        }
        let mut times = Vec::with_capacity(frames);
        for i in 0..frames + 5 {
            v.set_fixed_time(Some(i as f64 / 60.0));
            v.prepare(w, ht);
            let v = std::cell::RefCell::new(&mut v);
            let d: Duration =
                h.time_frame(w, ht, &mut |e| v.borrow_mut().encode(e), &mut |r, pass| {
                    v.borrow_mut().render(pass);
                    r.render(pass);
                });
            if i >= 5 {
                times.push(d.as_secs_f64() * 1e3);
            }
        }
        let m = median(times.clone());
        push(
            &format!("frame {w}x{ht}: {name}"),
            m,
            "ms",
            format!("p95 {:.2} ms, ~{:.0} fps", p95(times), 1000.0 / m),
        );
    }

    if json {
        let rows: Vec<_> = rows
            .iter()
            .map(|r| serde_json::json!({ "name": r.name, "value": (r.value * 100.0).round() / 100.0, "unit": r.unit, "detail": r.detail }))
            .collect();
        let doc = serde_json::json!({
            "seraphim": env!("CARGO_PKG_VERSION"),
            "adapter": info.name,
            "backend": format!("{:?}", info.backend),
            "device_type": format!("{:?}", info.device_type),
            "driver": format!("{} {}", info.driver, info.driver_info),
            "debug_build": cfg!(debug_assertions),
            "results": rows,
        });
        println!("{}", serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?);
    } else {
        println!("Seraphim {} benchmark", env!("CARGO_PKG_VERSION"));
        println!(
            "GPU: {} ({:?}, {:?}), driver {} {}",
            info.name, info.backend, info.device_type, info.driver, info.driver_info
        );
        if cfg!(debug_assertions) {
            println!("(debug build: numbers are not representative; use cargo build --release)");
        }
        println!();
        for r in &rows {
            println!("{:<46} {:>9.2} {:<6} {}", r.name, r.value, r.unit, r.detail);
        }
    }
    Ok(())
}
