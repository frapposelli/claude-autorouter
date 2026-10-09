//! External process measurements; no runtime-specific in-process heap proxies.
use crate::process::capture;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

fn output(program: &str, args: &[&str]) -> Option<String> {
    capture(
        Command::new(program).args(args),
        b"",
        Duration::from_secs(5),
    )
    .ok()
    .and_then(|bytes| String::from_utf8(bytes).ok())
    .map(|s| s.trim().to_owned())
}
pub fn load() -> Value {
    if cfg!(target_os = "macos") {
        json!(output("/usr/sbin/sysctl", &["-n", "vm.loadavg"]))
    } else {
        json!(
            std::fs::read_to_string("/proc/loadavg").ok().map(|s| s
                .split_whitespace()
                .take(3)
                .collect::<Vec<_>>()
                .join(" "))
        )
    }
}
pub fn environment(root: &Path) -> Value {
    let (cpu, memory, power) = if cfg!(target_os = "macos") {
        (
            output("/usr/sbin/sysctl", &["-n", "machdep.cpu.brand_string"]),
            output("/usr/sbin/sysctl", &["-n", "hw.memsize"]).and_then(|s| s.parse::<u64>().ok()),
            output("/usr/bin/pmset", &["-g", "custom"]),
        )
    } else {
        let cpu = std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find(|line| line.starts_with("model name") || line.starts_with("Hardware"))
                    .and_then(|v| v.split_once(':'))
                    .map(|(_, v)| v.trim().to_owned())
            });
        let memory = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find(|line| line.starts_with("MemTotal:"))
                    .and_then(|v| v.split_whitespace().nth(1))
                    .and_then(|v| v.parse::<u64>().ok())
            })
            .map(|v| v * 1024);
        (cpu, memory, None)
    };
    let commit = capture(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root),
        b"",
        Duration::from_secs(5),
    )
    .ok()
    .and_then(|v| String::from_utf8(v).ok())
    .map(|v| v.trim().to_owned());
    json!({"os":std::env::consts::OS,"architecture":std::env::consts::ARCH,"os_release":output("/usr/bin/uname", &["-r"]),"cpu":cpu,"memory_bytes":memory,"logical_cpus":std::thread::available_parallelism().map(usize::from).ok(),"node":output("node", &["--version"]),"rustc":output("rustc", &["--version","--verbose"]),"source_commit":commit,"build_profile":"release requested; binary hash recorded; source-to-binary provenance not verified","power_settings":power,"power_mode_qualification":"uncontrolled developer workstation; representative-hardware acceptance unavailable","background_load":load(),"driver_worker_threads":2,"candidate_worker_configuration":"Native serve uses one current-thread Tokio executor; Node uses one JavaScript event-loop thread; both may use runtime background workers. Driver uses two Tokio workers.","cpu_time_resolution_ms":if cfg!(target_os="macos"){10}else{1000},"rss_method":"ps whole-process RSS KiB sampled every100ms; sampled peak is a lower bound","process_names_collected":false})
}
pub fn cpu_seconds(text: &str) -> Option<f64> {
    let (days, text) = match text.split_once('-') {
        Some((d, t)) => (d.parse::<f64>().ok()?, t),
        None => (0.0, text),
    };
    let fields = text
        .split(':')
        .map(str::parse::<f64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let seconds = match fields.as_slice() {
        [m, s] => m * 60.0 + s,
        [h, m, s] => h * 3600.0 + m * 60.0 + s,
        _ => return None,
    };
    Some(days * 86400.0 + seconds)
}
pub async fn process(pid: u32) -> Value {
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::process::Command::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "rss=", "-o", "time="])
            .output(),
    )
    .await;
    let Ok(Ok(result)) = result else {
        return json!({"rss_bytes":null,"cpu_seconds":null});
    };
    if !result.status.success() {
        return json!({"rss_bytes":null,"cpu_seconds":null});
    }
    let text = String::from_utf8_lossy(&result.stdout);
    let parts = text.split_whitespace().collect::<Vec<_>>();
    json!({"rss_bytes":parts.first().and_then(|v|v.parse::<u64>().ok()).map(|v|v*1024),"cpu_seconds":parts.get(1).and_then(|v|cpu_seconds(v))})
}
pub fn percentile(values: &[f64], probability: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[((sorted.len() as f64 * probability).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1)]
}
pub fn stats(values: &[f64]) -> Value {
    json!({"samples":values.len(),"p50":percentile(values,0.5),"p95":percentile(values,0.95),"p99":percentile(values,0.99),"max":values.iter().copied().fold(0.0,f64::max)})
}
pub fn interval(values: &[f64]) -> Value {
    if values.len() < 5 {
        return json!({"median":null,"low":null,"high":null});
    }
    let mut state = 1597463007u64;
    let mut medians = Vec::with_capacity(5000);
    for _ in 0..5000 {
        let mut sample = Vec::with_capacity(values.len());
        for _ in values {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            sample.push(values[(state >> 32) as usize % values.len()]);
        }
        medians.push(percentile(&sample, 0.5));
    }
    json!({"median":percentile(values,0.5),"low":percentile(&medians,0.025),"high":percentile(&medians,0.975)})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_ps_day_hour_and_fraction_units() {
        assert_eq!(cpu_seconds("01:02.34"), Some(62.34));
        assert_eq!(cpu_seconds("2-03:04:05"), Some(183845.0));
        assert_eq!(cpu_seconds("bad"), None);
    }
    #[test]
    fn tail_is_nearest_rank_and_small_rounds_are_not_certified() {
        let v = (1..=2000).map(|v| v as f64).collect::<Vec<_>>();
        assert_eq!(percentile(&v, 0.99), 1980.0);
        assert!(interval(&[1.0, 2.0])["high"].is_null());
        assert_eq!(interval(&[2.0; 5])["low"], 2.0);
    }
}
