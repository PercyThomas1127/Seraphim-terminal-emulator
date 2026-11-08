// Seraphim layer prelude. A layer implements
//     fn shade(uv: vec2<f32>, px: vec2<f32>) -> vec4<f32>
// returning straight (non-premultiplied) RGBA in sRGB space. `uv` is 0..1
// across the window (origin top-left), `px` the pixel position.
// Available: u.time (seconds), u.resolution (pixels), p(i) parameters
// (named constants P_<name> are generated from the configuration).

struct Layer {
    resolution: vec2<f32>,
    time: f32,
    opacity: f32,
    params: array<vec4<f32>, 4>,
    colors: array<vec4<f32>, 8>,
    stops: array<vec4<f32>, 2>,
    // x: color count, y: angle (radians), z: fit mode, w: unused
    info: vec4<f32>,
    // xy: image size in pixels
    image: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Layer;
@group(0) @binding(1) var layer_texture: texture_2d<f32>;
@group(0) @binding(2) var layer_sampler: sampler;

fn p(i: u32) -> f32 {
    return u.params[i / 4u][i % 4u];
}

fn stop(i: u32) -> f32 {
    return u.stops[i / 4u][i % 4u];
}

// HSV to RGB, all components 0..1.
fn hsv(h: f32, s: f32, v: f32) -> vec3<f32> {
    let k = vec3<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0);
    let q = abs(fract(vec3<f32>(h) + k) * 6.0 - vec3<f32>(3.0));
    return v * mix(vec3<f32>(1.0), clamp(q - vec3<f32>(1.0), vec3<f32>(0.0), vec3<f32>(1.0)), s);
}

fn hash21(q: vec2<f32>) -> f32 {
    var x = fract(q * vec2<f32>(123.34, 456.21));
    x = x + dot(x, x + 45.32);
    return fract(x.x * x.y);
}

fn noise(q: vec2<f32>) -> f32 {
    let i = floor(q);
    let f = fract(q);
    let s = f * f * (3.0 - 2.0 * f);
    let a = hash21(i);
    let b = hash21(i + vec2<f32>(1.0, 0.0));
    let c = hash21(i + vec2<f32>(0.0, 1.0));
    let d = hash21(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, s.x), mix(c, d, s.x), s.y);
}

fn fbm(q0: vec2<f32>) -> f32 {
    var q = q0;
    var v = 0.0;
    var amp = 0.5;
    for (var i = 0; i < 5; i = i + 1) {
        v = v + amp * noise(q);
        q = q * 2.03 + vec2<f32>(1.7, 9.2);
        amp = amp * 0.5;
    }
    return v;
}

struct VsOut {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // One triangle covering the whole target.
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    var out: VsOut;
    out.position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let px = in.position.xy;
    let uv = px / max(u.resolution, vec2<f32>(1.0));
    let c = shade(uv, px);
    let a = clamp(c.a, 0.0, 1.0) * u.opacity;
    return vec4<f32>(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0)) * a, a);
}
