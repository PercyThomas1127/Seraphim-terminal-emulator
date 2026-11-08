// The old scene zooms toward the viewer and fades; the new one settles in.
fn transition(uv: vec2<f32>, p: f32) -> vec4<f32> {
    let c = vec2<f32>(0.5);
    let before = from_scene(c + (uv - c) / (1.0 + p * 0.6));
    let after = to_scene(c + (uv - c) * (1.0 + (1.0 - p) * 0.15));
    return mix(before, after, smoothstep(0.0, 1.0, p));
}
