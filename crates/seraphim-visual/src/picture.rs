//! Background pictures: PNG, JPEG, WebP and GIF, animated GIF / WebP /
//! APNG frames, and blur (done once at load time, so it costs nothing per
//! frame).

use std::io::BufReader;
use std::path::Path;

use image::{AnimationDecoder, ImageFormat, RgbaImage};

/// Decoded frames are capped at this much memory in total.
pub const MAX_BYTES: usize = 512 << 20;
/// At most this many frames are kept from an animation.
pub const MAX_FRAMES: usize = 1000;

#[derive(Clone, Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// How long this frame shows, in seconds (0 for a still image).
    pub delay: f32,
}

/// Load a picture; animations come back as several frames.
pub fn load(path: &Path, blur: f32) -> Result<Vec<Frame>, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let reader = image::ImageReader::open(path)
        .map_err(|e| err(&e))?
        .with_guessed_format()
        .map_err(|e| err(&e))?;
    let format = reader.format();
    let open = || std::fs::File::open(path).map(BufReader::new).map_err(|e| err(&e));
    let frames = match format {
        Some(ImageFormat::Gif) => {
            let d = image::codecs::gif::GifDecoder::new(open()?).map_err(|e| err(&e))?;
            collect(d.into_frames(), path)?
        }
        Some(ImageFormat::WebP) => {
            let d = image::codecs::webp::WebPDecoder::new(open()?).map_err(|e| err(&e))?;
            if d.has_animation() {
                collect(d.into_frames(), path)?
            } else {
                still(image::DynamicImage::from_decoder(d).map_err(|e| err(&e))?.to_rgba8())
            }
        }
        Some(ImageFormat::Png) => {
            let d = image::codecs::png::PngDecoder::new(open()?).map_err(|e| err(&e))?;
            if d.is_apng().unwrap_or(false) {
                collect(d.apng().map_err(|e| err(&e))?.into_frames(), path)?
            } else {
                still(image::DynamicImage::from_decoder(d).map_err(|e| err(&e))?.to_rgba8())
            }
        }
        Some(_) => still(reader.decode().map_err(|e| err(&e))?.to_rgba8()),
        None => {
            return Err(format!(
                "{}: unknown image format (PNG, JPEG, WebP or GIF)",
                path.display()
            ))
        }
    };
    if frames.is_empty() {
        return Err(format!("{}: no frames", path.display()));
    }
    Ok(frames.into_iter().map(|f| if blur > 0.0 { blurred(f, blur) } else { f }).collect())
}

fn still(img: RgbaImage) -> Vec<Frame> {
    let (width, height) = img.dimensions();
    vec![Frame { width, height, rgba: img.into_raw(), delay: 0.0 }]
}

fn collect(frames: image::Frames<'_>, path: &Path) -> Result<Vec<Frame>, String> {
    let mut out = Vec::new();
    let mut bytes = 0usize;
    for f in frames.take(MAX_FRAMES) {
        let f = f.map_err(|e| format!("{}: {e}", path.display()))?;
        let (num, den) = f.delay().numer_denom_ms();
        // Browsers treat very short delays as 100 ms; so do we.
        let mut delay = if den == 0 { 0.1 } else { num as f32 / den as f32 / 1000.0 };
        if delay < 0.02 {
            delay = 0.1;
        }
        let buf = f.into_buffer();
        bytes += buf.len();
        if bytes > MAX_BYTES {
            break; // keep what fits
        }
        let (width, height) = buf.dimensions();
        out.push(Frame { width, height, rgba: buf.into_raw(), delay });
    }
    if out.len() == 1 {
        out[0].delay = 0.0;
    }
    Ok(out)
}

/// Gaussian-like blur: three box blurs per axis (straight alpha is
/// premultiplied first so transparent pixels don't darken edges).
pub fn blurred(mut f: Frame, radius: f32) -> Frame {
    let (w, h) = (f.width as usize, f.height as usize);
    if w == 0 || h == 0 {
        return f;
    }
    // Box sizes approximating a Gaussian with sigma = radius / 2.
    let sigma = (radius / 2.0).max(0.5);
    let boxes = box_sizes(sigma, 3);
    let mut px: Vec<[f32; 4]> = f
        .rgba
        .chunks_exact(4)
        .map(|p| {
            let a = p[3] as f32 / 255.0;
            [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, p[3] as f32]
        })
        .collect();
    let mut tmp = px.clone();
    for &b in &boxes {
        let r = (b - 1) / 2;
        box_pass(&px, &mut tmp, w, h, r, true);
        box_pass(&tmp, &mut px, w, h, r, false);
    }
    for (out, p) in f.rgba.chunks_exact_mut(4).zip(&px) {
        let a = p[3].clamp(0.0, 255.0);
        let k = if a > 0.0 { 255.0 / a } else { 0.0 };
        out[0] = (p[0] * k).round().clamp(0.0, 255.0) as u8;
        out[1] = (p[1] * k).round().clamp(0.0, 255.0) as u8;
        out[2] = (p[2] * k).round().clamp(0.0, 255.0) as u8;
        out[3] = a.round() as u8;
    }
    f
}

