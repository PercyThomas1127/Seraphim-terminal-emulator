//! Sound through the sound server's own tools, no audio library:
//!
//! * playing: `pw-play` (PipeWire), `paplay` (PulseAudio), `aplay`
//!   (ALSA, WAV only), `ffplay`; the first one installed is used;
//! * the bell: the desktop's bell sound (`canberra-gtk-play`), or a short
//!   tone Seraphim writes once as a WAV file;
//! * levels: what the default output plays, read from its monitor with
//!   `parec` (PulseAudio or pipewire-pulse) or `ffmpeg`, split into bass,
//!   mid and treble. Capture starts the first time a level is read, so
//!   nothing listens unless something (a `[[bind]]` on `audio.*`) asks.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

/// Play a sound file at `volume` (0–1). Never blocks. Returns false if no
/// player could be started.
pub fn play(path: &Path, volume: f32) -> bool {
    let v = volume.clamp(0.0, 1.0);
    let p = path.to_string_lossy().into_owned();
    let wav = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav"));
    let mut tries: Vec<Command> = Vec::new();
    // The sound server must not remember these streams' volumes: under a
    // generic name ("paplay") WirePlumber would apply Seraphim's volume to
    // every later stream of that name, from any program.
    let mut c = Command::new("pw-play");
    c.args([
        "--volume",
        &format!("{v:.3}"),
        "--properties",
        "{ application.name = Seraphim state.restore-props = false }",
        &p,
    ]);
    tries.push(c);
    let mut c = Command::new("paplay");
    c.args([
        &format!("--volume={}", pulse_volume(v)),
        "--client-name=Seraphim",
        "--property=state.restore-props=false",
        &p,
    ]);
    tries.push(c);
    if wav {
        let mut c = Command::new("aplay");
        c.args(["-q", &p]);
        tries.push(c);
    }
    let mut c = Command::new("ffplay");
    c.args([
        "-nodisp",
        "-autoexit",
        "-loglevel",
        "quiet",
        "-volume",
        &format!("{}", (v * 100.0) as u32),
        &p,
    ]);
    tries.push(c);
    tries.into_iter().any(spawn)
}

/// `volume` (0–1, linear, as `pw-play` and `ffplay` take it) as a
/// PulseAudio volume, which is cubic: 65536 is 100%, and 0.3 linear is
/// 67% (`pa_sw_volume_from_linear`).
fn pulse_volume(volume: f32) -> u32 {
    (volume.clamp(0.0, 1.0).cbrt() * 65536.0).round() as u32
}

fn spawn(mut cmd: Command) -> bool {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    match cmd.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
            true
        }
        Err(_) => false,
    }
}

/// Ring the bell: `"default"` is the desktop's bell sound, else a tone;
/// anything else is a sound file.
pub fn bell(sound: &str, volume: f32) -> bool {
    match sound {
        "none" | "" => false,
        "default" => {
            let mut c = Command::new("canberra-gtk-play");
            c.args(["-i", "bell", "-d", "Seraphim"]);
            spawn(c) || tone().is_some_and(|t| play(&t, volume))
        }
        "tone" => tone().is_some_and(|t| play(&t, volume)),
        path => play(Path::new(path), volume),
    }
}

/// A short soft tone (880 Hz, 120 ms, fading out) as a WAV file, written
/// once into the cache directory.
pub fn tone() -> Option<PathBuf> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        let dir = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?
            .join("seraphim");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("bell.wav");
        if !path.exists() {
            std::fs::write(&path, tone_wav(22050, 880.0, 0.12)).ok()?;
        }
        Some(path)
    })
    .clone()
}

/// A 16-bit mono WAV of a sine tone with a quick fade in and a longer fade
/// out.
pub fn tone_wav(rate: u32, freq: f32, seconds: f32) -> Vec<u8> {
    let n = (rate as f32 * seconds) as u32;
    let mut data = Vec::with_capacity(n as usize * 2);
    for i in 0..n {
        let t = i as f32 / rate as f32;
        let env = (t / 0.005).min(1.0) * (1.0 - i as f32 / n as f32).powi(2);
        let s = (2.0 * std::f32::consts::PI * freq * t).sin() * env * 0.4;
        data.extend_from_slice(&((s * i16::MAX as f32) as i16).to_le_bytes());
    }
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend(data);
    out
}

/// How loud the default output is, 0–1 on a decibel scale (−60 dB → 0),
/// overall and per band.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Levels {
    pub level: f32,
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
}

/// Splits samples into bands with one-pole filters (bass below 250 Hz,
/// treble above 2 kHz) and follows their loudness: fast to rise, slower
/// to fall, as a meter does.
#[derive(Clone, Debug)]
pub struct Analyzer {
    low: f32,
    high: f32,
    a_low: f32,
    a_high: f32,
    pub levels: Levels,
}

impl Analyzer {
    pub fn new(rate: f32) -> Analyzer {
        let a = |fc: f32| 1.0 - (-2.0 * std::f32::consts::PI * fc / rate).exp();
        Analyzer {
            low: 0.0,
            high: 0.0,
            a_low: a(250.0),
            a_high: a(2000.0),
            levels: Levels::default(),
        }
    }

