// Fade out to transparent (the terminal background), then fade in.
fn transition(uv: vec2<f32>, p: f32) -> vec4<f32> {
    if (p < 0.5) {
        return from_scene(uv) * (1.0 - p * 2.0);
    }
    return to_scene(uv) * (p * 2.0 - 1.0);
}
