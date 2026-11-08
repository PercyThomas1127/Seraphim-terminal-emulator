// Inline images: one textured quad per placement. Image data is sRGB,
// sampled as-is into the non-sRGB target like every other color here.
// Output is premultiplied alpha.

struct Globals {
    screen: vec2<f32>,
    atlas: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var image_tex: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;

struct Quad {
    @location(0) rect: vec4<f32>, // x, y, w, h in pixels
    @location(1) uv: vec4<f32>,   // x, y, w, h in 0..1
};

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, q: Quad) -> VsOut {
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let px = q.rect.xy + corner * q.rect.zw;
    var out: VsOut;
    out.position = vec4<f32>(px.x / globals.screen.x * 2.0 - 1.0, 1.0 - px.y / globals.screen.y * 2.0, 0.0, 1.0);
    out.uv = q.uv.xy + corner * q.uv.zw;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(image_tex, image_sampler, in.uv);
    return vec4<f32>(c.rgb * c.a, c.a);
}
