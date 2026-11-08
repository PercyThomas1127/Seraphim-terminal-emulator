//! Visual Engine through the real wgpu pipeline (headless). Skipped when no
//! GPU adapter is available.

use seraphim_render::headless::{Headless, Image, FORMAT};
use seraphim_render::{FontConfig, Fonts, Theme, View};
use seraphim_terminal::Terminal;
use seraphim_visual::*;

struct Rig {
    h: Headless,
    v: VisualEngine,
}

fn rig() -> Option<Rig> {
    let fonts = Fonts::new(&FontConfig { family: None, size_px: 16.0 }).ok()?;
    let h = match Headless::new(fonts, Theme::default()) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("skipping: {e}");
            return None;
        }
    };
    let mut v = VisualEngine::new(&h.device, &h.queue, FORMAT);
    v.set_fixed_time(Some(0.0));
    Some(Rig { h, v })
}

impl Rig {
    fn frame(&mut self, text: &str) -> Image {
        let mut t = Terminal::new(10, 2);
        t.advance(format!("{text}\x1b[?25l").as_bytes());
        let (width, height) = self.h.size_for(&t, 4.0);
        let view = View { width, height, padding: 4.0, focused: true, cursor_on: true };
        self.v.prepare(width, height);
        let v = std::cell::RefCell::new(&mut self.v);
        self.h.render_with_encoder(&t, view, &mut |e| v.borrow_mut().encode(e), &mut |pass| {
            v.borrow_mut().render(pass)
        })
    }
}

fn rgb(p: [u8; 4]) -> [i32; 3] {
    [p[0] as i32, p[1] as i32, p[2] as i32]
}

fn near(a: [i32; 3], b: [i32; 3], tol: i32) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= tol)
}

fn solid(id: &str, c: Rgba) -> LayerSpec {
    LayerSpec::new(id, LayerKind::Solid(c))
}

#[test]
fn solid_layer_with_opacity_and_text_on_top() {
    let Some(mut r) = rig() else { return };
    assert!(r.v.set_layers(&[solid("bg", [1.0, 0.0, 0.0, 1.0])], Transition::Instant).is_empty());
    let img = r.frame("");
    assert_eq!(rgb(img.pixel(1, 1)), [255, 0, 0]);
    // 50% white over the theme background.
    r.v.set_opacity("bg", 0.5, Transition::Instant);
    r.v.set_layers(
        &[LayerSpec { opacity: 0.5, ..solid("w", [1.0, 1.0, 1.0, 1.0]) }],
        Transition::Instant,
    );
    let bg = Theme::default().background;
    let want = [0, 1, 2].map(|i| (bg[i] as i32 + 255) / 2);
    assert!(near(rgb(r.frame("").pixel(1, 1)), want, 2));
}

#[test]
fn text_stays_crisp_over_any_background() {
    let Some(mut r) = rig() else { return };
    let fg = Theme::default().foreground.map(|c| c as i32);
    for layer in [
        solid("a", [1.0, 0.0, 0.0, 1.0]),
        LayerSpec::new("b", LayerKind::Shader { name: "aurora".into(), source: None }),
    ] {
        r.v.set_layers(&[layer], Transition::Instant);
        let img = r.frame("H");
        // The glyph's core pixels are exactly the foreground color.
        let exact = (0..img.height)
            .flat_map(|y| (0..img.width).map(move |x| (x, y)))
            .filter(|&(x, y)| rgb(img.pixel(x, y)) == fg)
            .count();
        assert!(exact > 10, "only {exact} exact foreground pixels");
    }
}

#[test]
fn gradient_runs_along_its_angle() {
    let Some(mut r) = rig() else { return };
    let g = LayerKind::Gradient {
        colors: vec![[0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]],
        angle: 0.0,
    };
    r.v.set_layers(&[LayerSpec::new("g", g)], Transition::Instant);
    let img = r.frame("");
    let (w, y) = (img.width, img.height / 2);
    let (l, m, rr) =
        (rgb(img.pixel(0, y))[0], rgb(img.pixel(w / 2, y))[0], rgb(img.pixel(w - 1, y))[0]);
    assert!(l < 10 && rr > 245 && (m - 128).abs() < 12, "{l} {m} {rr}");
    // 90 degrees: top to bottom.
    let g = LayerKind::Gradient {
        colors: vec![[0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]],
        angle: 90.0,
    };
    r.v.set_layers(&[LayerSpec::new("g", g)], Transition::Instant);
    let img = r.frame("");
    assert!(rgb(img.pixel(1, 0))[0] < 20 && rgb(img.pixel(1, img.height - 1))[0] > 235);
}

