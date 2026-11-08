//! 本地持续记录 + 可选实时中转。
//!
//! 设计：
//! - 采样线程默认每 `ROOM_AGENT_LIVE_INTERVAL_MILLIS`（默认 500）采一次，写入进程内环形缓冲；
//! - `ROOM_AGENT_LIVE_PUSH=1` 时才把每个样本 POST 到 device-pki `/metrics`（毫秒实时链路）；
//! - 关闭实时推送时，样本仍在本地记录，下一次 fast/full 快照会把窗口聚合写入
//!   `sections.live_metrics`，即“本地持续记录，某一刻批量提交”。
//! - 间隔 0 = 完全关闭本地记录。

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::json;

use crate::collect::{cpu, disk, memory};
use crate::config::Config;
use crate::mtls;

pub const DEFAULT_INTERVAL_MS: u64 = 500;
const BUFFER_CAPACITY: usize = 7_200; // 500ms × 1h

#[derive(Debug, Clone, Copy)]
struct LiveSample {
    at_ms: i64,
    cpu: Option<f64>,
    memory: Option<f64>,
    disk: Option<f64>,
}

static BUFFER: OnceLock<Mutex<VecDeque<LiveSample>>> = OnceLock::new();

fn buffer() -> &'static Mutex<VecDeque<LiveSample>> {
    BUFFER.get_or_init(|| Mutex::new(VecDeque::with_capacity(BUFFER_CAPACITY)))
}

