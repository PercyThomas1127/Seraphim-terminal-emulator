// Seraphim cell renderer: one instanced quad per background run, glyph,
// decoration or cursor piece. Colors arrive in sRGB space and the target is
// a non-sRGB format, so blending happens in the same space other terminals
// use. Output is premultiplied alpha.

struct Globals {
    screen: vec2<f32>,
    atlas: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var atlas_tex: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;        // nearest: 1:1 text
@group(0) @binding(3) var atlas_sampler_linear: sampler; // scaled emoji

struct Instance {
    @location(0) rect: vec4<f32>,   // x, y, w, h in pixels
    @location(1) uv: vec4<f32>,     // x, y, w, h in atlas texels
    @location(2) color: vec4<f32>,
    @location(3) kind: u32,         // 0 solid, 1 mask glyph, 2 color glyph, 3 glow, 4 ring
    @location(4) params: vec3<f32>, // effect parameters
};

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) kind: u32,
    @location(3) @interpolate(flat) params: vec3<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: Instance) -> VsOut {
    // Triangle strip: (0,0) (1,0) (0,1) (1,1)
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let px = inst.rect.xy + corner * inst.rect.zw;
    var out: VsOut;
    out.position = vec4<f32>(px.x / globals.screen.x * 2.0 - 1.0, 1.0 - px.y / globals.screen.y * 2.0, 0.0, 1.0);
    out.uv = (inst.uv.xy + corner * inst.uv.zw) / globals.atlas;
    out.color = inst.color;
    out.kind = inst.kind;
    out.params = inst.params;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Sample unconditionally (uniform control flow), then select.
    let texel = textureSample(atlas_tex, atlas_sampler, in.uv);
    let smooth_texel = textureSample(atlas_tex, atlas_sampler_linear, in.uv);
    if in.kind == 1u {
        let a = texel.a * in.color.a;
        return vec4<f32>(in.color.rgb * a, a);
    }
    if in.kind == 2u {
        return vec4<f32>(smooth_texel.rgb * smooth_texel.a, smooth_texel.a);
    }
    if in.kind == 3u {
        // Glow: Gaussian falloff outside the inner rectangle. uv is 0..1
        // across the quad (see push_effect).
        let r = in.params.x;
        let size = in.params.yz;
        let p = in.uv * size;
        let half = size * 0.5 - vec2<f32>(r);
        let q = abs(p - size * 0.5) - half;
        let d = length(max(q, vec2<f32>(0.0)));
        let a = in.color.a * exp(-pow(d / (r * 0.5), 2.0));
        return vec4<f32>(in.color.rgb * a, a);
    }
    if in.kind == 4u {
        // Ring of radius params.x and thickness params.y.
        let size = in.params.z;
        let p = in.uv * size - vec2<f32>(size * 0.5);
        let d = abs(length(p) - in.params.x);
        let a = in.color.a * (1.0 - smoothstep(0.0, in.params.y, d));
        return vec4<f32>(in.color.rgb * a, a);
    }
    return vec4<f32>(in.color.rgb * in.color.a, in.color.a);
}
