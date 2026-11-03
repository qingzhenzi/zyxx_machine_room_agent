//! CPU 采集：`/proc/stat` 500ms 双采样 + `/proc/loadavg` + `/proc/cpuinfo`。

use std::collections::HashSet;
use std::fs;
use std::thread;
use std::time::Duration;

/// CPU 使用率采样窗口。
pub const SAMPLE_WINDOW: Duration = Duration::from_millis(500);

/// 一次 `/proc/stat` 的 CPU 累计时间片（单位：jiffies）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTimes {
    user: u64,
    nice: u64,
    system: u64,
    idle: u64,
    iowait: u64,
    irq: u64,
    softirq: u64,
    steal: u64,
}

impl CpuTimes {
    /// 从完整 `/proc/stat` 内容解析汇总行（首行 `cpu ...`，不含 `cpu0` 等）。
    pub fn parse_stat(content: &str) -> Option<Self> {
        content
            .lines()
            .find(|line| !line.trim().is_empty())
            .and_then(Self::parse_line)
    }

    /// 解析单行 `cpu  user nice system idle iowait irq softirq steal ...`。
    pub fn parse_line(line: &str) -> Option<Self> {
        let mut fields = line.split_whitespace();
        if fields.next()? != "cpu" {
            return None;
        }
        let mut values = [0u64; 8];
        for slot in &mut values {
            // 老内核字段更少：缺失字段按 0 处理。
            *slot = fields
                .next()
                .and_then(|raw| raw.parse::<u64>().ok())
                .unwrap_or(0);
        }
        Some(Self {
            user: values[0],
            nice: values[1],
            system: values[2],
            idle: values[3],
            iowait: values[4],
            irq: values[5],
            softirq: values[6],
            steal: values[7],
        })
    }

    fn total(&self) -> u64 {
        self.user
            .saturating_add(self.nice)
            .saturating_add(self.system)
            .saturating_add(self.idle)
            .saturating_add(self.iowait)
            .saturating_add(self.irq)
            .saturating_add(self.softirq)
            .saturating_add(self.steal)
    }

    fn idle_all(&self) -> u64 {
        self.idle.saturating_add(self.iowait)
    }
}

/// 根据两次采样计算 CPU 使用率（0–100）。时间片无变化时返回 None。
pub fn usage_percent(previous: &CpuTimes, current: &CpuTimes) -> Option<f64> {
    let total_delta = current.total().checked_sub(previous.total())?;
    let idle_delta = current.idle_all().checked_sub(previous.idle_all())?;
    if total_delta == 0 {
        return None;
    }
    let busy = total_delta.saturating_sub(idle_delta);
    Some((busy as f64 / total_delta as f64 * 100.0).clamp(0.0, 100.0))
}

/// `/proc/cpuinfo` 解析结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CpuInfo {
    /// CPU 型号（如 `AMD Ryzen 9 6900HX ...`）。
    pub model: Option<String>,
    /// 物理核心数（按 `(physical id, core id)` 去重）。
    pub physical_cores: Option<u32>,
    /// 逻辑核心数（`processor` 行数）。
    pub logical_cores: Option<u32>,
}