fn interval_ms() -> u64 {
    std::env::var("ROOM_AGENT_LIVE_INTERVAL_MILLIS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.min(5_000))
        .unwrap_or(DEFAULT_INTERVAL_MS)
}

fn push_enabled() -> bool {
    std::env::var("ROOM_AGENT_LIVE_PUSH")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn load1() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn uptime_secs() -> Option<u64> {
    std::fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()
        .map(|value| value as u64)
}

fn read_cpu() -> Option<cpu::CpuTimes> {
    std::fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|content| cpu::CpuTimes::parse_stat(&content))
}

/// 聚合本地环形缓冲（不消费）：即使本次上报失败，样本仍保留，下一份快照继续提交。
/// 缓冲上限 1h，滚动窗口；`sections.live_metrics` 表示最近窗口的本地记录。
pub fn report_section() -> Option<serde_json::Value> {
    let samples: Vec<LiveSample> = {
        let queue = buffer().lock().unwrap_or_else(|e| e.into_inner());
        queue.iter().copied().collect()
    };
    if samples.is_empty() {
        return None;
    }
    let count = samples.len() as u64;
    let first = samples.first()?.at_ms;
    let last = samples.last()?.at_ms;
    let avg = |values: Vec<f64>| -> Option<f64> {
        (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
    };
    let max = |values: Vec<f64>| values.into_iter().reduce(f64::max);
    // 原始样本随本次快照批量提交（最多最近 600 个，约 5 分钟 @500ms），
    // 避免无限增长；聚合之外的明细供控制面追溯。
    let items: Vec<serde_json::Value> = samples
        .iter()
        .rev()
        .take(600)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|sample| {
            json!({
                "ts_ms": sample.at_ms,
                "cpu": sample.cpu,
                "memory": sample.memory,
                "disk": sample.disk,
            })
        })
        .collect();
    let cpu_values: Vec<f64> = samples.iter().filter_map(|s| s.cpu).collect();
    let memory_values: Vec<f64> = samples.iter().filter_map(|s| s.memory).collect();
    let disk_values: Vec<f64> = samples.iter().filter_map(|s| s.disk).collect();
    Some(json!({
        "samples": count,
        "window_ms": (last - first).max(0),
        "cpu_avg": avg(cpu_values.clone()),
        "cpu_max": max(cpu_values),
        "memory_avg": avg(memory_values.clone()),
        "memory_max": max(memory_values),
        "disk_avg": avg(disk_values.clone()),
        "disk_max": max(disk_values),
        "source": "local-buffer",
        "items": items,
    }))
}

/// 启动本地采样线程；实时推送开关由 `ROOM_AGENT_LIVE_PUSH` 决定。
pub fn spawn(config: &Config) -> Result<()> {
    let interval = interval_ms();
    if interval == 0 || config.print || !config.is_mtls() {
        return Ok(());
    }
    let interval = Duration::from_millis(interval.max(100));
    let config = config.clone();
    let push = push_enabled();
    let client = if push {
        Some(mtls::build_blocking_client(&config).context("构建实时指标 mTLS 客户端失败")?)
    } else {
        None
    };
    let url = if push {
        Some(format!(
            "{}/metrics",
            config
                .mtls_endpoint
                .as_deref()
                .context("实时指标缺少 mTLS 端点")?
                .trim_end_matches('/')
        ))
    } else {
        None
    };
    let credentials = if push {
        Some(mtls::load_credentials(&config)?)
    } else {
        None
    };
    let serial = credentials
        .as_ref()
        .map(|credentials| credentials.serial.clone());
    let slot = config.device_slot.clone();

    std::thread::Builder::new()
        .name("qe-agent-live-metrics".to_owned())
        .spawn(move || {
            let mut previous = read_cpu();
            let mut failures = 0_u64;
            loop {
                std::thread::sleep(interval);
                let current = read_cpu();
                let cpu_percent = match (&previous, &current) {
                    (Some(prev), Some(curr)) => cpu::usage_percent(prev, curr),
                    _ => None,
                };
                previous = current;
                let sample = LiveSample {
                    at_ms: chrono::Utc::now().timestamp_millis(),
                    cpu: cpu_percent,
                    memory: memory::collect().as_ref().map(|s| s.usage_percent),
                    disk: disk::representative_usage(&disk::collect()),
                };
                {
                    let mut queue = buffer().lock().unwrap_or_else(|e| e.into_inner());
                    if queue.len() >= BUFFER_CAPACITY {
                        queue.pop_front();
                    }
                    queue.push_back(sample);
                }

                let (Some(client), Some(url), Some(serial)) =
                    (client.as_ref(), url.as_ref(), serial.as_ref())
                else {
                    continue;
                };
                let payload = json!({
                    "schema": "live-metrics/1",
                    "serial": serial,
                    "device_slot": slot,
                    "collected_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    "cpu_usage_percent": sample.cpu,
                    "memory_usage_percent": sample.memory,
                    "disk_usage_percent": sample.disk,
                    "load1": load1(),
                    "uptime_secs": uptime_secs(),
                });
                match client.post(url).json(&payload).send() {
                    Ok(response) if response.status().is_success() => failures = 0,
                    Ok(response) => {
                        failures += 1;
                        if failures <= 3 || failures % 30 == 0 {
                            tracing::warn!(status = %response.status(), failures, "实时指标推送失败");
                        }
                    }
                    Err(error) => {
                        failures += 1;
                        if failures <= 3 || failures % 30 == 0 {
                            tracing::warn!(%error, failures, "实时指标推送失败（mTLS）");
                        }
                    }
                }
            }
        })
        .context("启动本地指标采样线程失败")?;
    tracing::info!(
        interval_ms = interval.as_millis() as u64,
        push,
        "本地指标采样已启动（实时推送={})",
        push
    );
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_section_aggregates_and_clears_local_buffer() {
        {
            let mut queue = buffer().lock().unwrap();
            queue.clear();
            queue.push_back(LiveSample { at_ms: 1000, cpu: Some(10.0), memory: Some(40.0), disk: Some(80.0) });
            queue.push_back(LiveSample { at_ms: 2000, cpu: Some(30.0), memory: Some(60.0), disk: Some(82.0) });
        }
        let section = report_section().expect("window");
        assert_eq!(section["samples"], 2);
        assert_eq!(section["cpu_max"], 30.0);
        assert_eq!(section["memory_avg"], 50.0);
        assert_eq!(section["items"].as_array().unwrap().len(), 2);
        // 不消费缓冲：上报失败时本地样本仍保留，下一份快照继续聚合。
        assert_eq!(report_section().expect("still buffered")["samples"], 2);
    }

    #[test]
    fn push_defaults_off_so_local_recording_is_default() {
        // 测试环境未设置 ROOM_AGENT_LIVE_PUSH 时应为 false（只本地记录）。
        if std::env::var_os("ROOM_AGENT_LIVE_PUSH").is_none() {
            assert!(!push_enabled());
        }
    }
}
