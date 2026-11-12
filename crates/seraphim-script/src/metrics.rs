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
    /// GPU busy, percent.
    Gpu,
    /// GPU memory in use, percent.
    GpuMemory,
    /// GPU temperature, °C.
    GpuTemp,
    /// CPU temperature, °C.
    CpuTemp,
    /// Network receive and send rates, KiB/s (all interfaces but loopback).
    NetRx,
    NetTx,
    /// 1 with a default route, else 0.
    NetOnline,
    /// Disk read and write rates, KiB/s (whole disks).
    DiskRead,
    DiskWrite,
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
            "system.gpu" => Metric::Gpu,
            "system.gpu.memory" => Metric::GpuMemory,
            "system.gpu.temp" => Metric::GpuTemp,
            "system.temp" => Metric::CpuTemp,
            "network.rx" => Metric::NetRx,
            "network.tx" => Metric::NetTx,
            "network.online" => Metric::NetOnline,
            "disk.read" => Metric::DiskRead,
            "disk.write" => Metric::DiskWrite,
            _ => return None,
        })
    }
}

/// Reads metrics; keeps what CPU usage needs between samples.
#[derive(Debug, Default)]
pub struct Sampler {
    cpu: Option<(u64, u64)>,
    /// Byte counters at the last sample, for rates.
    net: Option<(u64, u64, Instant)>,
    disk: Option<(u64, u64, Instant)>,
    /// Intel: GPU idle time at the last sample (ms).
    gpu_idle: Option<(u64, Instant)>,
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
            Metric::Gpu | Metric::GpuMemory | Metric::GpuTemp => {
                let g = self.gpu(now);
                self.put(Metric::Gpu, now, g.busy);
                self.put(Metric::GpuMemory, now, g.memory);
                self.put(Metric::GpuTemp, now, g.temp);
                match m {
                    Metric::Gpu => g.busy,
                    Metric::GpuMemory => g.memory,
                    _ => g.temp,
                }
            }
            Metric::CpuTemp => cpu_temp(),
            Metric::NetRx | Metric::NetTx => {
                let now_bytes = net_bytes(&std::fs::read_to_string("/proc/net/dev").ok()?)?;
                let r = rates(&mut self.net, now_bytes, now);
                self.put(Metric::NetRx, now, r.map(|r| r.0));
                self.put(Metric::NetTx, now, r.map(|r| r.1));
                r.map(|r| if m == Metric::NetRx { r.0 } else { r.1 })
            }
            Metric::NetOnline => {
                let v4 = std::fs::read_to_string("/proc/net/route").unwrap_or_default();
                let v6 = std::fs::read_to_string("/proc/net/ipv6_route").unwrap_or_default();
                Some(default_route(&v4, &v6) as u8 as f64)
            }
            Metric::DiskRead | Metric::DiskWrite => {
                let disks = whole_disks();
                let stats = std::fs::read_to_string("/proc/diskstats").ok()?;
                let r = rates(&mut self.disk, disk_bytes(&stats, &disks), now);
                self.put(Metric::DiskRead, now, r.map(|r| r.0));
                self.put(Metric::DiskWrite, now, r.map(|r| r.1));
                r.map(|r| if m == Metric::DiskRead { r.0 } else { r.1 })
            }
        };
        self.put(m, now, v);
        v
    }

    fn put(&mut self, m: Metric, now: Instant, v: Option<f64>) {
        self.cache.retain(|(k, _, _)| *k != m);
        self.cache.push((m, now, v));
    }

    /// The first GPU's readings: AMD and Intel from sysfs, NVIDIA from
    /// `nvidia-smi` (running in the background, once per process).
    pub fn gpu(&mut self, now: Instant) -> Gpu {
        for card in drm_cards() {
            let dev = card.join("device");
            let read = |p: &std::path::Path| -> Option<f64> {
                std::fs::read_to_string(p).ok()?.trim().parse().ok()
            };
            let temp = std::fs::read_dir(dev.join("hwmon"))
                .ok()
                .and_then(|mut d| d.next())
                .and_then(|e| e.ok())
                .and_then(|e| read(&e.path().join("temp1_input")))
                .map(|t| t / 1000.0);
            // AMD.
            if let Some(busy) = read(&dev.join("gpu_busy_percent")) {
                let memory = match (
                    read(&dev.join("mem_info_vram_used")),
                    read(&dev.join("mem_info_vram_total")),
                ) {
                    (Some(u), Some(t)) if t > 0.0 => Some(100.0 * u / t),
                    _ => None,
                };
                return Gpu { busy: Some(busy), memory, temp };
            }
            // Intel: busy is what isn't idle (i915 RC6, or xe's gtidle).
            let idle = [
                card.join("gt/gt0/rc6_residency_ms"),
                card.join("power/rc6_residency_ms"),
                dev.join("tile0/gt0/gtidle/idle_residency_ms"),
            ]
            .iter()
            .find_map(|p| read(p));
            if let Some(idle) = idle {
                let idle = idle as u64;
                let busy = self.gpu_idle.replace((idle, now)).and_then(|(prev, t)| {
                    let dt = now.duration_since(t).as_millis() as f64;
                    (dt > 0.0).then(|| {
                        (100.0 * (1.0 - (idle.saturating_sub(prev)) as f64 / dt)).clamp(0.0, 100.0)
                    })
                });
                return Gpu { busy, memory: None, temp };
            }
        }
        if std::path::Path::new("/proc/driver/nvidia").exists() {
            return nvidia();
        }
        Gpu::default()
    }
}

