// Aurora: slow, flowing curtains of light.
// Parameters: speed, intensity, hue (0..1), spread.
fn shade(uv: vec2<f32>, px: vec2<f32>) -> vec4<f32> {
    let t = u.time * p(P_speed);
    let aspect = u.resolution.x / max(u.resolution.y, 1.0);
    var q = vec2<f32>(uv.x * aspect, uv.y);
    var light = 0.0;
    for (var i = 0; i < 3; i = i + 1) {
        let fi = f32(i);
        let wave = fbm(vec2<f32>(q.x * 1.3 + fi * 3.1, t * 0.15 + fi)) - 0.5;
        let center = 0.35 + 0.12 * fi + wave * 0.35 * p(P_spread);
        let band = exp(-pow((q.y - center) * (7.0 - fi), 2.0));
        let shimmer = 0.6 + 0.4 * fbm(vec2<f32>(q.x * 8.0 + t * 0.6, fi * 7.0));
        light = light + band * shimmer * (0.55 - 0.12 * fi);
    }
    let h = fract(p(P_hue) + 0.08 * sin(q.x * 2.0 + t * 0.2) - 0.1 * uv.y);
    let rgb = hsv(h, 0.65, 1.0);
    return vec4<f32>(rgb, clamp(light * p(P_intensity), 0.0, 1.0));
}
