//! System metrics read from Linux's /proc and /sys, shared by `[[bind]]`
//! bindings and the Lua `system` API. Missing sources (no battery, another
//! OS) read as `None`.

use std::time::{Duration, Instant};

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
    pub fn parse(s: &str) -> Option<Metric> {
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