#[test]
fn image_layer_fits() {
    let Some(mut r) = rig() else { return };
    let dir = std::env::temp_dir().join(format!("seraphim-visual-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("quad.png");
    {
        let f = std::fs::File::create(&path).unwrap();
        let mut e = png::Encoder::new(f, 2, 2);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header()
            .unwrap()
            .write_image_data(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255])
            .unwrap();
    }
    r.v.set_layers(
        &[LayerSpec::new(
            "i",
            LayerKind::Image { path: path.clone(), fit: Fit::Stretch, blur: 0.0 },
        )],
        Transition::Instant,
    );
    let img = r.frame("");
    let (w, h) = (img.width, img.height);
    assert!(near(rgb(img.pixel(1, 1)), [255, 0, 0], 8), "top-left red");
    assert!(near(rgb(img.pixel(w - 2, h - 2)), [255, 255, 255], 8), "bottom-right white");
    // Missing files are reported, not fatal.
    let errs = r.v.set_layers(
        &[LayerSpec::new(
            "x",
            LayerKind::Image { path: dir.join("nope.png"), fit: Fit::Cover, blur: 0.0 },
        )],
        Transition::Instant,
    );
    assert_eq!(errs.len(), 1, "{errs:?}");
}

#[test]
fn builtin_shaders_render_and_move_over_time() {
    let Some(mut r) = rig() else { return };
    let bg = Theme::default().background.map(|c| c as i32);
    for name in ["aurora", "starfield"] {
        let mut spec = LayerSpec::new(name, LayerKind::Shader { name: name.into(), source: None });
        if name == "starfield" {
            spec.params = vec![("density".into(), 1.0), ("size".into(), 2.0)];
        }
        assert!(r.v.set_layers(&[spec], Transition::Instant).is_empty());
        r.v.set_fixed_time(Some(1.0));
        let a = r.frame("");
        r.v.set_fixed_time(Some(9.0));
        let b = r.frame("");
        let lit = (0..a.height)
            .flat_map(|y| (0..a.width).map(move |x| (x, y)))
            .filter(|&(x, y)| rgb(a.pixel(x, y)) != bg)
            .count();
        assert!(lit > 50, "{name} draws something ({lit} px)");
        assert_ne!(a.rgba, b.rgba, "{name} changes over time");
        assert!(r.v.animating(), "{name} needs frames");
    }
}

#[test]
fn user_shader_with_animated_parameter() {
    let Some(mut r) = rig() else { return };
    let src = "fn shade(uv: vec2<f32>, px: vec2<f32>) -> vec4<f32> { return vec4<f32>(p(P_red), 0.0, 0.0, 1.0); }";
    let spec = LayerSpec {
        params: vec![("red".into(), 0.0)],
        ..LayerSpec::new("u", LayerKind::Shader { name: "mine".into(), source: Some(src.into()) })
    };
    assert!(r.v.set_layers(&[spec], Transition::Instant).is_empty());
    assert!(!r.v.animating(), "no u.time and nothing moving: no frames needed");
    assert_eq!(rgb(r.frame("").pixel(1, 1)), [0, 0, 0]);
    r.v.set_param("u", "red", 1.0, Transition::Tween { duration: 2.0, easing: Easing::Linear });
    assert!(r.v.animating());
    r.v.set_fixed_time(Some(1.0));
    assert!(near(rgb(r.frame("").pixel(1, 1)), [128, 0, 0], 2), "halfway");
    r.v.set_fixed_time(Some(3.0));
    assert_eq!(rgb(r.frame("").pixel(1, 1)), [255, 0, 0]);
    assert!(!r.v.animating(), "tween finished");
}

#[test]
fn broken_shaders_are_reported_not_fatal() {
    let Some(mut r) = rig() else { return };
    let bad = LayerSpec::new(
        "bad",
        LayerKind::Shader { name: "x".into(), source: Some("fn shade(oops".into()) },
    );
    let unknown_param = LayerSpec {
        params: vec![("nonsense".into(), 1.0)],
        ..LayerSpec::new("p", LayerKind::Shader { name: "aurora".into(), source: None })
    };
    let unknown_shader =
        LayerSpec::new("s", LayerKind::Shader { name: "nebulae".into(), source: None });
    let errs = r.v.set_layers(
        &[bad, unknown_param, unknown_shader, solid("ok", [0.0, 0.0, 1.0, 1.0])],
        Transition::Instant,
    );
    assert_eq!(errs.len(), 3, "{errs:#?}");
    assert!(errs[0].contains("did not compile"), "{}", errs[0]);
    assert!(errs[1].contains("speed"), "lists the real parameters: {}", errs[1]);
    assert!(errs[2].contains("aurora"), "lists built-ins: {}", errs[2]);
    assert_eq!(r.v.layer_ids(), ["ok"], "the valid layer is still shown");
    assert_eq!(rgb(r.frame("").pixel(1, 1)), [0, 0, 255]);
}

#[test]
fn blend_modes() {
    let Some(mut r) = rig() else { return };
    let base = solid("base", [0.5, 0.5, 0.5, 1.0]);
    let run = |r: &mut Rig, blend, c| {
        r.v.set_layers(
            &[base.clone(), LayerSpec { blend, ..solid("top", c) }],
            Transition::Instant,
        );
        rgb(r.frame("").pixel(1, 1))
    };
    assert!(near(run(&mut r, Blend::Multiply, [0.5, 1.0, 0.0, 1.0]), [64, 128, 0], 2));
    assert!(near(run(&mut r, Blend::Add, [0.25, 0.25, 0.25, 1.0]), [191, 191, 191], 2));
    assert!(near(run(&mut r, Blend::Screen, [0.5, 0.5, 0.5, 1.0]), [191, 191, 191], 2));
}

#[test]
fn scene_changes_crossfade_and_old_layers_are_removed() {
    let Some(mut r) = rig() else { return };
    r.v.set_layers(&[solid("old", [1.0, 0.0, 0.0, 1.0])], Transition::Instant);
    r.v.set_layers(
        &[solid("new", [0.0, 0.0, 1.0, 1.0])],
        Transition::Tween { duration: 1.0, easing: Easing::Linear },
    );
    r.v.set_fixed_time(Some(0.5));
    let mid = rgb(r.frame("").pixel(1, 1));
    assert!(mid[0] > 40 && mid[2] > 40, "both visible mid-fade: {mid:?}");
    r.v.set_fixed_time(Some(2.0));
    assert_eq!(rgb(r.frame("").pixel(1, 1)), [0, 0, 255]);
    assert_eq!(r.v.layer_ids(), ["new"]);
}

#[test]
fn paused_and_reduced_motion_stop_frames() {
    let Some(mut r) = rig() else { return };
    r.v.set_fixed_time(None);
    r.v.set_layers(
        &[LayerSpec::new("a", LayerKind::Shader { name: "aurora".into(), source: None })],
        Transition::Instant,
    );
    assert!(r.v.animating());
    r.v.set_paused(true);
    assert!(!r.v.animating(), "unfocused: no frames");
    let t = r.v.time();
    std::thread::sleep(std::time::Duration::from_millis(30));
    assert_eq!(r.v.time(), t, "clock stopped");
    r.v.set_paused(false);
    assert!(r.v.time() - t < 0.02, "no jump on resume");
    r.v.set_reduced_motion(true);
    assert!(!r.v.animating());
    // Transitions are instant under reduced motion.
    r.v.set_opacity("a", 0.25, Transition::tween(5.0));
    assert!(!r.v.animating());
}

mod heavens {
    use super::*;

    fn sky(style: f32) -> LayerSpec {
        LayerSpec {
            params: vec![("style".into(), style)],
            ..LayerSpec::new("sky", LayerKind::Shader { name: "heavens".into(), source: None })
        }
    }

    fn frame(r: &mut Rig, t: f64) -> Image {
        r.v.set_fixed_time(Some(t));
        let mut term = Terminal::new(48, 16);
        term.advance(b"seraphim\x1b[?25l");
        let (width, height) = r.h.size_for(&term, 4.0);
        let view = View { width, height, padding: 4.0, focused: true, cursor_on: true };
        r.v.prepare(width, height);
        let v = std::cell::RefCell::new(&mut r.v);
        r.h.render_with_encoder(&term, view, &mut |e| v.borrow_mut().encode(e), &mut |pass| {
            v.borrow_mut().render(pass)
        })
    }

    fn mean(img: &Image) -> [f32; 3] {
        let mut sum = [0.0f32; 3];
        for p in img.rgba.chunks(4) {
            for i in 0..3 {
                sum[i] += p[i] as f32;
            }
        }
        sum.map(|s| s / (img.rgba.len() / 4) as f32)
    }

    #[test]
    fn styles_paint_animate_and_keep_text_on_top() {
        let Some(mut r) = rig() else { return };
        for style in [1.0, 2.0, 3.0] {
            assert!(r.v.set_layers(&[sky(style)], Transition::Instant).is_empty());
            let a = frame(&mut r, 10.0);
            let b = frame(&mut r, 14.0);
            let changed = a.rgba.iter().zip(&b.rgba).filter(|(x, y)| x != y).count();
            assert!(changed > a.rgba.len() / 50, "style {style} animates ({changed} bytes)");
            assert!(r.v.animating());
            // Opaque: every pixel is painted, and it is not flat.
            let first = rgb(a.pixel(0, 0));
            let varied = a.rgba.chunks(4).filter(|p| rgb([p[0], p[1], p[2], 255]) != first);
            assert!(varied.count() > a.rgba.len() / 8, "style {style} paints a scene");
            // Text composites on top: the first glyph cell holds bright ink.
            let m = r.h.renderer.cell_metrics();
            let bright = (4..4 + m.height as u32)
                .flat_map(|y| (4..4 + m.width as u32).map(move |x| (x, y)))
                .filter(|&(x, y)| a.pixel(x, y)[0] > 200)
                .count();
            assert!(bright > 5, "style {style}: text stays visible");
        }
    }

    #[test]
    fn alert_mood_warms_the_scene() {
        let Some(mut r) = rig() else { return };
        r.v.set_layers(&[sky(1.0)], Transition::Instant);
        let calm = mean(&frame(&mut r, 20.0));
        r.v.set_param("sky", "alert", 1.0, Transition::tween(0.9));
        let _ = frame(&mut r, 20.5); // mid-fade
        let root = mean(&frame(&mut r, 22.0));
        assert!(root[0] > calm[0] + 3.0, "red rises: {calm:?} -> {root:?}");
        assert!(root[0] - root[2] > calm[0] - calm[2], "scene shifts warm");
    }

    #[test]
    fn install_wave_runs_once() {
        let Some(mut r) = rig() else { return };
        r.v.set_layers(&[sky(1.0)], Transition::Instant);
        let before = frame(&mut r, 5.0);
        // What the preset's rule does: jump to 0, sweep to 1 over 2.6 s.
        r.v.set_param("sky", "install", 0.0, Transition::Instant);
        r.v.set_param(
            "sky",
            "install",
            1.0,
            Transition::Tween { duration: 2.6, easing: Easing::Linear },
        );
        let during = mean(&frame(&mut r, 5.0 + 1.3));
        let after = mean(&frame(&mut r, 5.0 + 3.0));
        let b = mean(&before);
        let lift = |m: &[f32; 3]| m.iter().sum::<f32>() - b.iter().sum::<f32>();
        assert!(lift(&during) > 6.0, "wave lights up: {b:?} -> {during:?}");
        assert!(during[0] - b[0] > during[2] - b[2], "the surge color is warm");
        // Once the wave has passed, the scene matches one that never had it.
        r.v.set_layers(&[sky(1.0)], Transition::Instant);
        let calm = mean(&frame(&mut r, 5.0 + 3.0));
        assert!(after.iter().zip(&calm).all(|(x, y)| (x - y).abs() < 0.5), "{after:?} vs {calm:?}");
        assert!(lift(&after) < lift(&during) / 4.0);
    }

    #[test]
    fn palette_overrides_and_unknown_colors() {
        let Some(mut r) = rig() else { return };
        let red_base = LayerSpec {
            params: vec![("intensity".into(), 0.0)],
            colors: vec![("base".into(), [1.0, 0.0, 0.0, 1.0])],
            ..sky(1.0)
        };
        assert!(r.v.set_layers(&[red_base], Transition::Instant).is_empty());
        let c = rgb(frame(&mut r, 1.0).pixel(200, 200));
        assert!(c[0] > 150 && c[1] < 20 && c[2] < 20, "base color drives the sky: {c:?}");
        let bad = LayerSpec { colors: vec![("bse".into(), [0.0; 4])], ..sky(1.0) };
        let errs = r.v.set_layers(&[bad], Transition::Instant);
        assert!(errs[0].contains("no color \"bse\"") && errs[0].contains("accent0"), "{errs:?}");
    }

    #[test]
    fn custom_shader_colors() {
        let Some(mut r) = rig() else { return };
        let src = "fn shade(uv: vec2<f32>, px: vec2<f32>) -> vec4<f32> { return color(C_tint); }";
        let spec = LayerSpec {
            colors: vec![("tint".into(), [0.0, 1.0, 0.0, 1.0])],
            ..LayerSpec::new(
                "c",
                LayerKind::Shader { name: "mine".into(), source: Some(src.into()) },
            )
        };
        assert!(r.v.set_layers(&[spec], Transition::Instant).is_empty());
        assert_eq!(rgb(frame(&mut r, 0.0).pixel(1, 1)), [0, 255, 0]);
    }
}

mod pictures {
    use super::*;

    fn gif(path: &std::path::Path) {
        let file = std::fs::File::create(path).unwrap();
        let mut enc = image::codecs::gif::GifEncoder::new(file);
        let frame = |c: [u8; 4], ms| {
            image::Frame::from_parts(
                image::RgbaImage::from_pixel(8, 8, image::Rgba(c)),
                0,
                0,
                image::Delay::from_numer_denom_ms(ms, 1),
            )
        };
        enc.encode_frames([frame([255, 0, 0, 255], 500), frame([0, 0, 255, 255], 500)]).unwrap();
    }

    #[test]
    fn animated_gif_plays_by_the_clock() {
        let Some(mut r) = rig() else { return };
        let dir = std::env::temp_dir().join(format!("seraphim-vis-gif-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.gif");
        gif(&path);
        let spec = LayerSpec::new("g", LayerKind::Image { path, fit: Fit::Stretch, blur: 0.0 });
        assert!(r.v.set_layers(&[spec], Transition::Instant).is_empty());
        assert!(r.v.animating(), "an animation needs frames");
        r.v.set_fixed_time(Some(0.2));
        assert_eq!(rgb(r.frame("").pixel(1, 1)), [255, 0, 0]);
        r.v.set_fixed_time(Some(0.7));
        assert_eq!(rgb(r.frame("").pixel(1, 1)), [0, 0, 255]);
        r.v.set_fixed_time(Some(1.2));
        assert_eq!(rgb(r.frame("").pixel(1, 1)), [255, 0, 0], "loops");
    }

    #[test]
    fn grading_and_blur() {
        let Some(mut r) = rig() else { return };
        let dir = std::env::temp_dir().join(format!("seraphim-vis-grade-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("g.png");
        image::RgbaImage::from_pixel(4, 4, image::Rgba([200, 100, 50, 255])).save(&path).unwrap();
        let grey = LayerSpec {
            params: vec![("saturation".into(), 0.0)],
            ..LayerSpec::new(
                "i",
                LayerKind::Image { path: path.clone(), fit: Fit::Stretch, blur: 0.0 },
            )
        };
        assert!(r.v.set_layers(&[grey], Transition::Instant).is_empty());
        let p = rgb(r.frame("").pixel(1, 1));
        assert!(p[0] == p[1] && p[1] == p[2], "desaturated: {p:?}");
        r.v.set_param("i", "brightness", 0.5, Transition::Instant);
        let q = rgb(r.frame("").pixel(1, 1));
        assert!(q[0] < p[0] / 2 + 10, "darker: {p:?} -> {q:?}");
        let bad = LayerSpec {
            params: vec![("hue".into(), 1.0)],
            ..LayerSpec::new(
                "j",
                LayerKind::Image { path: path.clone(), fit: Fit::Stretch, blur: 0.0 },
            )
        };
        let e = r.v.set_layers(&[bad], Transition::Instant);
        assert!(e[0].contains("saturation"), "{e:?}");
        // Blur: a hard edge becomes a ramp.
        let edge = dir.join("e.png");
        image::RgbaImage::from_fn(64, 8, |x, _| {
            image::Rgba(if x < 32 { [255, 255, 255, 255] } else { [0, 0, 0, 255] })
        })
        .save(&edge)
        .unwrap();
        let spec =
            LayerSpec::new("b", LayerKind::Image { path: edge, fit: Fit::Stretch, blur: 8.0 });
        assert!(r.v.set_layers(&[spec], Transition::Instant).is_empty());
        let img = r.frame("");
        let mid = img.width / 2;
        let row: Vec<u8> = (mid - 6..mid + 6).map(|x| img.pixel(x, 1)[0]).collect();
        let distinct = row.windows(2).filter(|w| w[0] != w[1]).count();
        assert!(distinct >= 5, "soft edge {row:?}");
    }
}

#[test]
fn frame_pacing_and_animated_gradients() {
    let Some(mut r) = rig() else { return };
    let still = LayerSpec::new(
        "g",
        LayerKind::Gradient {
            colors: vec![[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]],
            angle: 0.0,
        },
    );
    assert!(r.v.set_layers(std::slice::from_ref(&still), Transition::Instant).is_empty());
    assert_eq!(r.v.next_frame_in(), None, "a plain gradient is still");
    let a = r.frame("");
    // Spinning 90 degrees per second: after a second it runs top to bottom.
    r.v.set_param("g", "spin", 90.0, Transition::Instant);
    assert_eq!(r.v.next_frame_in(), Some(0.0), "spinning: continuous");
    r.v.set_fixed_time(Some(1.0));
    let b = r.frame("");
    let (w, h) = (a.width, a.height);
    assert!(
        rgb(a.pixel(1, h / 2))[0] > 200 && rgb(a.pixel(w - 2, h / 2))[2] > 200,
        "left to right at t=0"
    );
    assert!(
        rgb(b.pixel(w / 2, 1))[0] > 150 && rgb(b.pixel(w / 2, h - 2))[2] > 150,
        "top to bottom at t=1"
    );
    // Flow drifts the bands.
    let flowing = LayerSpec { params: vec![("flow".into(), 0.5)], ..still };
    r.v.set_layers(&[flowing], Transition::Instant);
    r.v.set_fixed_time(Some(0.0));
    let c = r.frame("");
    r.v.set_fixed_time(Some(0.5));
    let d = r.frame("");
    assert_ne!(c.pixel(w / 2, h / 2), d.pixel(w / 2, h / 2), "bands moved");
    // Animated image: the time to its next frame, not "continuous".
    let dir = std::env::temp_dir().join(format!("seraphim-pace-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.gif");
    {
        let file = std::fs::File::create(&path).unwrap();
        let mut enc = image::codecs::gif::GifEncoder::new(file);
        let f = |c: [u8; 4]| {
            image::Frame::from_parts(
                image::RgbaImage::from_pixel(2, 2, image::Rgba(c)),
                0,
                0,
                image::Delay::from_numer_denom_ms(250, 1),
            )
        };
        enc.encode_frames([f([255, 0, 0, 255]), f([0, 255, 0, 255])]).unwrap();
    }
    r.v.set_layers(
        &[LayerSpec::new("i", LayerKind::Image { path, fit: Fit::Stretch, blur: 0.0 })],
        Transition::Instant,
    );
    r.v.set_fixed_time(Some(0.1));
    let left = r.v.next_frame_in().unwrap();
    assert!((left - 0.15).abs() < 0.01, "{left}");
}

mod compositing {
    use super::*;

    fn halves() -> LayerSpec {
        // A hard edge: left white, right black.
        let dir = std::env::temp_dir().join(format!("seraphim-halves-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("h.png");
        image::RgbaImage::from_fn(64, 4, |x, _| {
            image::Rgba(if x < 32 { [255; 4] } else { [0, 0, 0, 255] })
        })
        .save(&path)
        .unwrap();
        LayerSpec::new("g", LayerKind::Image { path, fit: Fit::Stretch, blur: 0.0 })
    }

    fn edge_softness(img: &Image) -> usize {
        let y = img.height / 2;
        (0..img.width).filter(|&x| (20..235).contains(&img.pixel(x, y)[0])).count()
    }

    #[test]
    fn blur_layer_blurs_what_is_beneath() {
        let Some(mut r) = rig() else { return };
        r.v.set_layers(&[halves()], Transition::Instant);
        let sharp = edge_softness(&r.frame(""));
        let blur = LayerSpec {
            params: vec![("radius".into(), 12.0)],
            ..LayerSpec::new("b", LayerKind::Blur)
        };
        assert!(r.v.set_layers(&[halves(), blur], Transition::Instant).is_empty());
        let soft = r.frame("");
        assert!(edge_softness(&soft) > sharp + 8, "softened: {sharp} -> {}", edge_softness(&soft));
        // Half opacity: between the two.
        r.v.set_opacity("b", 0.5, Transition::Instant);
        let half = edge_softness(&r.frame(""));
        assert!(half > sharp && half <= edge_softness(&soft) + 2, "{sharp} {half}");
        // A layer above the blur stays sharp.
        let top = LayerSpec::new("t", LayerKind::Solid([1.0, 0.0, 0.0, 1.0]));
        let blur = LayerSpec {
            params: vec![("radius".into(), 12.0)],
            ..LayerSpec::new("b", LayerKind::Blur)
        };
        r.v.set_layers(&[halves(), blur, top], Transition::Instant);
        assert_eq!(rgb(r.frame("").pixel(1, 1)), [255, 0, 0]);
        let e = r.v.set_layers(
            &[LayerSpec {
                params: vec![("sigma".into(), 1.0)],
                ..LayerSpec::new("x", LayerKind::Blur)
            }],
            Transition::Instant,
        );
        assert!(e[0].contains("radius"), "{e:?}");
    }

    #[test]
    fn styled_transitions() {
        let Some(mut r) = rig() else { return };
        let red = LayerSpec::new("r", LayerKind::Solid([1.0, 0.0, 0.0, 1.0]));
        let blue = LayerSpec::new("b", LayerKind::Solid([0.0, 0.0, 1.0, 1.0]));
        assert!(r.v.set_transition_style(TransitionStyle::Builtin("slide".into())).is_ok());
        r.v.set_layers(std::slice::from_ref(&red), Transition::Instant);
        r.v.set_fixed_time(Some(10.0));
        r.v.set_layers(
            std::slice::from_ref(&blue),
            Transition::Tween { duration: 1.0, easing: Easing::Linear },
        );
        assert!(r.v.animating());
        r.v.set_fixed_time(Some(10.5));
        let img = r.frame("");
        let w = img.width;
        assert_eq!(rgb(img.pixel(2, 2)), [255, 0, 0], "old scene on the left");
        assert_eq!(rgb(img.pixel(w - 3, 2)), [0, 0, 255], "new one slides in from the right");
        r.v.set_fixed_time(Some(11.5));
        assert_eq!(rgb(r.frame("").pixel(2, 2)), [0, 0, 255], "done");
        assert!(!r.v.animating(), "outgoing scene dropped");

        // A user transition: a plain mix.
        let mix = "fn transition(uv: vec2<f32>, p: f32) -> vec4<f32> { return mix(from_scene(uv), to_scene(uv), p); }";
        r.v.set_transition_style(TransitionStyle::Custom {
            name: "mix".into(),
            source: mix.into(),
        })
        .unwrap();
        r.v.set_layers(&[red], Transition::Tween { duration: 1.0, easing: Easing::Linear });
        r.v.set_fixed_time(Some(12.0));
        let c = rgb(r.frame("").pixel(2, 2));
        assert!(c[0] > 100 && c[2] > 100, "half-way mix {c:?}");
        // A broken one is refused and the style stays.
        let e = r.v.set_transition_style(TransitionStyle::Custom {
            name: "bad".into(),
            source: "fn transition(".into(),
        });
        assert!(e.unwrap_err().contains("did not compile"));
        // Every built-in compiles.
        for (name, _) in BUILTIN_TRANSITIONS {
            r.v.set_transition_style(TransitionStyle::Builtin(name.to_string()))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
}

#[test]
fn video_layer_plays_and_loops() {
    let Some(mut r) = rig() else { return };
    if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let dir = std::env::temp_dir().join(format!("seraphim-video-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v.mp4");
    // 0.5 s red, then 0.5 s blue, 10 fps.
    let ok = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=red:s=32x16:d=0.5:r=10",
            "-f",
            "lavfi",
            "-i",
            "color=blue:s=32x16:d=0.5:r=10",
        ])
        .args(["-filter_complex", "[0][1]concat=n=2:v=1", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status()
        .unwrap()
        .success();
    assert!(ok, "ffmpeg made the test clip");
    r.v.set_fixed_time(Some(0.0));
    let spec = LayerSpec::new("v", LayerKind::Video { path: path.clone(), fit: Fit::Stretch });
    assert!(r.v.set_layers(&[spec], Transition::Instant).is_empty());
    let red = |c: [i32; 3]| c[0] > 200 && c[2] < 60;
    let blue = |c: [i32; 3]| c[2] > 200 && c[0] < 60;
    assert!(red(rgb(r.frame("").pixel(1, 1))), "first frame");
    assert!(r.v.next_frame_in().is_some_and(|d| d > 0.0 && d <= 0.1), "paced by its frame rate");
    r.v.set_fixed_time(Some(0.75));
    assert!(blue(rgb(r.frame("").pixel(1, 1))), "second half");
    r.v.set_fixed_time(Some(1.25));
    assert!(red(rgb(r.frame("").pixel(1, 1))), "loops");
    let e = r.v.set_layers(
        &[LayerSpec::new("x", LayerKind::Video { path: dir.join("missing.mp4"), fit: Fit::Cover })],
        Transition::Instant,
    );
    assert_eq!(e.len(), 1, "{e:?}");
    std::fs::write(dir.join("junk.mp4"), b"nope").unwrap();
    let e = r.v.set_layers(
        &[LayerSpec::new("y", LayerKind::Video { path: dir.join("junk.mp4"), fit: Fit::Cover })],
        Transition::Instant,
    );
    assert_eq!(e.len(), 1, "{e:?}");
}

#[test]
fn colors_animate_and_single_layers_come_and_go() {
    let Some(mut r) = rig() else { return };
    let g = LayerSpec::new(
        "g",
        LayerKind::Gradient { colors: vec![[0.0, 0.0, 1.0, 1.0]; 2], angle: 0.0 },
    );
    assert!(r
        .v
        .set_layers(&[solid("bg", [1.0, 0.0, 0.0, 1.0]), g], Transition::Instant)
        .is_empty());
    assert_eq!(r.v.color_names("bg"), ["color"]);
    assert_eq!(r.v.color_names("g"), ["0", "1"]);
    assert!(!r.v.set_color("bg", "nope", [0.0; 4], Transition::Instant));
    assert!(r.v.set_color("g", "0", [0.0, 1.0, 0.0, 1.0], Transition::Instant));
    assert!(r.v.set_color("g", "1", [0.0, 1.0, 0.0, 1.0], Transition::Instant));
    assert!(near(rgb(r.frame("").pixel(1, 1)), [0, 255, 0], 2), "gradient now green");
    // A tween is part-way at the half.
    r.v.set_fixed_time(Some(0.0));
    assert!(r.v.set_color(
        "g",
        "0",
        [0.0, 0.0, 0.0, 1.0],
        Transition::Tween { duration: 1.0, easing: Easing::Linear }
    ));
    assert!(r.v.set_color(
        "g",
        "1",
        [0.0, 0.0, 0.0, 1.0],
        Transition::Tween { duration: 1.0, easing: Easing::Linear }
    ));
    assert!(r.v.animating());
    r.v.set_fixed_time(Some(0.5));
    let mid = rgb(r.frame("").pixel(1, 1));
    assert!(near(mid, [0, 128, 0], 40), "{mid:?}");
    r.v.set_fixed_time(Some(2.0));
    // Add a layer on top, replace it in place, remove it.
    r.v.upsert_layer(&solid("top", [1.0, 1.0, 1.0, 1.0]), Transition::Instant).unwrap();
    assert_eq!(r.v.layer_ids(), ["bg", "g", "top"]);
    assert_eq!(rgb(r.frame("").pixel(1, 1)), [255, 255, 255]);
    r.v.upsert_layer(&solid("top", [1.0, 1.0, 0.0, 1.0]), Transition::Instant).unwrap();
    assert_eq!(r.v.layer_ids(), ["bg", "g", "top"]);
    assert_eq!(rgb(r.frame("").pixel(1, 1)), [255, 255, 0]);
    assert!(r
        .v
        .upsert_layer(
            &LayerSpec::new("bad", LayerKind::Shader { name: "nope".into(), source: None }),
            Transition::Instant
        )
        .is_err());
    assert!(r.v.remove_layer("top", Transition::Instant));
    assert!(!r.v.remove_layer("top", Transition::Instant));
    assert_eq!(r.v.layer_ids(), ["bg", "g"]);
}