fn box_sizes(sigma: f32, n: usize) -> Vec<usize> {
    let ideal = ((12.0 * sigma * sigma / n as f32) + 1.0).sqrt();
    let mut wl = ideal.floor() as usize;
    if wl.is_multiple_of(2) {
        wl = wl.saturating_sub(1).max(1);
    }
    let wu = wl + 2;
    let m = ((12.0 * sigma * sigma - (n * wl * wl) as f32 - 4.0 * (n * wl) as f32 - 3.0 * n as f32)
        / (-4.0 * wl as f32 - 4.0))
        .round() as isize;
    (0..n).map(|i| if (i as isize) < m { wl } else { wu }).collect()
}

/// Running-sum box blur of radius `r` along one axis, clamping at edges.
fn box_pass(
    src: &[[f32; 4]],
    dst: &mut [[f32; 4]],
    w: usize,
    h: usize,
    r: usize,
    horizontal: bool,
) {
    let (len, lines) = if horizontal { (w, h) } else { (h, w) };
    let at = |line: usize, i: usize| if horizontal { line * w + i } else { i * w + line };
    let norm = 1.0 / (2 * r + 1) as f32;
    for line in 0..lines {
        let mut acc = [0.0f32; 4];
        for k in 0..=2 * r {
            let i = (k as isize - r as isize).clamp(0, len as isize - 1) as usize;
            let p = src[at(line, i)];
            for c in 0..4 {
                acc[c] += p[c];
            }
        }
        for i in 0..len {
            let o = &mut dst[at(line, i)];
            for c in 0..4 {
                o[c] = acc[c] * norm;
            }
            let add = (i + r + 1).min(len - 1);
            let sub = (i as isize - r as isize).max(0) as usize;
            let (pa, ps) = (src[at(line, add)], src[at(line, sub)]);
            for c in 0..4 {
                acc[c] += pa[c] - ps[c];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("seraphim-picture-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn jpeg_webp_and_animated_gif() {
        let d = dir();
        let img =
            RgbaImage::from_fn(8, 4, |x, _| image::Rgba([if x < 4 { 255 } else { 0 }, 0, 0, 255]));
        let jpg = d.join("a.jpg");
        image::DynamicImage::ImageRgba8(img.clone()).to_rgb8().save(&jpg).unwrap();
        let f = load(&jpg, 0.0).unwrap();
        assert_eq!((f.len(), f[0].width, f[0].height, f[0].delay), (1, 8, 4, 0.0));
        assert!(f[0].rgba[0] > 200, "red survives JPEG");
        let webp = d.join("a.webp");
        img.save(&webp).unwrap();
        assert_eq!(load(&webp, 0.0).unwrap()[0].rgba[..4], [255, 0, 0, 255], "lossless WebP");

        // Two-frame GIF: red 50 ms (clamped to 100), blue 200 ms.
        let gif = d.join("a.gif");
        {
            let file = std::fs::File::create(&gif).unwrap();
            let mut enc = image::codecs::gif::GifEncoder::new(file);
            let frame = |c: [u8; 4], ms| {
                image::Frame::from_parts(
                    RgbaImage::from_pixel(4, 4, image::Rgba(c)),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(ms, 1),
                )
            };
            enc.encode_frames([frame([255, 0, 0, 255], 10), frame([0, 0, 255, 255], 200)]).unwrap();
        }
        let f = load(&gif, 0.0).unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].rgba[..3], [255, 0, 0]);
        assert_eq!(f[1].rgba[..3], [0, 0, 255]);
        assert!(
            (f[0].delay - 0.1).abs() < 1e-3 && (f[1].delay - 0.2).abs() < 1e-3,
            "{} {}",
            f[0].delay,
            f[1].delay
        );
        assert!(load(&d.join("missing.png"), 0.0).is_err());
        std::fs::write(d.join("junk.png"), b"not an image").unwrap();
        assert!(load(&d.join("junk.png"), 0.0).is_err());
    }

    #[test]
    fn blur_spreads_and_keeps_flat_areas() {
        let mut rgba = Vec::new();
        for _y in 0..9 {
            for x in 0..9 {
                rgba.extend_from_slice(&if x == 4 { [255, 255, 255, 255] } else { [0, 0, 0, 255] });
            }
        }
        let f = blurred(Frame { width: 9, height: 9, rgba, delay: 0.0 }, 4.0);
        let at = |x: usize, y: usize| f.rgba[(y * 9 + x) * 4];
        assert!(at(4, 4) < 200 && at(4, 4) > at(2, 4) && at(2, 4) > 0, "a soft bump");
        assert_eq!(at(4, 0), at(4, 8), "uniform along the line");
        // Transparent neighbours don't darken an opaque color.
        let mut rgba = vec![0u8; 4 * 4 * 4];
        rgba[..4].copy_from_slice(&[200, 100, 50, 255]);
        let f = blurred(Frame { width: 4, height: 4, rgba, delay: 0.0 }, 2.0);
        let p = &f.rgba[..4];
        assert!(p[3] > 0 && p[3] < 255);
        assert!((p[0] as i32 - 200).abs() <= 2 && (p[1] as i32 - 100).abs() <= 2, "{p:?}");
    }
}
