//! 毫秒级实时指标推送：采样后 `POST {mtls_endpoint}/metrics`，由 device-pki 内存广播
//! 给 qe-api（不逐样本写 DB）。历史快照仍由 `run_round` 负责。
//!
//! 环境变量：`ROOM_AGENT_LIVE_INTERVAL_MILLIS`（默认 500，0 关闭，范围 100–5000）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::json;

use crate::collect::{cpu, disk, memory};
use crate::config::Config;
use crate::mtls;

pub const DEFAULT_INTERVAL_MS: u64 = 500;

fn interval_ms() -> u64 {
    std::env::var("ROOM_AGENT_LIVE_INTERVAL_MILLIS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.min(5_000))
        .unwrap_or(DEFAULT_INTERVAL_MS)
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

/// 启动实时指标线程（每进程最多一次；调用方保证）。
pub fn spawn(config: &Config) -> Result<()> {
    let interval = interval_ms();
    if interval == 0 || config.print || !config.is_mtls() {
        return Ok(());
    }
    let interval = Duration::from_millis(interval.max(100));
    let config = config.clone();
    let client = mtls::build_blocking_client(&config).context("构建实时指标 mTLS 客户端失败")?;
    let url = format!(
        "{}/metrics",
        config
            .mtls_endpoint
            .as_deref()
            .context("实时指标缺少 mTLS 端点")?
            .trim_end_matches('/')
    );
    let credentials = mtls::load_credentials(&config)?;
    let serial = credentials.serial.clone();
    let slot = config.device_slot.clone();
    let failures = AtomicU64::new(0);

    std::thread::Builder::new()
        .name("qe-agent-live-metrics".to_owned())
        .spawn(move || {
            let mut previous = read_cpu();
            let ticker = std::thread::sleep;
            loop {
                ticker(interval);
                let current = read_cpu();
                let cpu_percent = match (&previous, &current) {
                    (Some(prev), Some(curr)) => cpu::usage_percent(prev, curr),
                    _ => None,
                };
                previous = current;
                let memory = memory::collect();
                let disks = disk::collect();
                let payload = json!({
                    "schema": "live-metrics/1",
                    "serial": serial,
                    "device_slot": slot,
                    "collected_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    "cpu_usage_percent": cpu_percent,
                    "memory_usage_percent": memory.as_ref().map(|snapshot| snapshot.usage_percent),
                    "disk_usage_percent": disk::representative_usage(&disks),
                    "load1": load1(),
                    "uptime_secs": uptime_secs(),
                });
                match client.post(&url).json(&payload).send() {
                    Ok(response) if response.status().is_success() => {
                        failures.store(0, Ordering::Relaxed);
                    }
                    Ok(response) => {
                        let count = failures.fetch_add(1, Ordering::Relaxed) + 1;
                        if count <= 3 || count % 30 == 0 {
                            tracing::warn!(status = %response.status(), count, "实时指标推送失败");
                        }
                    }
                    Err(error) => {
                        let count = failures.fetch_add(1, Ordering::Relaxed) + 1;
                        if count <= 3 || count % 30 == 0 {
                            tracing::warn!(%error, count, "实时指标推送失败（mTLS）");
                        }
                    }
                }
            }
        })
        .context("启动实时指标线程失败")?;
    tracing::info!(interval_ms = interval.as_millis() as u64, "实时指标推送已启动");
    Ok(())
}
