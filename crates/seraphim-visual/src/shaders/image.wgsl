// Background image. u.info.z: 0 cover, 1 contain, 2 stretch, 3 tile.
fn shade(uv: vec2<f32>, px: vec2<f32>) -> vec4<f32> {
    let img = max(u.image.xy, vec2<f32>(1.0));
    let win = max(u.resolution, vec2<f32>(1.0));
    let mode = u32(u.info.z);
    var st = uv;
    if (mode == 3u) {
        st = fract(px / img);
    } else if (mode != 2u) {
        let sx = win.x / img.x;
        let sy = win.y / img.y;
        var s = max(sx, sy);
        if (mode == 1u) {
            s = min(sx, sy);
        }
        let shown = img * s;
        st = (px - (win - shown) * 0.5) / shown;
        if (any(st < vec2<f32>(0.0)) || any(st > vec2<f32>(1.0))) {
            return vec4<f32>(0.0);
        }
    }
    return textureSampleLevel(layer_texture, layer_sampler, st, 0.0);
}