    /// Take one block of samples (about 30 ms).
    pub fn block(&mut self, samples: &[i16]) -> Levels {
        if samples.is_empty() {
            return self.levels;
        }
        let (mut all, mut bass, mut mid, mut treble) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for &s in samples {
            let x = s as f32 / 32768.0;
            self.low += self.a_low * (x - self.low);
            self.high += self.a_high * (x - self.high);
            let (b, m, t) = (self.low, self.high - self.low, x - self.high);
            all += x * x;
            bass += b * b;
            mid += m * m;
            treble += t * t;
        }
        let n = samples.len() as f32;
        let db =
            |sq: f32| ((20.0 * (sq / n).sqrt().max(1e-9).log10() + 60.0) / 60.0).clamp(0.0, 1.0);
        let follow = |old: f32, new: f32| if new > old { new } else { old * 0.85 + new * 0.15 };
        let l = &mut self.levels;
        l.level = follow(l.level, db(all));
        l.bass = follow(l.bass, db(bass));
        l.mid = follow(l.mid, db(mid));
        l.treble = follow(l.treble, db(treble));
        *l
    }
}

/// Capture rate. Resampling to it filters out everything near half of it,
/// so it must sit well above the treble band: at 11025 Hz a 5 kHz tone
/// already read 13 dB low. 24 kHz is half of the usual 48 kHz.
const RATE: u32 = 24000;
/// Samples per analysed block (about 33 ms).
const BLOCK: usize = RATE as usize / 30;

/// The latest levels and when they were read.
type Shared = Arc<Mutex<Option<(Levels, Instant)>>>;

/// The latest levels of the default output, starting capture on first use.
/// `None` until the first block arrives, or if no capture tool works.
pub fn levels() -> Option<Levels> {
    static LATEST: OnceLock<Shared> = OnceLock::new();
    let latest = LATEST.get_or_init(|| {
        let latest = Arc::new(Mutex::new(None));
        let out = latest.clone();
        let _ =
            std::thread::Builder::new().name("seraphim-audio".into()).spawn(move || capture(out));
        latest
    });
    // Stale levels (the capture ended) read as silence gone missing.
    let l = *latest.lock().unwrap();
    l.filter(|(_, t)| t.elapsed().as_secs() < 2).map(|(l, _)| l)
}

fn capture(out: Shared) {
    let rate = RATE.to_string();
    let tries: Vec<Vec<&str>> = vec![
        vec![
            "parec",
            "--device=@DEFAULT_MONITOR@",
            "--format=s16le",
            "--channels=1",
            "--latency-msec=30",
        ],
        vec![
            "ffmpeg",
            "-loglevel",
            "quiet",
            "-f",
            "pulse",
            "-i",
            "@DEFAULT_MONITOR@",
            "-ac",
            "1",
            "-ar",
            &rate,
            "-f",
            "s16le",
            "-",
        ],
    ];
    for argv in tries {
        let mut cmd = Command::new(argv[0]);
        cmd.args(&argv[1..]);
        if argv[0] == "parec" {
            cmd.arg(format!("--rate={RATE}"));
        }
        let Ok(mut child) =
            cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
        else {
            continue;
        };
        let Some(mut stdout) = child.stdout.take() else { continue };
        let mut an = Analyzer::new(RATE as f32);
        let mut buf = vec![0u8; BLOCK * 2];
        let mut got_any = false;
        while stdout.read_exact(&mut buf).is_ok() {
            got_any = true;
            let samples: Vec<i16> =
                buf.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
            let l = an.block(&samples);
            *out.lock().unwrap() = Some((l, Instant::now()));
        }
        let _ = child.kill();
        let _ = child.wait();
        if got_any {
            // It worked, then stopped (the sound server restarted): try
            // again from the top after a pause.
            std::thread::sleep(std::time::Duration::from_secs(2));
            return capture(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, amp: f32, n: usize) -> Vec<i16> {
        (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                ((2.0 * std::f32::consts::PI * freq * t).sin() * amp * 32767.0) as i16
            })
            .collect()
    }

    #[test]
    fn bands_follow_the_frequency() {
        let mut a = Analyzer::new(RATE as f32);
        let silence = a.block(&vec![0; BLOCK]);
        assert_eq!(silence.level, 0.0);
        let steady = |freq: f32| {
            let mut a = Analyzer::new(RATE as f32);
            for _ in 0..5 {
                a.block(&sine(freq, 0.5, BLOCK));
            }
            a
        };
        let low = steady(60.0).levels;
        assert!(low.bass > low.treble + 0.2 && low.level > 0.8, "{low:?}");
        let mut a = steady(4000.0);
        let high = a.levels;
        assert!(high.treble > high.bass + 0.2, "{high:?}");
        // Treble stands out from mid, up to well past 5 kHz.
        for f in [5000.0, 8000.0] {
            let l = steady(f).levels;
            assert!(l.treble > l.mid + 0.1 && l.level > 0.8, "{f} Hz: {l:?}");
        }
        // Falling is slower than rising.
        let after = a.block(&vec![0; BLOCK]);
        assert!(after.level > 0.5, "{after:?}");
    }

    #[test]
    fn pulse_volume_is_cubic() {
        assert_eq!(pulse_volume(1.0), 65536);
        assert_eq!(pulse_volume(0.125), 32768);
        assert_eq!(pulse_volume(0.0), 0);
        assert_eq!(pulse_volume(2.0), 65536);
    }

    #[test]
    fn the_tone_is_a_wav() {
        let w = tone_wav(22050, 880.0, 0.12);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(w.len(), 44 + (22050.0 * 0.12) as usize * 2);
    }
}