/// 解析 `/proc/cpuinfo`。
pub fn parse_cpuinfo(content: &str) -> CpuInfo {
    let mut model: Option<String> = None;
    let mut logical_cores = 0u32;
    let mut fallback_cores: Option<u32> = None;
    let mut pairs: HashSet<(String, String)> = HashSet::new();
    let mut pending_physical: Option<String> = None;
    let mut pending_core: Option<String> = None;

    let flush_pair = |physical: &mut Option<String>,
                      core: &mut Option<String>,
                      pairs: &mut HashSet<(String, String)>| {
        if let (Some(physical), Some(core)) = (physical.take(), core.take()) {
            pairs.insert((physical, core));
        }
    };

    for line in content.lines() {
        if line.trim().is_empty() {
            flush_pair(&mut pending_physical, &mut pending_core, &mut pairs);
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "processor" => logical_cores += 1,
            "model name" | "Processor" | "cpu model" => {
                if model.is_none() && !value.is_empty() {
                    model = Some(value.to_owned());
                }
            }
            "physical id" => pending_physical = Some(value.to_owned()),
            "core id" => pending_core = Some(value.to_owned()),
            "cpu cores" => {
                if let Ok(cores) = value.parse::<u32>() {
                    fallback_cores = Some(fallback_cores.map_or(cores, |old| old.max(cores)));
                }
            }
            _ => {}
        }
    }
    flush_pair(&mut pending_physical, &mut pending_core, &mut pairs);

    let physical_cores = if pairs.is_empty() {
        match (fallback_cores, logical_cores) {
            // `cpu cores` 是每 socket 的核心数；没有 physical id 时用它，
            // 但不能超过逻辑核心数（测试机/虚拟机的合成 cpuinfo 可能不一致）。
            (Some(cores), 0) => Some(cores),
            (Some(cores), logical) => Some(cores.min(logical)),
            (None, 0) => None,
            (None, logical) => Some(logical),
        }
    } else {
        Some(pairs.len() as u32)
    };

    CpuInfo {
        model,
        physical_cores,
        logical_cores: (logical_cores > 0).then_some(logical_cores),
    }
}

/// 解析 `/proc/loadavg` 的前三个值（1/5/15 分钟平均负载）。
pub fn parse_loadavg(content: &str) -> Option<(f64, f64, f64)> {
    let mut fields = content.split_whitespace();
    let one = fields.next()?.parse::<f64>().ok()?;
    let five = fields.next()?.parse::<f64>().ok()?;
    let fifteen = fields.next()?.parse::<f64>().ok()?;
    if [one, five, fifteen]
        .iter()
        .any(|value| !value.is_finite() || *value < 0.0)
    {
        return None;
    }
    Some((one, five, fifteen))
}

/// 内置 CPU 采集结果。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CpuSnapshot {
    /// 总 CPU 使用率（0–100）。
    pub usage_percent: Option<f64>,
    /// 1 分钟平均负载。
    pub load1: Option<f64>,
    /// 5 分钟平均负载。
    pub load5: Option<f64>,
    /// 15 分钟平均负载。
    pub load15: Option<f64>,
    /// CPU 型号。
    pub model: Option<String>,
    /// 物理核心数。
    pub physical_cores: Option<u32>,
    /// 逻辑核心数。
    pub logical_cores: Option<u32>,
}

