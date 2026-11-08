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
        let v = &self.v;
        self.h.render_with(&t, view, &mut |pass| v.render(pass))
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
        &[LayerSpec::new("i", LayerKind::Image { path: path.clone(), fit: Fit::Stretch })],
        Transition::Instant,
    );
    let img = r.frame("");
    let (w, h) = (img.width, img.height);
    assert!(near(rgb(img.pixel(1, 1)), [255, 0, 0], 8), "top-left red");
    assert!(near(rgb(img.pixel(w - 2, h - 2)), [255, 255, 255], 8), "bottom-right white");
    // Missing files are reported, not fatal.
    let errs = r.v.set_layers(
        &[LayerSpec::new("x", LayerKind::Image { path: dir.join("nope.png"), fit: Fit::Cover })],
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
