//! Continuous bindings (`[[bind]]`): a layer parameter follows a live value
//! — a system metric sampled on an interval, or a field of the latest event
//! of a kind — eased toward each new value.
//!
//! Metrics come from Linux's /proc and /sys; where a source is missing
//! (no battery, another OS) the binding simply leaves the parameter alone.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use seraphim_events::{EventBus, Pattern, Subscription, Value};
pub use seraphim_script::metrics::{Metric, Sampler};
use seraphim_visual::{Easing, Transition, VisualEngine};

#[derive(Clone, Debug, PartialEq)]
enum Input {
    Metric(Metric),
    Event { pattern: Pattern, name: String, field: Vec<String> },
}

/// A validated `[[bind]]`.
#[derive(Clone, Debug)]
pub struct Bind {
    input: Input,
    layer: String,
    param: String,
    range: [f32; 2],
    output: [f32; 2],
    map: BTreeMap<String, f32>,
    /// A color binding: `map` gives an index into these.
    palette: Vec<[f32; 4]>,
    /// Which color of the layer (`None`: its first).
    color: Option<String>,
    smoothing: Duration,
    interval: Duration,
}

impl Bind {
    /// `theme` resolves theme colors (`theme.bright.1`) in color bindings.
    pub fn from_config(b: &seraphim_config::Bind, theme: &seraphim_render::Theme) -> Option<Bind> {
        let alias = b
            .source
            .as_deref()
            .and_then(|s| seraphim_config::BIND_EVENT_SOURCES.iter().find(|(n, _, _)| *n == s));
        let (input, natural) = match (&b.source, &b.event) {
            (Some(_), _) if alias.is_some() => {
                let (_, event, field) = alias.unwrap();
                (
                    Input::Event {
                        pattern: Pattern::parse(event),
                        name: event.to_string(),
                        field: field.split('.').map(str::to_owned).collect(),
                    },
                    [0.0, 1.0],
                )
            }
            (Some(s), _) => {
                let natural = seraphim_config::BIND_SOURCES.iter().find(|(n, _)| n == s)?.1;
                (Input::Metric(Metric::parse(s)?), natural)
            }
            (None, Some(e)) => (
                Input::Event {
                    pattern: Pattern::parse(e),
                    name: e.clone(),
                    field: b.field.as_deref()?.split('.').map(str::to_owned).collect(),
                },
                [0.0, 1.0],
            ),
            _ => return None,
        };
        let (map, palette, color) = if b.is_color() {
            let mut palette = Vec::new();
            let mut map = BTreeMap::new();
            for (k, c) in &b.colors {
                palette.push(crate::profile::resolve_color(c, theme)?);
                map.insert(k.clone(), (palette.len() - 1) as f32);
            }
            (map, palette, b.param.strip_prefix("color.").map(str::to_owned))
        } else {
            (b.map.clone(), Vec::new(), None)
        };
        Some(Bind {
            input,
            layer: b.layer.clone(),
            param: b.param.clone(),
            range: b.input.unwrap_or(natural),
            output: b.output,
            map,
            palette,
            color,
            smoothing: Duration::from_millis(b.smoothing_ms as u64),
            interval: Duration::from_millis(b.interval_ms as u64),
        })
    }

    /// The parameter value for a source value.
    fn map_number(&self, v: f64) -> Option<f32> {
        if !self.map.is_empty() {
            let key = if v.fract() == 0.0 { format!("{}", v as i64) } else { format!("{v}") };
            return self.map.get(&key).or_else(|| self.map.get("default")).copied();
        }
        let [a, b] = self.range;
        let t = ((v as f32 - a) / (b - a)).clamp(0.0, 1.0);
        let [o0, o1] = self.output;
        Some(o0 + t * (o1 - o0))
    }

