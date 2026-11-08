// Pixel blocks switch over in random order.
fn transition(uv: vec2<f32>, p: f32) -> vec4<f32> {
    let cell = floor(uv * u.resolution / 4.0);
    let n = post_hash(cell);
    let m = smoothstep(n - 0.05, n + 0.05, p * 1.1 - 0.05);
    return mix(from_scene(uv), to_scene(uv), m);
}
