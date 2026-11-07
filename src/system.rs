//! Zustand des Raspberry Pi: CPU, RAM, Temperatur, Speicherplatz.

use serde_json::{json, Value};
use std::{path::Path, sync::Mutex, time::Duration};

#[derive(Default)]
pub struct Sampler {
    last: Mutex<Option<(u64, u64)>>,
    cpu: Mutex<Option<f64>>,
}

impl Sampler {
    /// Alle 2 Sekunden die CPU-Last messen.
    pub fn spawn(self: std::sync::Arc<Self>) {
        tokio::spawn(async move {
            loop {
                if let Some((busy, total)) = read_cpu() {
                    let mut last = self.last.lock().unwrap();
                    if let Some((b0, t0)) = *last {
                        let dt = total.saturating_sub(t0);
                        if dt > 0 {
                            let pct = busy.saturating_sub(b0) as f64 / dt as f64 * 100.0;
                            *self.cpu.lock().unwrap() = Some((pct * 10.0).round() / 10.0);
                        }
                    }
                    *last = Some((busy, total));
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
    }

    pub fn snapshot(&self, data_dir: &Path) -> Value {
        let mem = read_meminfo();
        let disk = disk_space(data_dir);
        json!({
            "model": read_trim("/proc/device-tree/model"),
            "hostname": read_trim("/proc/sys/kernel/hostname"),
            "cpu_percent": *self.cpu.lock().unwrap(),
            "cores": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
            "load": read_trim("/proc/loadavg").map(|l| l.split_whitespace().take(3).collect::<Vec<_>>().join(" ")),
            "temp_c": read_temp(),
            "cpu_mhz": read_trim("/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq")
                .and_then(|s| s.parse::<u64>().ok()).map(|k| k / 1000),
            "mem_total": mem.map(|m| m.0),
            "mem_available": mem.map(|m| m.1),
            "disk_total": disk.map(|d| d.0),
            "disk_free": disk.map(|d| d.1),
            "uptime_seconds": read_trim("/proc/uptime")
                .and_then(|s| s.split_whitespace().next().and_then(|n| n.parse::<f64>().ok()))
                .map(|s| s as u64),
        })
    }
}

fn read_trim(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim_matches(|c: char| c.is_whitespace() || c == '\0').to_owned())
        .filter(|s| !s.is_empty())
}

/// (beschäftigt, gesamt) in Jiffies, aus der ersten Zeile von /proc/stat.
fn read_cpu() -> Option<(u64, u64)> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    parse_cpu(stat.lines().next()?)
}

pub fn parse_cpu(line: &str) -> Option<(u64, u64)> {
    let nums: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|n| n.parse().ok())
        .collect();
    if nums.len() < 4 {
        return None;
    }
    let total: u64 = nums.iter().take(8).sum();
    let idle = nums[3] + nums.get(4).copied().unwrap_or(0);
    Some((total - idle, total))
}

/// (gesamt, verfügbar) in Bytes.
fn read_meminfo() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let get = |key: &str| {
        text.lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|n| n.parse::<u64>().ok())
            .map(|kb| kb * 1024)
    };
    Some((get("MemTotal:")?, get("MemAvailable:")?))
}

fn read_temp() -> Option<f64> {
    for zone in 0..4 {
        let path = format!("/sys/class/thermal/thermal_zone{zone}/temp");
        if let Some(milli) = read_trim(&path).and_then(|s| s.parse::<f64>().ok()) {
            return Some((milli / 100.0).round() / 10.0);
        }
    }
    None
}

/// (gesamt, frei) in Bytes für das Dateisystem des Datenordners.
fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let frsize = s.f_frsize as u64;
    Some((s.f_blocks as u64 * frsize, s.f_bavail as u64 * frsize))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_line() {
        let (busy, total) = parse_cpu("cpu  100 0 50 800 50 0 0 0 0 0").unwrap();
        assert_eq!(total, 1000);
        assert_eq!(busy, 150);
    }
}
