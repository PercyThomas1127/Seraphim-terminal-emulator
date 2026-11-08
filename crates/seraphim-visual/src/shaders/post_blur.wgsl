// One direction of a separable Gaussian (sigma = radius / 2), sampling
// tex_a; the output target may be smaller (downsampled blur).
@fragment
fn fs_post(in: PostOut) -> @location(0) vec4<f32> {
    let step = u.dir / u.resolution;
    let sigma = max(u.radius * 0.5, 0.5);
    var sum = vec4<f32>(0.0);
    var total = 0.0;
    for (var i = -12; i <= 12; i = i + 1) {
        // Taps spread over +-3 sigma.
        let x = f32(i) * sigma * 3.0 / 12.0;
        let w = exp(-0.5 * (x / sigma) * (x / sigma));
        sum = sum + textureSampleLevel(tex_a, samp, in.uv + step * x, 0.0) * w;
        total = total + w;
    }
    return sum / total;
}
