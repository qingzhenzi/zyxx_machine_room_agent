//! 内置采集器：主机清单、CPU、内存、磁盘、网络、温度。
//!
//! 所有采集器都是「尽力而为」：数据源读不到就返回 None/空集合，绝不返回错误，
//! 这样单台机器缺少 /proc 某项时仍能上报其余指标。
//!
//! 两档采集（契约 v1.1）：
//! - [`MachineReportTier::Fast`]：只跑内置采集（/proc、df、hwmon），不启动任何外部工具；
//! - [`MachineReportTier::Full`]：内置采集 + platform-health / diagnose / qeaudit /
//!   docker / virsh（各自超时，diagnose/qeaudit 按间隔复用缓存）。

pub mod cpu;
pub mod disk;
pub mod host;
pub mod macs;
pub mod memory;
pub mod network;
pub mod thermal;

use std::time::Instant;

use qe_domain::machine::MachineReportTier;
use serde_json::{Map, Value, json};

use crate::config::Config;
use crate::external;
use crate::health;
use crate::report::{Health, Metrics, Report, SCHEMA_VERSION};

/// 采集并组装一份上报 payload。
///
/// 顺序：内置采集（CPU 需要 500ms 采样）→ 外部工具（仅 full 档；各自超时上限，
/// diagnose / qeaudit 按间隔复用 [`external::ToolsCache`]）→
/// 健康结论（优先 platform-health，其次内置阈值）→ 组装 [`Report`]。
pub fn collect_report(
    config: &Config,
    tools_cache: &mut external::ToolsCache,
    tier: MachineReportTier,
    fast_interval_secs: u64,
    full_interval_secs: u64,
) -> Report {
    let builtin_started = Instant::now();
    let host = host::collect();
    let cpu = cpu::collect();
    let memory = memory::collect();
    let disks = disk::collect();
    let network = network::collect();
    let thermal = thermal::collect();
    let builtin_duration_ms = builtin_started.elapsed().as_millis() as u64;

    let external = match tier {
        MachineReportTier::Fast => external::skipped_outcome(),
        MachineReportTier::Full => external::collect_with_cache(config, tools_cache),
    };

    let metrics = Metrics {
        cpu_usage_percent: cpu.usage_percent,
        memory_usage_percent: memory.as_ref().map(|snapshot| snapshot.usage_percent),
        disk_usage_percent: disk::representative_usage(&disks),
        uptime_secs: host.uptime_secs,
    };

    let signals = health::BuiltinSignals {
        cpu_usage_percent: metrics.cpu_usage_percent,
        memory_usage_percent: metrics.memory_usage_percent,
        disk_usage_percent: metrics.disk_usage_percent,
        max_temperature_c: thermal::max_temperature(&thermal),
        load1: cpu.load1,
        logical_cores: cpu.logical_cores,
    };
    let health: Health = external
        .platform_health
        .as_ref()
        .and_then(health::adopt_platform_health)
        .unwrap_or_else(|| health::evaluate_builtin(&signals));

    let machine_id = host::machine_id_for(&host.hostname);
    let sections = build_sections(
        config,
        tier,
        &external,
        &disks,
        &network,
        &thermal,
        &host,
        &cpu,
        &machine_id,
        builtin_duration_ms,
        fast_interval_secs,
        full_interval_secs,
    );

    Report {
        schema: SCHEMA_VERSION,
        device_slot: config.device_slot.clone(),
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
        collected_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        hostname: host.hostname,
        os_name: host.os_name,
        kernel_version: host.kernel_version,
        health,
        metrics,
        tier,
        sections,
    }
}

