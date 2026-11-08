//! Cursor and selection effects: glide, trail, blink fade, glow, typing
//! ripples and selection fade-in. All time-based (spec §6.4), so they
//! take the same time at any frame rate; [`CursorEffects::still`] is what
//! reduced motion uses.

use std::time::{Duration, Instant};

/// Effect settings (mirrors `[cursor]` in the configuration). The default
/// is no effects at all.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CursorEffects {
    pub glide_ms: u32,
    pub trail: bool,
    pub blink_fade_ms: u32,
    /// 0..1
    pub glow: f32,
    pub ripple: bool,
    pub selection_fade_ms: u32,
}

impl CursorEffects {
    /// Without motion (reduced motion): only the static glow stays.
    pub fn still(self) -> Self {
        CursorEffects { glow: self.glow, ..Default::default() }
    }
}

/// How long a ripple spreads.
pub const RIPPLE: Duration = Duration::from_millis(450);
const MAX_RIPPLES: usize = 8;

pub type Rect = [f32; 4];

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

fn lerp(a: Rect, b: Rect, t: f32) -> Rect {
    [0, 1, 2, 3].map(|i| a[i] + (b[i] - a[i]) * t)
}

fn secs(d: Duration) -> f32 {
    d.as_secs_f32()
}

#[derive(Debug)]
pub(crate) struct Fx {
    pub cfg: CursorEffects,
    /// Fixed time (tests); otherwise the wall clock.
    pub clock: Option<Instant>,
    from: Rect,
    to: Rect,
    start: Instant,
    known: bool,
    blink_on: bool,
    blink_at: Option<Instant>,
    ripples: Vec<([f32; 2], Instant)>,
    pending_ripples: u32,
    had_selection: bool,
    selection_at: Option<Instant>,
}

impl Default for Fx {
    fn default() -> Self {
        Fx {
            cfg: CursorEffects::default(),
            clock: None,
            from: [0.0; 4],
            to: [0.0; 4],
            start: Instant::now(),
            known: false,
            blink_on: true,
            blink_at: None,
            ripples: Vec::new(),
            pending_ripples: 0,
            had_selection: false,
            selection_at: None,
        }
    }
}

impl Fx {
    pub fn now(&self) -> Instant {
        self.clock.unwrap_or_else(Instant::now)
    }

    fn glide(&self, now: Instant) -> f32 {
        if self.cfg.glide_ms == 0 {
            return 1.0;
        }
        secs(now.saturating_duration_since(self.start)) / (self.cfg.glide_ms as f32 / 1000.0)
    }

    /// The cursor's drawn rectangle for this frame and how far along the
    /// glide is (1 = arrived). Calling it twice in a frame is harmless.
    pub fn cursor(&mut self, target: Rect, now: Instant) -> (Rect, f32) {
        if !self.known || self.cfg.glide_ms == 0 {
            self.known = true;
            self.from = target;
            self.to = target;
            return (target, 1.0);
        }
        if target != self.to {
            // Retarget from wherever it is now: no jump mid-glide.
            self.from = lerp(self.from, self.to, ease_out(self.glide(now)));
            self.to = target;
            self.start = now;
        }
        let t = self.glide(now).min(1.0);
        (lerp(self.from, self.to, ease_out(t)), t)
    }

    /// Fading copies behind a gliding cursor: (rect, alpha).
    pub fn trail(&self, now: Instant) -> Vec<(Rect, f32)> {
        let t = self.glide(now);
        if !self.cfg.trail || t >= 1.0 {
            return Vec::new();
        }
        (1..=4)
            .filter_map(|k| {
                let tk = t - k as f32 * 0.12;
                (tk > -0.12).then(|| {
                    (lerp(self.from, self.to, ease_out(tk.max(0.0))), 0.32 * (1.0 - k as f32 / 5.0))
                })
            })
            .collect()
    }

    /// Cursor opacity for the blink phase `on`.
    pub fn blink_alpha(&mut self, on: bool, now: Instant) -> f32 {
        if self.cfg.blink_fade_ms == 0 {
            self.blink_on = on;
            self.blink_at = None;
            return on as u8 as f32;
        }
        if on != self.blink_on {
            self.blink_on = on;
            self.blink_at = Some(now);
        }
        let Some(at) = self.blink_at else { return on as u8 as f32 };
        let t = ease_out(
            secs(now.saturating_duration_since(at)) / (self.cfg.blink_fade_ms as f32 / 1000.0),
        );
        if on {
            t
        } else {
            1.0 - t
        }
    }

    /// Opacity for the selection background (fades in when it appears).
    pub fn selection_alpha(&mut self, has: bool, now: Instant) -> f32 {
        if has && !self.had_selection {
            self.selection_at = Some(now);
        }
        self.had_selection = has;
        match (self.cfg.selection_fade_ms, self.selection_at) {
            (0, _) | (_, None) => 1.0,
            (ms, Some(at)) => {
                ease_out(secs(now.saturating_duration_since(at)) / (ms as f32 / 1000.0))
            }
        }
    }

    /// A key was pressed: spread a ring from the cursor (next frame).
    pub fn ripple(&mut self) {
        if self.cfg.ripple {
            self.pending_ripples += 1;
        }
    }

    /// Live ripples as (center, progress 0..1); new ones start at `center`.
    pub fn ripples(&mut self, center: [f32; 2], now: Instant) -> Vec<([f32; 2], f32)> {
        for _ in 0..std::mem::take(&mut self.pending_ripples) {
            self.ripples.push((center, now));
        }
        if self.ripples.len() > MAX_RIPPLES {
            let drop = self.ripples.len() - MAX_RIPPLES;
            self.ripples.drain(..drop);
        }
        self.ripples.retain(|(_, at)| now.saturating_duration_since(*at) < RIPPLE);
        self.ripples
            .iter()
            .map(|(c, at)| (*c, secs(now.saturating_duration_since(*at)) / secs(RIPPLE)))
            .collect()
    }

