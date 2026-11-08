//! The Animation Engine (spec §6): how visual properties change over time.
//!
//! Everything is a pure function of time (seconds on the engine clock), so
//! animations take the same duration at any refresh rate (spec §6.4) and
//! are deterministic to test.

/// Easing curves (spec §6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Easing {
    Linear,
    EaseIn,
    EaseOut,
    #[default]
    EaseInOut,
    EaseOutExpo,
    /// Overshoots slightly, then settles.
    EaseOutBack,
}

impl Easing {
    pub fn parse(s: &str) -> Option<Easing> {
        Some(match s {
            "linear" => Easing::Linear,
            "ease-in" => Easing::EaseIn,
            "ease-out" => Easing::EaseOut,
            "ease-in-out" | "ease" => Easing::EaseInOut,
            "ease-out-expo" => Easing::EaseOutExpo,
            "ease-out-back" => Easing::EaseOutBack,
            _ => return None,
        })
    }

    /// Map linear progress `t` in 0..=1 to eased progress.
    pub fn apply(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::EaseIn => t * t * t,
            Easing::EaseOut => 1.0 - (1.0 - t).powi(3),
            Easing::EaseInOut => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
                }
            }
            Easing::EaseOutExpo => {
                if t >= 1.0 {
                    1.0
                } else {
                    1.0 - 2f64.powf(-10.0 * t)
                }
            }
            Easing::EaseOutBack => {
                let (c1, c3) = (1.70158, 2.70158);
                1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
            }
        }
    }
}

/// How a value moves to a new target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Transition {
    /// Jump immediately.
    Instant,
    /// Fixed-duration tween (seconds).
    Tween { duration: f64, easing: Easing },
    /// Critically-damped-ish spring: natural motion that can be retargeted
    /// mid-flight without a jolt.
    Spring { stiffness: f64, damping: f64 },
}

impl Transition {
    pub fn tween(seconds: f64) -> Self {
        Transition::Tween { duration: seconds, easing: Easing::EaseInOut }
    }
}

/// Looping behaviour for keyframe animations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repeat {
    Once,
    Loop,
    /// Forward, then backward (reversible animation, spec §6.2).
    PingPong,
}

#[derive(Clone, Debug, PartialEq)]
enum Motion {
    Still,
    Tween { from: f64, start: f64, duration: f64, easing: Easing },
    Spring { velocity: f64, stiffness: f64, damping: f64, at: f64 },
    Keyframes { frames: Vec<(f64, f64)>, start: f64, repeat: Repeat, easing: Easing },
}

/// A numeric property that can be animated (opacity, shader parameter,
/// color channel...).
#[derive(Clone, Debug, PartialEq)]
pub struct Animated {
    target: f64,
    /// Current value for springs (integrated), else derived from `motion`.
    value: f64,
    motion: Motion,
}

/// Spring integration step (fixed, so results don't depend on frame rate).
const STEP: f64 = 1.0 / 240.0;

impl Animated {
    pub fn new(value: f64) -> Self {
        Animated { target: value, value, motion: Motion::Still }
    }

    pub fn target(&self) -> f64 {
        self.target
    }

    /// Move toward `to`, starting from wherever the value is at `now`.
    pub fn set(&mut self, to: f64, transition: Transition, now: f64) {
        let current = self.value_at(now);
        self.target = to;
        self.motion = match transition {
            Transition::Instant => {
                self.value = to;
                Motion::Still
            }
            Transition::Tween { duration, .. } if duration <= 0.0 => {
                self.value = to;
                Motion::Still
            }
            Transition::Tween { duration, easing } => {
                Motion::Tween { from: current, start: now, duration, easing }
            }
            Transition::Spring { stiffness, damping } => {
                let velocity = match self.motion {
                    Motion::Spring { velocity, .. } => velocity,
                    _ => 0.0,
                };
                self.value = current;
                Motion::Spring { velocity, stiffness, damping, at: now }
            }
        };
    }

    /// Play `frames` = (time offset, value) pairs starting at `now`
    /// (keyframe animation / timeline, spec §6.2).
    pub fn play(&mut self, mut frames: Vec<(f64, f64)>, repeat: Repeat, easing: Easing, now: f64) {
        frames.sort_by(|a, b| a.0.total_cmp(&b.0));
        if frames.is_empty() {
            return;
        }
        self.target = frames.last().unwrap().1;
        self.motion = Motion::Keyframes { frames, start: now, repeat, easing };
    }

    /// Value at time `now`. Takes `&mut self` because springs integrate.
    pub fn value(&mut self, now: f64) -> f64 {
        if let Motion::Spring { velocity, stiffness, damping, at } = &mut self.motion {
            let (mut v, mut x) = (*velocity, self.value);
            while *at + STEP <= now {
                let force = stiffness.max(0.0) * (self.target - x) - damping.max(0.0) * v;
                v += force * STEP;
                x += v * STEP;
                *at += STEP;
            }
            *velocity = v;
            self.value = x;
            if (self.target - x).abs() < 1e-4 && v.abs() < 1e-3 {
                self.value = self.target;
                self.motion = Motion::Still;
            }
            return self.value;
        }
        let v = self.value_at(now);
        if !self.active(now) {
            if let Motion::Tween { .. } = self.motion {
                self.value = self.target;
                self.motion = Motion::Still;
            }
        }
        v
    }

