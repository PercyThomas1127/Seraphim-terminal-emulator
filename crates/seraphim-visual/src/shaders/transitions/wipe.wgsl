// A soft edge sweeps left to right.
fn transition(uv: vec2<f32>, p: f32) -> vec4<f32> {
    let edge = p * 1.3 - 0.15;
    let m = smoothstep(edge - 0.15, edge + 0.15, uv.x);
    return mix(to_scene(uv), from_scene(uv), m);
}
