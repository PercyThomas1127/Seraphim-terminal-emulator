//! Continuous bindings (`[[bind]]`): a layer parameter follows a live value
//! — a system metric sampled on an interval, or a field of the latest event
//! of a kind — eased toward each new value.
//!
//! Metrics come from Linux's /proc and /sys; where a source is missing
//! (no battery, another OS) the binding simply leaves the parameter alone.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use seraphim_events::{EventBus, Pattern, Subscription, Value};
use seraphim_visual::{Easing, Transition, VisualEngine};

#[derive(Clone, Debug, PartialEq)]
enum Input {
    Metric(Metric),
    Event { pattern: Pattern, name: String, field: Vec<String> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Cpu,
    Memory,
    Load,
    BatteryLevel,
    BatteryCharging,
    Hour,
}

impl Metric {
    fn parse(s: &str) -> Option<Metric> {
        Some(match s {
            "system.cpu" => Metric::Cpu,
            "system.memory" => Metric::Memory,
            "system.load" => Metric::Load,
            "battery.level" => Metric::BatteryLevel,
            "battery.charging" => Metric::BatteryCharging,
            "time.hour" => Metric::Hour,
            _ => return None,
        })
    }
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
    smoothing: Duration,
    interval: Duration,
}

impl Bind {
    pub fn from_config(b: &seraphim_config::Bind) -> Option<Bind> {
        let (input, natural) = match (&b.source, &b.event) {
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
        Some(Bind {
            input,
            layer: b.layer.clone(),
            param: b.param.clone(),
            range: b.input.unwrap_or(natural),
            output: b.output,
            map: b.map.clone(),
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
            let same = old
                .iter()
                .find(|(o, _)| o.input == b.input && o.layer == b.layer && o.param == b.param);
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
        for e in events.iter().filter(|e| e.session.is_none_or(|s| s == session)) {
            for (i, b) in self.binds.iter().enumerate() {
                if let Input::Event { pattern, field, .. } = &b.input {
                    if !pattern.matches(&e.name) {
                        continue;
                    }
                    let v = field.iter().try_fold(&e.data, |v, k| v.get(k));
                    if let Some(x) = v.and_then(|v| b.map_value(v)) {
                        updates.push((i, x));
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

/// Reads metrics; keeps what CPU usage needs between samples.
#[derive(Debug, Default)]
pub struct Sampler {
    cpu: Option<(u64, u64)>,
    /// Cached per instant, so several bindings share one read.
    cache: Vec<(Metric, Instant, Option<f64>)>,
}

impl Sampler {
    pub fn read(&mut self, m: Metric, now: Instant) -> Option<f64> {
        if let Some((_, _, v)) = self
            .cache
            .iter()
            .find(|(k, t, _)| *k == m && now.duration_since(*t) < Duration::from_millis(150))
        {
            return *v;
        }
        let v = match m {
            Metric::Cpu => {
                let (busy, total) = cpu_times(&std::fs::read_to_string("/proc/stat").ok()?)?;
                let prev = self.cpu.replace((busy, total));
                prev.and_then(|(pb, pt)| cpu_percent((pb, pt), (busy, total)))
            }
            Metric::Memory => memory_percent(&std::fs::read_to_string("/proc/meminfo").ok()?),
            Metric::Load => {
                let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
                load_per_core(&std::fs::read_to_string("/proc/loadavg").ok()?, cores)
            }
            Metric::BatteryLevel => battery().map(|(level, _)| level),
            Metric::BatteryCharging => battery().map(|(_, c)| c as u8 as f64),
            Metric::Hour => local_hour(),
        };
        self.cache.retain(|(k, _, _)| *k != m);
        self.cache.push((m, now, v));
        v
    }
}

/// (busy, total) jiffies from the first line of /proc/stat.
pub fn cpu_times(stat: &str) -> Option<(u64, u64)> {
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = line.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
    if v.len() < 4 {
        return None;
    }
    // user nice system idle iowait irq softirq steal (guest is in user).
    let total: u64 = v.iter().take(8).sum();
    let idle = v[3] + v.get(4).copied().unwrap_or(0);
    Some((total - idle, total))
}

pub fn cpu_percent(prev: (u64, u64), now: (u64, u64)) -> Option<f64> {
    let dt = now.1.checked_sub(prev.1)?;
    let db = now.0.checked_sub(prev.0)?;
    (dt > 0).then(|| 100.0 * db as f64 / dt as f64)
}

pub fn memory_percent(meminfo: &str) -> Option<f64> {
    let field = |name: &str| -> Option<f64> {
        let l = meminfo.lines().find(|l| l.starts_with(name))?;
        l.split_whitespace().nth(1)?.parse().ok()
    };
    let (total, avail) = (field("MemTotal:")?, field("MemAvailable:")?);
    (total > 0.0).then(|| 100.0 * (1.0 - avail / total))
}

pub fn load_per_core(loadavg: &str, cores: usize) -> Option<f64> {
    let one: f64 = loadavg.split_whitespace().next()?.parse().ok()?;
    Some(one / cores.max(1) as f64)
}

/// (level percent, charging) of the first battery.
fn battery() -> Option<(f64, bool)> {
    for e in std::fs::read_dir("/sys/class/power_supply").ok()?.flatten() {
        let p = e.path();
        let kind = std::fs::read_to_string(p.join("type")).unwrap_or_default();
        if kind.trim() != "Battery" {
            continue;
        }
        let level = std::fs::read_to_string(p.join("capacity")).ok()?.trim().parse().ok()?;
        let status = std::fs::read_to_string(p.join("status")).unwrap_or_default();
        return Some((level, status.trim() == "Charging" || status.trim() == "Full"));
    }
    None
}

/// Local time of day in hours (0-24, fractional).
fn local_hour() -> Option<f64> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?;
    let t = now.as_secs() as libc::time_t;
    // SAFETY: localtime_r writes into the zeroed struct we own.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return None;
    }
    Some(tm.tm_hour as f64 + tm.tm_min as f64 / 60.0 + tm.tm_sec as f64 / 3600.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(toml: &str) -> Bind {
        let c = seraphim_config::Config::parse(toml, std::path::Path::new("t.toml")).unwrap();
        Bind::from_config(&c.bind[0]).unwrap()
    }

    #[test]
    fn parsing_proc_files() {
        let a = cpu_times("cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 1 2 3 4\n").unwrap();
        assert_eq!(a, (150, 1000));
        let b = cpu_times("cpu  200 0 100 900 50 0 0 0 0 0\n").unwrap();
        assert_eq!(cpu_percent(a, b), Some(60.0), "150 more busy of 250 more jiffies");
        assert_eq!(cpu_percent(b, b), None, "no time passed");
        let mem = "MemTotal:       16000000 kB\nMemFree:  1 kB\nMemAvailable:    4000000 kB\n";
        assert_eq!(memory_percent(mem), Some(75.0));
        assert_eq!(load_per_core("2.00 1.50 1.00 2/300 1234\n", 4), Some(0.5));
        assert!(local_hour().is_some_and(|h| (0.0..24.0).contains(&h)));
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

    #[test]
    fn sampler_caches_and_needs_two_cpu_samples() {
        let mut s = Sampler::default();
        let t = Instant::now();
        if std::path::Path::new("/proc/stat").exists() {
            assert_eq!(s.read(Metric::Cpu, t), None, "first sample has no delta");
            std::thread::sleep(Duration::from_millis(30));
            let later = t + Duration::from_secs(1);
            let v = s.read(Metric::Cpu, later);
            assert!(v.is_none_or(|v| (0.0..=100.0).contains(&v)), "{v:?}");
            assert!(s.read(Metric::Memory, later).is_some_and(|v| (0.0..=100.0).contains(&v)));
        }
    }
}