/// One GPU's readings; `None` where the driver doesn't say.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Gpu {
    pub busy: Option<f64>,
    pub memory: Option<f64>,
    pub temp: Option<f64>,
}

fn drm_cards() -> Vec<std::path::PathBuf> {
    let Ok(dir) = std::fs::read_dir("/sys/class/drm") else { return Vec::new() };
    let mut cards: Vec<_> = dir
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.strip_prefix("card")
                .is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit()))
        })
        .map(|e| e.path())
        .collect();
    cards.sort();
    cards
}

/// NVIDIA: `nvidia-smi` prints a line a second in the background; the
/// latest one is read.
fn nvidia() -> Gpu {
    use std::sync::{Mutex, OnceLock};
    static LATEST: OnceLock<std::sync::Arc<Mutex<Gpu>>> = OnceLock::new();
    let latest = LATEST.get_or_init(|| {
        let latest = std::sync::Arc::new(Mutex::new(Gpu::default()));
        let out = latest.clone();
        let child = std::process::Command::new("nvidia-smi")
            .args([
                "--query-gpu=utilization.gpu,memory.used,memory.total,temperature.gpu",
                "--format=csv,noheader,nounits",
                "-lms",
                "1000",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Ok(mut child) = child {
            let stdout = child.stdout.take();
            let _ =
                std::thread::Builder::new().name("seraphim-nvidia-smi".into()).spawn(move || {
                    use std::io::BufRead;
                    let Some(stdout) = stdout else { return };
                    for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                        // The first GPU only.
                        if let Some(g) = parse_nvidia_smi(&line) {
                            *out.lock().unwrap() = g;
                        }
                    }
                    let _ = child.wait();
                });
        }
        latest
    });
    *latest.lock().unwrap()
}

/// `45, 1024, 8192, 61` (utilization %, memory used and total MiB, °C).
pub fn parse_nvidia_smi(line: &str) -> Option<Gpu> {
    let v: Vec<Option<f64>> = line.split(',').map(|x| x.trim().parse().ok()).collect();
    if v.len() < 4 {
        return None;
    }
    let memory = match (v[1], v[2]) {
        (Some(u), Some(t)) if t > 0.0 => Some(100.0 * u / t),
        _ => None,
    };
    Some(Gpu { busy: v[0], memory, temp: v[3] })
}

/// Received and sent bytes over all interfaces but loopback.
pub fn net_bytes(dev: &str) -> Option<(u64, u64)> {
    let mut rx = 0;
    let mut tx = 0;
    let mut any = false;
    for line in dev.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else { continue };
        if name.trim() == "lo" {
            continue;
        }
        let v: Vec<u64> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        if v.len() >= 9 {
            rx += v[0];
            tx += v[8];
            any = true;
        }
    }
    any.then_some((rx, tx))
}

/// Whole disks (not partitions, loop, RAM or zram devices).
fn whole_disks() -> Vec<String> {
    let Ok(dir) = std::fs::read_dir("/sys/block") else { return Vec::new() };
    dir.flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !["loop", "ram", "zram", "dm-", "sr", "fd"].iter().any(|p| n.starts_with(p)))
        .collect()
}

/// Bytes read and written by these disks (`/proc/diskstats`: sectors of
/// 512 bytes in fields 6 and 10).
pub fn disk_bytes(stats: &str, disks: &[String]) -> (u64, u64) {
    let mut r = 0;
    let mut w = 0;
    for line in stats.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 10 || !disks.iter().any(|d| d == f[2]) {
            continue;
        }
        r += f[5].parse::<u64>().unwrap_or(0) * 512;
        w += f[9].parse::<u64>().unwrap_or(0) * 512;
    }
    (r, w)
}

