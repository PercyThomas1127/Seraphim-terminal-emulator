// Starfield: twinkling, slowly drifting stars.
// Parameters: density (0..1), speed, twinkle, size.
fn shade(uv: vec2<f32>, px: vec2<f32>) -> vec4<f32> {
    var a = 0.0;
    var tint = vec3<f32>(0.0);
    for (var layer = 0; layer < 3; layer = layer + 1) {
        let fl = f32(layer);
        let cell = mix(42.0, 14.0, fl / 2.0) * max(p(P_size), 0.1);
        let drift = vec2<f32>(u.time * p(P_speed) * (2.0 + fl * 3.0), 0.0);
        let g = (px + drift) / cell;
        let id = floor(g);
        let h = hash21(id + fl * 17.0);
        if (h > 1.0 - 0.25 * p(P_density)) {
            let pos = vec2<f32>(hash21(id + 3.1), hash21(id + 7.7));
            let d = length(fract(g) - pos);
            let tw = 0.65 + 0.35 * sin(u.time * p(P_twinkle) * (1.0 + 3.0 * h) + h * 50.0);
            let s = smoothstep(0.09, 0.0, d) * tw * (0.5 + 0.25 * fl);
            a = max(a, s);
            tint = mix(vec3<f32>(0.8, 0.85, 1.0), vec3<f32>(1.0, 0.8, 0.9), hash21(id + 1.3));
        }
    }
    return vec4<f32>(tint, a);
}
