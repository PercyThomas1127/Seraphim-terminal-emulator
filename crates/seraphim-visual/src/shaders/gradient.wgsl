// Linear gradient through up to 8 color stops at angle u.info.y.
fn shade(uv: vec2<f32>, px: vec2<f32>) -> vec4<f32> {
    let n = u32(u.info.x);
    let dir = vec2<f32>(cos(u.info.y), sin(u.info.y));
    // Project onto the direction, normalized so the corners map to 0 and 1.
    let aspect = vec2<f32>(u.resolution.x / max(u.resolution.y, 1.0), 1.0);
    let q = (uv - vec2<f32>(0.5)) * aspect;
    let reach = 0.5 * (abs(dir.x) * aspect.x + abs(dir.y) * aspect.y);
    let t = clamp(dot(q, dir) / max(reach, 0.0001) * 0.5 + 0.5, 0.0, 1.0);
    var c = u.colors[0];
    for (var i = 1u; i < n; i = i + 1u) {
        let a = stop(i - 1u);
        let b = stop(i);
        if (t >= a) {
            c = mix(u.colors[i - 1u], u.colors[i], clamp((t - a) / max(b - a, 0.0001), 0.0, 1.0));
        }
    }
    return c;
}