    /// Something is still moving: keep drawing frames.
    pub fn animating(&self) -> bool {
        let now = self.now();
        let fading = |at: Option<Instant>, ms: u32| {
            ms > 0
                && at.is_some_and(|a| {
                    now.saturating_duration_since(a) < Duration::from_millis(ms as u64)
                })
        };
        (self.known && self.cfg.glide_ms > 0 && self.glide(now) < 1.0)
            || fading(self.blink_at, self.cfg.blink_fade_ms)
            || fading(self.selection_at, self.cfg.selection_fade_ms)
            || self.pending_ripples > 0
            || self.ripples.iter().any(|(_, at)| now.saturating_duration_since(*at) < RIPPLE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(cfg: CursorEffects) -> (Fx, Instant) {
        let t0 = Instant::now();
        (Fx { cfg, clock: Some(t0), ..Default::default() }, t0)
    }

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn glide_moves_smoothly_and_retargets() {
        let (mut f, t0) = fx(CursorEffects { glide_ms: 100, ..Default::default() });
        let a = [0.0, 0.0, 10.0, 20.0];
        let b = [100.0, 0.0, 10.0, 20.0];
        assert_eq!(f.cursor(a, t0), (a, 1.0), "first position: no glide");
        let (r, t) = f.cursor(b, t0);
        assert_eq!((r[0], t), (0.0, 0.0));
        let (r, _) = f.cursor(b, t0 + MS(50));
        assert!(r[0] > 50.0 && r[0] < 100.0, "ease-out: past half-way at half time ({})", r[0]);
        f.clock = Some(t0 + MS(50));
        assert!(f.animating());
        // Retarget mid-glide: starts from where it is, no jump.
        let mid = r[0];
        let (r2, _) = f.cursor(a, t0 + MS(50));
        assert_eq!(r2[0], mid);
        let (r3, t) = f.cursor(a, t0 + MS(200));
        assert_eq!((r3, t), (a, 1.0));
        f.clock = Some(t0 + MS(200));
        assert!(!f.animating());
    }

    #[test]
    fn no_glide_jumps() {
        let (mut f, t0) = fx(CursorEffects::default());
        f.cursor([0.0; 4], t0);
        assert_eq!(f.cursor([5.0, 5.0, 1.0, 1.0], t0).0, [5.0, 5.0, 1.0, 1.0]);
        assert!(!f.animating());
    }

    #[test]
    fn trail_follows_and_vanishes() {
        let (mut f, t0) = fx(CursorEffects { glide_ms: 100, trail: true, ..Default::default() });
        f.cursor([0.0; 4], t0);
        f.cursor([100.0, 0.0, 0.0, 0.0], t0);
        let tr = f.trail(t0 + MS(60));
        assert!(!tr.is_empty());
        let lead = f.cursor([100.0, 0.0, 0.0, 0.0], t0 + MS(60)).0[0];
        assert!(tr.iter().all(|(r, a)| r[0] <= lead && *a > 0.0 && *a < 0.5), "{tr:?}");
        assert!(f.trail(t0 + MS(150)).is_empty());
    }

    #[test]
    fn blink_fades() {
        let (mut f, t0) = fx(CursorEffects { blink_fade_ms: 100, ..Default::default() });
        assert_eq!(f.blink_alpha(true, t0), 1.0);
        assert_eq!(f.blink_alpha(false, t0), 1.0, "the fade starts when the switch is seen");
        let a = f.blink_alpha(false, t0 + MS(50));
        assert!(a > 0.0 && a < 1.0, "{a}");
        assert_eq!(f.blink_alpha(false, t0 + MS(200)), 0.0);
        let (mut hard, t0) = fx(CursorEffects::default());
        assert_eq!(hard.blink_alpha(false, t0), 0.0);
    }

    #[test]
    fn ripples_spread_and_end() {
        let (mut f, t0) = fx(CursorEffects { ripple: true, ..Default::default() });
        f.ripple();
        assert!(f.animating());
        let r = f.ripples([5.0, 5.0], t0);
        assert_eq!(r, vec![([5.0, 5.0], 0.0)]);
        let r = f.ripples([9.0, 9.0], t0 + MS(225));
        assert_eq!(r[0].0, [5.0, 5.0], "stays where it started");
        assert!((r[0].1 - 0.5).abs() < 0.01);
        assert!(f.ripples([0.0; 2], t0 + MS(500)).is_empty());
        let (mut off, _) = fx(CursorEffects::default());
        off.ripple();
        assert!(!off.animating(), "disabled");
    }

    #[test]
    fn selection_fades_in_once() {
        let (mut f, t0) = fx(CursorEffects { selection_fade_ms: 100, ..Default::default() });
        assert_eq!(f.selection_alpha(true, t0), 0.0);
        assert!(f.selection_alpha(true, t0 + MS(50)) > 0.5);
        assert_eq!(f.selection_alpha(true, t0 + MS(150)), 1.0);
        f.selection_alpha(false, t0 + MS(160));
        assert_eq!(f.selection_alpha(true, t0 + MS(170)), 0.0, "a new selection fades again");
    }

    #[test]
    fn still_keeps_only_glow() {
        let c = CursorEffects {
            glide_ms: 80,
            trail: true,
            blink_fade_ms: 1,
            glow: 0.4,
            ripple: true,
            selection_fade_ms: 9,
        };
        assert_eq!(c.still(), CursorEffects { glow: 0.4, ..Default::default() });
    }
}