    /// For event fields: numbers map as above; strings and booleans only
    /// through `map`.
    fn map_value(&self, v: &Value) -> Option<f32> {
        match v {
            Value::Number(n) => self.map_number(n.as_f64()?),
            Value::Bool(b) if self.map.is_empty() => self.map_number(*b as u8 as f64),
            Value::Bool(b) => {
                self.map.get(&b.to_string()).or_else(|| self.map.get("default")).copied()
            }
            Value::String(s) => self.map.get(s).or_else(|| self.map.get("default")).copied(),
            _ => self.map.get("default").copied(),
        }
    }

    fn apply(&self, visual: &mut VisualEngine, value: f32, instant: bool) -> bool {
        let t = if instant || self.smoothing.is_zero() {
            Transition::Instant
        } else {
            Transition::Tween { duration: self.smoothing.as_secs_f64(), easing: Easing::EaseInOut }
        };
        if !self.palette.is_empty() {
            let Some(rgba) = self.palette.get(value as usize) else { return false };
            let name = match &self.color {
                Some(n) => n.clone(),
                None => match visual.color_names(&self.layer).first() {
                    Some(n) => n.to_string(),
                    None => return false,
                },
            };
            return visual.set_color(&self.layer, &name, *rgba, t);
        }
        if self.param == "opacity" {
            visual.set_opacity(&self.layer, value, t)
        } else {
            visual.set_param(&self.layer, &self.param, value, t)
        }
    }
}

/// Samples metrics and follows events for a set of bindings.
pub struct Bindings {
    binds: Vec<Bind>,
    /// Last value applied per binding (re-applied after a scene rebuild).
    last: Vec<Option<f32>>,
    next_sample: Vec<Instant>,
    subs: Vec<(Pattern, Subscription)>,
    sampler: Sampler,
    session: u64,
    /// Latest event value per (binding, session), to switch sessions.
    latest: std::collections::HashMap<(usize, u64), f32>,
}

impl Bindings {
    pub fn new(binds: Vec<Bind>, bus: &EventBus, session: u64) -> Self {
        let mut patterns: Vec<&str> = binds
            .iter()
            .filter_map(|b| match &b.input {
                Input::Event { name, .. } => Some(name.as_str()),
                Input::Metric(_) => None,
            })
            .collect();
        patterns.sort_unstable();
        patterns.dedup();
        let subs = patterns.iter().map(|p| (Pattern::parse(p), bus.subscribe(*p, 64))).collect();
        let now = Instant::now();
        Bindings {
            last: vec![None; binds.len()],
            next_sample: vec![now; binds.len()],
            binds,
            subs,
            sampler: Sampler::default(),
            session,
            latest: Default::default(),
        }
    }

    /// New bindings after a reload; the metric history (CPU deltas) stays.
    pub fn replace(&mut self, binds: Vec<Bind>, bus: &EventBus) {
        let sampler = std::mem::take(&mut self.sampler);
        let old: Vec<(Bind, Option<f32>)> = self.binds.drain(..).zip(self.last.drain(..)).collect();
        *self = Bindings::new(binds, bus, self.session);
        self.sampler = sampler;
        // The same binding keeps its value (events may not come again).
        for (i, b) in self.binds.iter().enumerate() {
            let same = old.iter().find(|(o, _)| {
                o.input == b.input
                    && o.layer == b.layer
                    && o.param == b.param
                    && o.palette == b.palette
            });
            if let Some((_, v)) = same {
                self.last[i] = *v;
            }
        }
    }

    /// The scene was rebuilt (parameters back to defaults): put the last
    /// values back at once.
    pub fn reapply(&self, visual: &mut VisualEngine) {
        for (b, v) in self.binds.iter().zip(&self.last) {
            if let Some(v) = v {
                b.apply(visual, *v, true);
            }
        }
    }