    fn value_at(&self, now: f64) -> f64 {
        match &self.motion {
            Motion::Still | Motion::Spring { .. } => self.value,
            Motion::Tween { from, start, duration, easing } => {
                let t = ((now - start) / duration).clamp(0.0, 1.0);
                from + (self.target - from) * easing.apply(t)
            }
            Motion::Keyframes { frames, start, repeat, easing } => {
                let total = frames.last().unwrap().0.max(1e-9);
                let mut t = (now - start).max(0.0);
                t = match repeat {
                    Repeat::Once => t.min(total),
                    Repeat::Loop => t % total,
                    Repeat::PingPong => {
                        let k = t % (2.0 * total);
                        if k > total {
                            2.0 * total - k
                        } else {
                            k
                        }
                    }
                };
                keyframe(frames, t, *easing)
            }
        }
    }

    /// Whether the value is still changing at `now` (the window keeps
    /// rendering frames only while something is).
    pub fn active(&self, now: f64) -> bool {
        match &self.motion {
            Motion::Still => false,
            Motion::Tween { start, duration, .. } => now < start + duration,
            Motion::Spring { .. } => true,
            Motion::Keyframes { frames, start, repeat, .. } => {
                *repeat != Repeat::Once || now < start + frames.last().unwrap().0
            }
        }
    }
}

fn keyframe(frames: &[(f64, f64)], t: f64, easing: Easing) -> f64 {
    if t <= frames[0].0 {
        return frames[0].1;
    }
    for w in frames.windows(2) {
        let ((t0, v0), (t1, v1)) = (w[0], w[1]);
        if t <= t1 {
            let p = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1.0 };
            return v0 + (v1 - v0) * easing.apply(p);
        }
    }
    frames.last().unwrap().1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn easings_hit_their_endpoints() {
        for e in [
            Easing::Linear,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
            Easing::EaseOutExpo,
            Easing::EaseOutBack,
        ] {
            assert!(close(e.apply(0.0), 0.0), "{e:?}");
            assert!(close(e.apply(1.0), 1.0), "{e:?}");
        }
        assert!(Easing::EaseOutBack.apply(0.8) > 1.0, "overshoots");
        assert!(close(Easing::EaseInOut.apply(0.5), 0.5));
    }

    #[test]
    fn tween_is_time_based_and_finishes() {
        let mut a = Animated::new(0.0);
        a.set(10.0, Transition::Tween { duration: 2.0, easing: Easing::Linear }, 100.0);
        assert!(close(a.value(101.0), 5.0));
        assert!(a.active(101.0));
        assert!(close(a.value(105.0), 10.0));
        assert!(!a.active(105.0));
    }

    #[test]
    fn retargeting_starts_from_the_current_value() {
        let mut a = Animated::new(0.0);
        a.set(10.0, Transition::Tween { duration: 1.0, easing: Easing::Linear }, 0.0);
        a.set(0.0, Transition::Tween { duration: 1.0, easing: Easing::Linear }, 0.5);
        assert!(close(a.value(0.5), 5.0), "no jump when retargeted");
        assert!(close(a.value(1.0), 2.5));
    }

    #[test]
    fn spring_settles_regardless_of_sampling_rate() {
        let run = |dt: f64| {
            let mut a = Animated::new(0.0);
            a.set(1.0, Transition::Spring { stiffness: 120.0, damping: 20.0 }, 0.0);
            let mut t = 0.0;
            while t + dt <= 0.25 {
                t += dt;
                a.value(t);
            }
            a.value(0.25)
        };
        // 30 fps and 240 fps sampling agree (fixed integration step).
        assert!((run(1.0 / 30.0) - run(1.0 / 240.0)).abs() < 1e-3);
        let mut a = Animated::new(0.0);
        a.set(1.0, Transition::Spring { stiffness: 120.0, damping: 20.0 }, 0.0);
        assert!(close(a.value(5.0), 1.0));
        assert!(!a.active(5.0), "a settled spring stops requesting frames");
    }

    #[test]
    fn keyframes_loop_and_ping_pong() {
        let frames = vec![(0.0, 0.0), (1.0, 10.0)];
        let mut a = Animated::new(0.0);
        a.play(frames.clone(), Repeat::Loop, Easing::Linear, 0.0);
        assert!(close(a.value(1.5), 5.0));
        a.play(frames.clone(), Repeat::PingPong, Easing::Linear, 0.0);
        assert!(close(a.value(1.5), 5.0));
        assert!(close(a.value(1.75), 2.5), "coming back down");
        a.play(frames, Repeat::Once, Easing::Linear, 0.0);
        assert!(close(a.value(9.0), 10.0));
        assert!(!a.active(9.0));
    }
}