/// KiB/s since the last sample of these counters.
fn rates(
    prev: &mut Option<(u64, u64, Instant)>,
    now_bytes: (u64, u64),
    now: Instant,
) -> Option<(f64, f64)> {
    let (a, b, t) = prev.replace((now_bytes.0, now_bytes.1, now))?;
    let dt = now.duration_since(t).as_secs_f64();
    (dt > 0.0).then(|| {
        (
            now_bytes.0.saturating_sub(a) as f64 / 1024.0 / dt,
            now_bytes.1.saturating_sub(b) as f64 / 1024.0 / dt,
        )
    })
}

/// A default route (IPv4 or IPv6): the machine is online.
pub fn default_route(route: &str, ipv6_route: &str) -> bool {
    let v4 = route.lines().skip(1).any(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        f.len() > 7 && f[1] == "00000000" && f[7] == "00000000"
    });
    let v6 = ipv6_route.lines().any(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        f.len() > 9 && f[0] == "00000000000000000000000000000000" && f[1] == "00" && f[9] != "lo"
    });
    v4 || v6
}

/// The CPU's temperature: the hwmon sensor of a CPU driver, else the
/// first thermal zone.
fn cpu_temp() -> Option<f64> {
    let read = |p: std::path::PathBuf| -> Option<f64> {
        std::fs::read_to_string(p).ok()?.trim().parse::<f64>().ok().map(|t| t / 1000.0)
    };
    if let Ok(dir) = std::fs::read_dir("/sys/class/hwmon") {
        for e in dir.flatten() {
            let name = std::fs::read_to_string(e.path().join("name")).unwrap_or_default();
            if ["k10temp", "coretemp", "zenpower", "cpu_thermal", "cpu-thermal"]
                .contains(&name.trim())
            {
                if let Some(t) = read(e.path().join("temp1_input")) {
                    return Some(t);
                }
            }
        }
    }
    read("/sys/class/thermal/thermal_zone0/temp".into())
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
    fn parsing_gpu_network_and_disk() {
        let g = parse_nvidia_smi("45, 2048, 8192, 61").unwrap();
        assert_eq!(g, Gpu { busy: Some(45.0), memory: Some(25.0), temp: Some(61.0) });
        assert_eq!(parse_nvidia_smi("[N/A], 10, 100, 40").unwrap().busy, None);
        assert_eq!(parse_nvidia_smi("garbage"), None);
        let dev = "Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 9999999     100    0    0    0     0          0         0  9999999     100    0    0    0     0       0          0
  eth0: 1000     10    0    0    0     0          0         0  2000     20    0    0    0     0       0          0
 wlan0:  500      5    0    0    0     0          0         0   300      3    0    0    0     0       0          0
";
        assert_eq!(net_bytes(dev), Some((1500, 2300)), "loopback left out");
        let stats = "   8       0 sda 100 0 2048 0 50 0 4096 0 0 0 0
   8       1 sda1 100 0 2048 0 50 0 4096 0 0 0 0
   7       0 loop0 1 0 999 0 0 0 0 0 0 0 0
 259       0 nvme0n1 1 0 10 0 1 0 20 0 0 0 0
";
        let disks = vec!["sda".to_owned(), "nvme0n1".to_owned()];
        assert_eq!(disk_bytes(stats, &disks), ((2048 + 10) * 512, (4096 + 20) * 512));
        let mut prev = None;
        let t = Instant::now();
        assert_eq!(rates(&mut prev, (0, 0), t), None);
        assert_eq!(rates(&mut prev, (2048, 1024), t + Duration::from_secs(2)), Some((1.0, 0.5)));
        let route = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\n                     eth0\t00000000\t0102A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n";
        assert!(default_route(&route.replace("\\t", "\t").replace("\\n", "\n"), ""));
        assert!(!default_route(
            "Iface Destination Gateway\neth0 0002A8C0 00000000 0001 0 0 0 00FFFFFF 0 0 0\n",
            ""
        ));
        let v6 = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000400 00000001 00000000 00000003 wlan0\n";
        assert!(default_route("", v6));
        assert_eq!(Metric::parse("system.gpu"), Some(Metric::Gpu));
        assert_eq!(Metric::parse("network.rx"), Some(Metric::NetRx));
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
