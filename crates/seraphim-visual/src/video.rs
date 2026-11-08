//! Video backgrounds, decoded by an `ffmpeg` subprocess into raw RGBA
//! frames (no codecs linked into Seraphim). A reader thread fills a small
//! bounded queue, so memory stays flat and ffmpeg simply blocks while the
//! window is paused. The video loops; audio is ignored.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

/// Videos wider or taller than this are scaled down while decoding.
pub const MAX_SIDE: u32 = 1920;
/// Frames decoded ahead.
const QUEUE: usize = 3;

pub struct Stream {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    frames: Receiver<Vec<u8>>,
    child: Child,
    /// Index of the frame on screen (0-based, counting loops).
    pub shown: u64,
}

/// Width, height and frame rate of the first video stream.
fn probe(path: &Path) -> Result<(u32, u32, f64), String> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,avg_frame_rate",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("video needs ffmpeg/ffprobe on PATH ({e})"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "{}: {}",
            path.display(),
            err.lines().next().unwrap_or("not a video").trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let f: Vec<&str> = text.trim().split(',').collect();
    let (w, h) = (f.first().and_then(|s| s.parse().ok()), f.get(1).and_then(|s| s.parse().ok()));
    let fps = f.get(2).and_then(|r| {
        let (n, d) = r.split_once('/')?;
        let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
        (d > 0.0 && n > 0.0).then(|| n / d)
    });
    match (w, h) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Ok((w, h, fps.unwrap_or(30.0).clamp(1.0, 120.0))),
        _ => Err(format!("{}: no video stream", path.display())),
    }
}

impl Stream {
    /// Start decoding `path`; waits (up to a few seconds) for the first
    /// frame so the layer never shows garbage.
    pub fn open(path: &Path) -> Result<(Stream, Vec<u8>), String> {
        let (w0, h0, fps) = probe(path)?;
        let s = (MAX_SIDE as f64 / w0.max(h0) as f64).min(1.0);
        // Even sizes keep every pixel format happy.
        let (w, h) =
            (((w0 as f64 * s) as u32 / 2 * 2).max(2), ((h0 as f64 * s) as u32 / 2 * 2).max(2));
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-nostdin", "-loglevel", "error", "-stream_loop", "-1", "-i"])
            .arg(path)
            .args([
                "-an",
                "-vf",
                &format!("scale={w}:{h}"),
                "-pix_fmt",
                "rgba",
                "-f",
                "rawvideo",
                "pipe:1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // If Seraphim is killed (no destructors run), the kernel stops
        // ffmpeg too instead of leaving it decoding.
        #[cfg(target_os = "linux")]
        unsafe {
            use std::os::unix::process::CommandExt;
            // SAFETY: prctl is async-signal-safe; nothing else runs here.
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let mut child = cmd.spawn().map_err(|e| format!("video needs ffmpeg on PATH ({e})"))?;
        let mut out = child.stdout.take().expect("piped");
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(QUEUE);
        let size = (w * h * 4) as usize;
        std::thread::Builder::new()
            .name("seraphim-video".into())
            .spawn(move || loop {
                let mut frame = vec![0u8; size];
                if out.read_exact(&mut frame).is_err() || tx.send(frame).is_err() {
                    return; // ffmpeg ended or the layer is gone
                }
            })
            .map_err(|e| e.to_string())?;
        let first = rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| format!("{}: ffmpeg produced no frames", path.display()))?;
        Ok((Stream { width: w, height: h, fps, frames: rx, child, shown: 0 }, first))
    }

    /// The newest frame due by `t` seconds of play, if a new one is due
    /// and decoded (late frames are skipped, not queued up). `wait` blocks
    /// for frames that are due but not decoded yet (tests, screenshots).
    pub fn frame_for(&mut self, t: f64, wait: bool) -> Option<Vec<u8>> {
        let due = (t.max(0.0) * self.fps) as u64;
        let mut latest = None;
        while self.shown < due {
            let next = if wait {
                match self.frames.recv_timeout(Duration::from_secs(5)) {
                    Ok(f) => Some(f),
                    Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => None,
                }
            } else {
                self.frames.try_recv().ok()
            };
            match next {
                Some(f) => {
                    self.shown += 1;
                    latest = Some(f);
                }
                None => break,
            }
        }
        latest
    }

    /// Seconds until the next frame is due at time `t`.
    pub fn next_in(&self, t: f64) -> f64 {
        let next = (self.shown + 1) as f64 / self.fps;
        (next - t).max(0.001)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