/// 组装 `sections`：外部工具原样存档 + 内置明细 + `agent` 元数据。
#[allow(clippy::too_many_arguments)]
fn build_sections(
    config: &Config,
    tier: MachineReportTier,
    external: &external::ToolsOutcome,
    disks: &[disk::DiskInfo],
    network: &[network::InterfaceStats],
    thermal: &[thermal::ThermalSensor],
    host: &host::HostInfo,
    cpu: &cpu::CpuSnapshot,
    machine_id: &str,
    builtin_duration_ms: u64,
    fast_interval_secs: u64,
    full_interval_secs: u64,
) -> Map<String, Value> {
    let mut sections = Map::new();

    // 外部工具原始 JSON 原样存档，控制台按已知 key 渲染（fast 档为空）。
    if let Some(value) = &external.platform_health {
        sections.insert("platform_health".to_owned(), value.clone());
    }
    if let Some(value) = &external.diagnose {
        sections.insert("diagnose".to_owned(), value.clone());
    }
    if let Some(value) = &external.security {
        // qeaudit 原始报告包装成契约示例的 {summary, raw} 形状。
        sections.insert("security".to_owned(), external::security_section(value));
    }
    if let Some(value) = &external.containers {
        sections.insert("containers".to_owned(), Value::Array(value.clone()));
    }
    if let Some(value) = &external.vms {
        sections.insert("vms".to_owned(), Value::Array(value.clone()));
    }

    // 内置采集明细（契约未强制，便于排障/后续展示）。
    sections.insert("disks".to_owned(), json!(disks));
    sections.insert("network".to_owned(), json!(network));
    sections.insert("thermal".to_owned(), json!(thermal));

    // fast/full 两档各自的当前周期；`interval_secs` 保持契约 v1 字段名（当前档间隔）。
    let tier_interval_secs = match tier {
        MachineReportTier::Fast => fast_interval_secs,
        MachineReportTier::Full => full_interval_secs,
    };
    sections.insert(
        "agent".to_owned(),
        json!({
            "agent_version": env!("CARGO_PKG_VERSION"),
            "tier": tier.as_str(),
            "report_mode": config.report_mode.as_str(),
            "interval_secs": tier_interval_secs,
            "fast_interval_secs": fast_interval_secs,
            "full_interval_secs": full_interval_secs,
            "builtin_collect_duration_ms": builtin_duration_ms,
            "tools": external.statuses,
            "host": {
                "machine_id": machine_id,
                "hostname": host.hostname,
                "os_name": host.os_name,
                "kernel_version": host.kernel_version,
                "arch": host.arch,
                "uptime_secs": host.uptime_secs,
                "macs": host.macs,
                "cpu_model": cpu.model,
                "logical_cores": cpu.logical_cores,
                "physical_cores": cpu.physical_cores,
                "load1": cpu.load1,
                "load5": cpu.load5,
                "load15": cpu.load15,
            },
        }),
    );

    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config_with(pairs: &[(&str, &str)]) -> Config {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        Config::from_lookup(&crate::cli::Cli::default(), |key| map.get(key).cloned())
            .expect("config")
    }

    fn disabled_config() -> Config {
        config_with(&[
            ("DEVICE_PKI_MTLS_URL", "https://node:4441"),
            ("DEVICE_SLOT", "room-101"),
            ("ROOM_AGENT_DISABLE_HEALTH", "true"),
            ("ROOM_AGENT_DISABLE_DIAGNOSE", "true"),
            ("ROOM_AGENT_DISABLE_SECURITY", "true"),
            ("ROOM_AGENT_DISABLE_DOCKER", "true"),
            ("ROOM_AGENT_DISABLE_VMS", "true"),
        ])
    }

    #[test]
    fn fast_tier_skips_external_tools_entirely() {
        let config = disabled_config();
        let mut cache = external::ToolsCache::default();
        let report = collect_report(
            &config,
            &mut cache,
            MachineReportTier::Fast,
            config.fast_interval_secs,
            config.full_interval_secs,
        );
        assert_eq!(report.tier, MachineReportTier::Fast);

        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["tier"], serde_json::json!("fast"));
        let agent = &value["sections"]["agent"];
        assert_eq!(agent["tier"], serde_json::json!("fast"));
        assert_eq!(agent["report_mode"], serde_json::json!("mtls"));
        assert_eq!(agent["interval_secs"], serde_json::json!(10));
        assert_eq!(agent["fast_interval_secs"], serde_json::json!(10));
        assert_eq!(agent["full_interval_secs"], serde_json::json!(300));
        // fast 档不启动外部工具：5 个工具全部标记 skipped，且没有外部 section。
        for tool in [
            "platform_health",
            "platform_diagnose",
            "qeaudit",
            "docker",
            "virsh",
        ] {
            assert_eq!(
                agent["tools"][tool],
                serde_json::json!({"available": false, "skipped": true}),
                "tool {tool}"
            );
        }
        for section in [
            "platform_health",
            "diagnose",
            "security",
            "containers",
            "vms",
        ] {
            assert!(
                value["sections"].get(section).is_none(),
                "fast 档不应包含 {section}"
            );
        }
        // 内置明细仍在。
        assert!(value["sections"]["disks"].is_array());
        assert!(value["sections"]["network"].is_array());
        assert!(value["sections"]["thermal"].is_array());
        // 网卡 MAC（WOL 用）：`sections.agent.host.macs`，/sys/class/net 读不到时是空数组。
        assert!(
            agent["host"]["macs"].is_array(),
            "sections.agent.host.macs 必须是数组"
        );
    }

    #[test]
    fn full_tier_records_tool_statuses_and_tier() {
        let config = disabled_config();
        let mut cache = external::ToolsCache::default();
        let report = collect_report(
            &config,
            &mut cache,
            MachineReportTier::Full,
            config.fast_interval_secs,
            config.full_interval_secs,
        );
        assert_eq!(report.tier, MachineReportTier::Full);

        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["tier"], serde_json::json!("full"));
        let agent = &value["sections"]["agent"];
        assert_eq!(agent["tier"], serde_json::json!("full"));
        assert_eq!(agent["interval_secs"], serde_json::json!(300));
        for tool in [
            "platform_health",
            "platform_diagnose",
            "qeaudit",
            "docker",
            "virsh",
        ] {
            assert_eq!(
                agent["tools"][tool],
                serde_json::json!({"available": false, "disabled": true}),
                "tool {tool}"
            );
        }
    }
}
