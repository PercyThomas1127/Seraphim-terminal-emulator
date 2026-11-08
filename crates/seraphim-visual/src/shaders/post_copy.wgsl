// Draw tex_a (premultiplied) scaled by u.opacity.
@fragment
fn fs_post(in: PostOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(tex_a, samp, in.uv, 0.0) * u.opacity;
}
