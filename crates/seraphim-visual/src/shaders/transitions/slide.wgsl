// The new scene slides in from the right, pushing the old one out.
fn transition(uv: vec2<f32>, p: f32) -> vec4<f32> {
    let e = p * p * (3.0 - 2.0 * p);
    if (uv.x >= 1.0 - e) {
        return to_scene(vec2<f32>(uv.x - (1.0 - e), uv.y));
    }
    return from_scene(vec2<f32>(uv.x + e, uv.y));
}