/// 采集 CPU 使用率/负载/型号/核心数。任何一步失败只让对应字段为 None。
pub fn collect() -> CpuSnapshot {
    let usage_percent = match fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|content| CpuTimes::parse_stat(&content))
    {
        Some(previous) => {
            thread::sleep(SAMPLE_WINDOW);
            fs::read_to_string("/proc/stat")
                .ok()
                .and_then(|content| CpuTimes::parse_stat(&content))
                .and_then(|current| usage_percent(&previous, &current))
        }
        None => None,
    };

    let info = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .map(|content| parse_cpuinfo(&content))
        .unwrap_or_default();
    let load = fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|content| parse_loadavg(&content));

    CpuSnapshot {
        usage_percent,
        load1: load.map(|(one, _, _)| one),
        load5: load.map(|(_, five, _)| five),
        load15: load.map(|(_, _, fifteen)| fifteen),
        model: info.model,
        physical_cores: info.physical_cores,
        logical_cores: info.logical_cores,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn times(
        user: u64,
        nice: u64,
        system: u64,
        idle: u64,
        iowait: u64,
        irq: u64,
        softirq: u64,
        steal: u64,
    ) -> CpuTimes {
        CpuTimes {
            user,
            nice,
            system,
            idle,
            iowait,
            irq,
            softirq,
            steal,
        }
    }

    #[test]
    fn parses_aggregate_line_only() {
        let content = "cpu  100 2 50 800 10 0 3 5 0 0\ncpu0 10 0 5 80 1 0 0 0 0 0\n";
        let parsed = CpuTimes::parse_stat(content).unwrap();
        assert_eq!(parsed.total(), 970);
        assert_eq!(parsed.idle_all(), 810);
    }

    #[test]
    fn parse_line_accepts_short_older_kernels() {
        let parsed = CpuTimes::parse_line("cpu 1 2 3 4").unwrap();
        assert_eq!(parsed, times(1, 2, 3, 4, 0, 0, 0, 0));
        assert!(CpuTimes::parse_line("cpu0 1 2 3 4").is_none());
        assert!(CpuTimes::parse_line("intr 1 2 3").is_none());
    }

    #[test]
    fn usage_is_computed_from_deltas() {
        let previous = times(100, 0, 100, 800, 0, 0, 0, 0);
        // 100 jiffies 中 25 busy / 100 total = 25%
        let current = times(125, 0, 100, 875, 0, 0, 0, 0);
        assert_eq!(usage_percent(&previous, &current), Some(25.0));
    }

    #[test]
    fn usage_handles_idle_iowait_and_zero_delta() {
        let previous = times(0, 0, 0, 100, 0, 0, 0, 0);
        let current = times(0, 0, 0, 100, 10, 0, 0, 0);
        // 全部是 idle+iowait → 0%
        assert_eq!(usage_percent(&previous, &current), Some(0.0));
        assert_eq!(usage_percent(&previous, &previous), None);
        // 计数器回绕/重启（current < previous）不计算。
        assert_eq!(usage_percent(&current, &previous), None);
    }

    #[test]
    fn usage_never_exceeds_100() {
        let previous = times(0, 0, 0, 0, 0, 0, 0, 0);
        let current = times(10, 0, 0, 0, 0, 0, 0, 0);
        assert_eq!(usage_percent(&previous, &current), Some(100.0));
    }

    #[test]
    fn cpuinfo_extracts_model_and_core_counts() {
        let content = "\
processor\t: 0
vendor_id\t: AuthenticAMD
model name\t: AMD Ryzen 9 6900HX with Radeon Graphics
physical id\t: 0
siblings\t: 14
core id\t\t: 0
cpu cores\t: 7

processor\t: 1
model name\t: AMD Ryzen 9 6900HX with Radeon Graphics
physical id\t: 0
core id\t\t: 1

processor\t: 2
model name\t: AMD Ryzen 9 6900HX with Radeon Graphics
physical id\t: 0
core id\t\t: 0
";
        let info = parse_cpuinfo(content);
        assert_eq!(
            info.model.as_deref(),
            Some("AMD Ryzen 9 6900HX with Radeon Graphics")
        );
        assert_eq!(info.logical_cores, Some(3));
        // 只有 (0,0)、(0,1) 两个不同物理核心。
        assert_eq!(info.physical_cores, Some(2));
    }

    #[test]
    fn cpuinfo_handles_arm_style_without_physical_ids() {
        let content = "\
processor\t: 0
Processor\t: ARMv8 Processor rev 1
cpu cores\t: 4

processor\t: 1
Processor\t: ARMv8 Processor rev 1
";
        let info = parse_cpuinfo(content);
        assert_eq!(info.model.as_deref(), Some("ARMv8 Processor rev 1"));
        assert_eq!(info.logical_cores, Some(2));
        assert_eq!(info.physical_cores, Some(2));
    }

    #[test]
    fn empty_cpuinfo_is_safe() {
        let info = parse_cpuinfo("");
        assert_eq!(info, CpuInfo::default());
    }

    #[test]
    fn loadavg_is_parsed() {
        assert_eq!(
            parse_loadavg("1.94 1.59 1.51 5/2080 54482\n"),
            Some((1.94, 1.59, 1.51))
        );
        assert_eq!(parse_loadavg("garbage"), None);
        assert_eq!(parse_loadavg("1.0 2.0"), None);
        assert_eq!(parse_loadavg("-1 0 0"), None);
    }
}
