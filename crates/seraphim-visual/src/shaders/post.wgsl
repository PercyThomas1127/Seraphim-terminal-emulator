// Seraphim post-process prelude: fullscreen passes over scene textures.
// Textures hold premultiplied RGBA; uv is 0..1 with the origin top-left.

struct Post {
    resolution: vec2<f32>,
    time: f32,
    // Transition progress, 0 (old scene) to 1 (new scene).
    progress: f32,
    dir: vec2<f32>,
    radius: f32,
    opacity: f32,
};

@group(0) @binding(0) var<uniform> u: Post;
@group(0) @binding(1) var tex_a: texture_2d<f32>;
@group(0) @binding(2) var tex_b: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;

struct PostOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_post(@builtin(vertex_index) vi: u32) -> PostOut {
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    var out: PostOut;
    out.position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

// The outgoing scene (transitions), premultiplied.
fn from_scene(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(tex_a, samp, uv, 0.0);
}

// The incoming scene, premultiplied.
fn to_scene(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(tex_b, samp, uv, 0.0);
}

fn post_hash(q: vec2<f32>) -> f32 {
    var x = fract(q * vec2<f32>(123.34, 456.21));
    x = x + dot(x, x + 45.32);
    return fract(x.x * x.y);
}
