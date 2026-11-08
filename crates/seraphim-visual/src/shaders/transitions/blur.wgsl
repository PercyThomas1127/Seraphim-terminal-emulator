// Blur out the old scene, blur in the new one.
fn blurred(uv: vec2<f32>, r: f32, outgoing: bool) -> vec4<f32> {
    var sum = vec4<f32>(0.0);
    let px = r / u.resolution;
    for (var i = -3; i <= 3; i = i + 1) {
        for (var j = -3; j <= 3; j = j + 1) {
            let o = uv + vec2<f32>(f32(i), f32(j)) * px / 3.0;
            if (outgoing) {
                sum = sum + from_scene(o);
            } else {
                sum = sum + to_scene(o);
            }
        }
    }
    return sum / 49.0;
}

fn transition(uv: vec2<f32>, p: f32) -> vec4<f32> {
    let r = sin(p * 3.14159265) * 24.0;
    return mix(blurred(uv, r, true), blurred(uv, r, false), smoothstep(0.3, 0.7, p));
}
