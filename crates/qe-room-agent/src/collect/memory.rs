//! 内存采集：`/proc/meminfo`。

use std::fs;

/// 内置采集到的内存信息。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemorySnapshot {
    /// 总内存（字节）。
    pub total_bytes: u64,
    /// 可用内存（字节）；优先 `MemAvailable`，回退 `MemFree`。
    pub available_bytes: u64,
    /// 已用内存（字节）= 总量 - 可用。
    pub used_bytes: u64,
    /// 使用率（0–100）。
    pub usage_percent: f64,
}

/// 解析 `/proc/meminfo`；缺少 `MemTotal` 时返回 None。
pub fn parse_meminfo(content: &str) -> Option<MemorySnapshot> {
    let mut total_kib: Option<u64> = None;
    let mut available_kib: Option<u64> = None;
    let mut free_kib: Option<u64> = None;

    for line in content.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        // 值形如 `15934537728 kB`；只用第一段数字。
        let Some(amount) = value
            .split_whitespace()
            .next()
            .and_then(|raw| raw.parse::<u64>().ok())
        else {
            continue;
        };
        match key.trim() {
            "MemTotal" => total_kib = Some(amount),
            "MemAvailable" => available_kib = Some(amount),
            "MemFree" => free_kib = Some(amount),
            _ => {}
        }
    }

    let total_bytes = total_kib?.saturating_mul(1024);
    if total_bytes == 0 {
        return None;
    }
    let available_bytes = available_kib
        .or(free_kib)
        .unwrap_or(0)
        .saturating_mul(1024)
        .min(total_bytes);
    let used_bytes = total_bytes.saturating_sub(available_bytes);
    let usage_percent = used_bytes as f64 / total_bytes as f64 * 100.0;
    Some(MemorySnapshot {
        total_bytes,
        available_bytes,
        used_bytes,
        usage_percent,
    })
}

/// 采集本机内存，失败返回 None。
pub fn collect() -> Option<MemorySnapshot> {
    fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|content| parse_meminfo(&content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_meminfo_with_available() {
        let content = "\
MemTotal:       15561072 kB
MemFree:         1065708 kB
MemAvailable:    7410352 kB
Buffers:          123456 kB
";
        let snapshot = parse_meminfo(content).unwrap();
        assert_eq!(snapshot.total_bytes, 15_561_072 * 1024);
        assert_eq!(snapshot.available_bytes, 7_410_352 * 1024);
        assert_eq!(snapshot.used_bytes, (15_561_072 - 7_410_352) * 1024);
        let expected = (15_561_072_u64 - 7_410_352) as f64 / 15_561_072_f64 * 100.0;
        assert!((snapshot.usage_percent - expected).abs() < 1e-9);
    }

    #[test]
    fn available_falls_back_to_free() {
        let content = "MemTotal: 1000 kB\nMemFree: 250 kB\n";
        let snapshot = parse_meminfo(content).unwrap();
        assert_eq!(snapshot.available_bytes, 250 * 1024);
        assert_eq!(snapshot.usage_percent, 75.0);
    }

    #[test]
    fn missing_total_is_rejected() {
        assert!(parse_meminfo("MemFree: 250 kB\n").is_none());
        assert!(parse_meminfo("MemTotal: 0 kB\n").is_none());
        assert!(parse_meminfo("garbage").is_none());
    }

    #[test]
    fn available_is_clamped_to_total() {
        let content = "MemTotal: 1000 kB\nMemAvailable: 4000 kB\n";
        let snapshot = parse_meminfo(content).unwrap();
        assert_eq!(snapshot.available_bytes, 1000 * 1024);
        assert_eq!(snapshot.usage_percent, 0.0);
    }
}
