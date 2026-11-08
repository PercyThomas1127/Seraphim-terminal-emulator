// Seraphim animated background: one full-screen triangle, drawn before the
// text pass so text always composites on top and stays crisp.
//
// Colors are sRGB values written to a non-sRGB target, matching the cell
// shader. Everything is a pure function of the uniforms, so a frame can be
// reproduced exactly from (style, time, mood).

struct Bg {
    frame: vec4<f32>,    // width px, height px, wall time s, intensity 0..1
    params: vec4<f32>,   // style, animation time, privileged 0..1, install phase 0..1 (<0 off)
    base: vec4<f32>,     // terminal background color
    accent0: vec4<f32>,
    accent1: vec4<f32>,
    accent2: vec4<f32>,
    alert0: vec4<f32>,   // privileged palette
    alert1: vec4<f32>,
    surge: vec4<f32>,    // install color
};

@group(0) @binding(0) var<uniform> bg: Bg;

const STYLE_AURORA: f32 = 1.0;
const STYLE_NEBULA: f32 = 2.0;
const STYLE_STARFIELD: f32 = 3.0;
const PI: f32 = 3.14159265;

@vertex
fn vs_bg(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // (0,0) (2,0) (0,2): one triangle covering the screen.
    let p = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

// ---------------------------------------------------------------- noise ----

fn hash21(p: vec2<f32>) -> f32 {
    var q = fract(p * vec2<f32>(123.34, 456.21));
    q += dot(q, q + 45.32);
    return fract(q.x * q.y);
}

fn hash22(p: vec2<f32>) -> vec2<f32> {
    let n = hash21(p);
    return vec2<f32>(n, hash21(p + n + 17.0));
}

fn noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash21(i);
    let b = hash21(i + vec2<f32>(1.0, 0.0));
    let c = hash21(i + vec2<f32>(0.0, 1.0));
    let d = hash21(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

fn fbm(p: vec2<f32>) -> f32 {
    let rot = mat2x2<f32>(0.8, 0.6, -0.6, 0.8);
    var v = 0.0;
    var a = 0.5;
    var q = p;
    for (var i = 0; i < 4; i++) {
        v += a * noise(q);
        q = rot * q * 2.03 + vec2<f32>(7.1, 3.7);
        a *= 0.5;
    }
    return v;
}

// ---------------------------------------------------------------- layers ---

// Twinkling points on a jittered grid. `p` is in screen-height units.
fn stars(p: vec2<f32>, scale: f32, density: f32, t: f32) -> f32 {
    let g = p * scale;
    let id = floor(g);
    let f = fract(g) - 0.5;
    let h = hash21(id);
    let off = (hash22(id + 3.1) - 0.5) * 0.6;
    let d = length(f - off);
    let size = mix(0.04, 0.10, hash21(id + 5.3));
    let twinkle = 0.55 + 0.45 * sin(t * (0.8 + 2.5 * hash21(id + 9.7)) + h * 6.283);
    let core = smoothstep(size, 0.0, d);
    let halo = smoothstep(size * 3.5, 0.0, d) * 0.25;
    return step(1.0 - density, h) * (core + halo) * twinkle;
}

fn starfield(p: vec2<f32>, t: f32, drift: f32) -> f32 {
    var s = 0.0;
    for (var i = 0; i < 3; i++) {
        let fi = f32(i);
        let move_by = vec2<f32>(drift * (0.004 + 0.006 * fi), drift * 0.001 * fi);
        s += stars(p + move_by + fi * 13.7, 28.0 + 22.0 * fi, 0.10 - 0.025 * fi, t) * (1.0 - 0.28 * fi);
    }
    return s;
}

// Curtains of light: a bright lower edge with a long fade upward, folded by
// low-frequency noise and streaked by vertical rays.
fn aurora(p: vec2<f32>, t: f32, c0: vec3<f32>, c1: vec3<f32>, c2: vec3<f32>) -> vec3<f32> {
    var col = vec3<f32>(0.0);
    for (var i = 0; i < 3; i++) {
        let fi = f32(i);
        let x = p.x;
        let fold = fbm(vec2<f32>(x * 0.9 + t * 0.020, fi * 3.7)) - 0.5;
        let center = 0.26 + 0.13 * fi
            + 0.06 * sin(x * 1.15 + t * 0.07 * (1.0 + 0.3 * fi) + fi * 2.1)
            + 0.14 * fold;
        let d = p.y - center; // screen y grows downward: d > 0 is below the edge
        let below = max(d, 0.0);
        let shape = select(exp(d * 6.0), exp(-below * below * 700.0), d > 0.0);
        let rays = 0.50 + 0.50 * noise(vec2<f32>(x * 16.0 + fi * 11.0 + fold * 6.0, t * 0.22 + fi));
        let fine = 0.65 + 0.35 * noise(vec2<f32>(x * 52.0 - t * 0.08, fi * 5.0));
        // Each curtain has its own edge color, fading upward into another,
        // and the pair drifts along x. Never average all three: that
        // collapses to gray.
        var edge_c = c2;
        var top_c = c0;
        if i == 1 {
            edge_c = c1;
            top_c = c2;
        } else if i == 2 {
            edge_c = c0;
            top_c = c1;
        }
        let drift = 0.5 + 0.5 * sin(x * 0.7 + t * 0.04 + fi * 2.3);
        edge_c = mix(edge_c, top_c, drift * 0.35);
        let height = clamp(-d * 5.0, 0.0, 1.0);
        let c = mix(edge_c, top_c, height);
        col += c * shape * rays * fine * (0.80 - 0.18 * fi);
    }
    return col;
}

// Slow domain-warped clouds.
fn nebula(p: vec2<f32>, t: f32, c0: vec3<f32>, c1: vec3<f32>, c2: vec3<f32>) -> vec3<f32> {
    let q = vec2<f32>(
        fbm(p * 1.5 + vec2<f32>(0.0, t * 0.012)),
        fbm(p * 1.5 + vec2<f32>(5.2, 1.3) - t * 0.010),
    );
    let r = fbm(p * 1.3 + 2.4 * q + vec2<f32>(t * 0.008, 0.0));
    var col = mix(c1, c0, smoothstep(0.45, 0.75, r));
    col = mix(col, c2, smoothstep(0.55, 0.85, q.x) * 0.8);
    let density = smoothstep(0.32, 0.85, r);
    // Bright filaments where the warp folds.
    let glow = pow(smoothstep(0.62, 0.9, r), 2.0) * 0.6;
    return col * (density * 0.8 + glow);
}

// ------------------------------------------------------------------ main ---

@fragment
fn fs_bg(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let res = bg.frame.xy;
    let wall = bg.frame.z;
    let intensity = bg.frame.w;
    let style = bg.params.x;
    let t = bg.params.y;
    let alert = bg.params.z;
    let phase = bg.params.w;

    let uv = pos.xy / res;            // 0..1, y down
    let aspect = res.x / res.y;
    let p = vec2<f32>(uv.x * aspect, uv.y);

    // Privileged sessions pull every accent toward the alert palette.
    let c0 = mix(bg.accent0.rgb, bg.alert0.rgb, alert);
    let c1 = mix(bg.accent1.rgb, bg.alert1.rgb, alert);
    let c2 = mix(bg.accent2.rgb, bg.alert0.rgb, alert);

    // Base: the terminal background, lifted slightly toward the top.
    let base = bg.base.rgb;
    var col = base + (base * 0.35 + vec3<f32>(0.010)) * (1.0 - uv.y) * (1.0 - uv.y);

    var fx = vec3<f32>(0.0);
    var star_amount = 0.0;
    if style == STYLE_AURORA {
        fx += nebula(p * 0.8 + 3.0, t * 0.5, c1, c2, c0) * 0.18;
        let a = aurora(p, t, c0, c1, c2);
        fx += a;
        // Stars fade where the curtains are bright.
        star_amount = starfield(p, wall, t) * (1.0 - clamp(dot(a, vec3<f32>(0.6)), 0.0, 1.0));
    } else if style == STYLE_NEBULA {
        fx += nebula(p, t, c0, c1, c2);
        star_amount = starfield(p, wall, t) * 0.8;
    } else if style == STYLE_STARFIELD {
        star_amount = starfield(p, wall, t * 6.0) * 1.3;
        // A faint band of galactic dust.
        let band = exp(-pow((uv.y - 0.45 - 0.15 * (uv.x - 0.5)) * 3.5, 2.0));
        fx += mix(c1, c0, uv.x) * band * fbm(p * 3.0 + t * 0.01) * 0.45;
    }
    let star_tint = mix(vec3<f32>(1.0, 0.97, 0.98), c2, 0.25);
    fx += star_tint * star_amount * 0.55;

    // Privileged: a slow red breath around the edges.
    let edge = min(min(uv.x, 1.0 - uv.x) * aspect, min(uv.y, 1.0 - uv.y));
    let breath = 0.70 + 0.30 * sin(wall * 2.0);
    fx += bg.alert0.rgb * exp(-edge * 7.0) * alert * breath * 0.55;

    // Install: a wave of light rising from the bottom, trailing sparks.
    if phase >= 0.0 {
        let env = smoothstep(0.0, 0.08, phase) * (1.0 - smoothstep(0.55, 1.0, phase));
        let front = 1.12 - phase * 1.45 + 0.018 * sin(p.x * 9.0 + wall * 3.0);
        let dy = uv.y - front;
        // A thin bright crest inside a soft halo, a faint glow trailing
        // below it, and sparks drifting up through the lit region.
        let crest = exp(-dy * dy * 1800.0) * 0.9;
        let halo = exp(-dy * dy * 40.0) * 0.22;
        let wake = exp(-max(dy, 0.0) * 6.0) * smoothstep(0.0, 0.05, dy) * 0.14;
        let sp = vec2<f32>(p.x, p.y + wall * 0.35);
        let sparks = stars(sp, 34.0, 0.22, wall * 4.0) * smoothstep(-0.02, 0.04, dy) * exp(-max(dy, 0.0) * 2.2);
        fx += bg.surge.rgb * (crest + halo + wake + sparks * 1.4) * env;
    }

    // Vignette keeps the corners calm; intensity scales every effect.
    let vig = 1.0 - 0.35 * dot(uv - 0.5, uv - 0.5);
    col = (col + fx * intensity) * vig;

    // Dither away 8-bit banding in the dark gradients.
    col += (hash21(pos.xy + fract(wall) * 61.0) - 0.5) / 255.0;
    return vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