    /// Sample due metrics and drain events. Returns when to call again, and
    /// whether anything changed.
    pub fn tick(&mut self, now: Instant, visual: &mut VisualEngine) -> (Option<Instant>, bool) {
        let mut changed = false;
        // Events: the latest matching one per binding wins.
        let events: Vec<_> = self.subs.iter().flat_map(|(_, s)| s.drain()).collect();
        let session = self.session;
        let mut updates = Vec::new();
        for e in &events {
            for (i, b) in self.binds.iter().enumerate() {
                if let Input::Event { pattern, field, .. } = &b.input {
                    if !pattern.matches(&e.name) {
                        continue;
                    }
                    let v = field.iter().try_fold(&e.data, |v, k| v.get(k));
                    if let Some(x) = v.and_then(|v| b.map_value(v)) {
                        self.latest.insert((i, e.session.unwrap_or(0)), x);
                        if e.session.is_none_or(|s| s == session) {
                            updates.push((i, x));
                        }
                    }
                }
            }
        }
        for (i, x) in updates {
            changed |= self.set(i, x, visual);
        }
        // Metrics, each on its own interval.
        let mut next: Option<Instant> = None;
        for i in 0..self.binds.len() {
            let Input::Metric(m) = self.binds[i].input else { continue };
            if now >= self.next_sample[i] {
                self.next_sample[i] = now + self.binds[i].interval;
                if let Some(v) = self.sampler.read(m, now) {
                    if let Some(x) = self.binds[i].map_number(v) {
                        changed |= self.set(i, x, visual);
                    }
                }
            }
            next = Some(next.map_or(self.next_sample[i], |n| n.min(self.next_sample[i])));
        }
        (next, changed)
    }

    /// Follow another session's events (the focused pane changed).
    pub fn set_session(&mut self, session: u64, visual: &mut VisualEngine) {
        if session == self.session {
            return;
        }
        self.tick(std::time::Instant::now(), visual);
        self.session = session;
        for i in 0..self.binds.len() {
            if let Some(x) = self.latest.get(&(i, session)).copied() {
                self.set(i, x, visual);
            }
        }
    }

    fn set(&mut self, i: usize, x: f32, visual: &mut VisualEngine) -> bool {
        if self.last[i].is_some_and(|l| (l - x).abs() < 1e-4) {
            return false;
        }
        let first = self.last[i].is_none();
        self.last[i] = Some(x);
        // The first value lands at once: no slow drift in from the default.
        self.binds[i].apply(visual, x, first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(toml: &str) -> Bind {
        let c = seraphim_config::Config::parse(toml, std::path::Path::new("t.toml")).unwrap();
        Bind::from_config(&c.bind[0], &seraphim_render::Theme::default()).unwrap()
    }

    #[test]
    fn mapping() {
        let b = bind(
            "[[bind]]\nsource = \"system.cpu\"\nlayer = \"a\"\nparam = \"p\"\noutput = [1, 3]\n",
        );
        assert_eq!(b.map_number(0.0), Some(1.0));
        assert_eq!(b.map_number(50.0), Some(2.0));
        assert_eq!(b.map_number(250.0), Some(3.0), "clamped");
        let b = bind(
            "[[bind]]\nsource = \"system.load\"\ninput = [2, 0]\nlayer = \"a\"\nparam = \"p\"\n",
        );
        assert_eq!(b.map_number(0.5), Some(0.75), "reversed input range");
        let b = bind("[[bind]]\nevent = \"command.completed\"\nfield = \"exit_code\"\nmap = { \"0\" = 0.2, \"130\" = 0.5, default = 1.0 }\nlayer = \"a\"\nparam = \"p\"\n");
        assert_eq!(b.map_value(&serde_json::json!(0)), Some(0.2));
        assert_eq!(b.map_value(&serde_json::json!(130)), Some(0.5));
        assert_eq!(b.map_value(&serde_json::json!(1)), Some(1.0));
        assert_eq!(b.map_value(&serde_json::json!(null)), Some(1.0), "no exit code: default");
        let b = bind("[[bind]]\nevent = \"session.cwd.changed\"\nfield = \"cwd\"\nmap = { \"/root\" = 1.0 }\nlayer = \"a\"\nparam = \"p\"\n");
        assert_eq!(b.map_value(&serde_json::json!("/root")), Some(1.0));
        assert_eq!(
            b.map_value(&serde_json::json!("/tmp")),
            None,
            "no match, no default: unchanged"
        );
    }
}
