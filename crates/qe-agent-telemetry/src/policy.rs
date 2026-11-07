//! 运行时策略（契约 v1.1）：启动时通过 mTLS `GET /agent/config` 拉取两档上报间隔。
//!
//! 优先级与 `qe-agent-node` 一致：**远端策略 > 本地显式 env > 内置默认**。
//! 拉取失败只 `warn!`，继续用本地间隔，绝不阻塞上报。
//!
//! 远端只覆盖可下发字段（[`TelemetryIntervals`] 两个间隔）；mTLS 端点、slot、
//! 数据目录、可执行文件等本地专属配置永远只来自 env/CLI。

use anyhow::{Context, Result, bail};
use qe_device_pki_client::transport::build_mtls_client;
use qe_domain::agent_config::{AgentPolicy, AgentPolicyPatch, validate_policy_patch};
use serde::Deserialize;
use tracing::{info, warn};

use crate::config::Config;
use crate::mtls;

/// `GET /agent/config` 的响应（契约 §17.3）。
#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfigReply {
    /// 已合并的 global/area/room/device 配置 revision。
    #[serde(default)]
    pub revision: i64,
    /// 合并后的完整策略。
    pub policy: AgentPolicy,
}

/// 两档采集间隔（秒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetryIntervals {
    /// fast 档（内置指标）间隔。
    pub fast_secs: u64,
    /// full 档（含外部工具）间隔。
    pub full_secs: u64,
}

impl TelemetryIntervals {
    /// 由本地配置构造（env 已覆盖内置默认）。
    pub fn from_config(config: &Config) -> Self {
        Self {
            fast_secs: config.fast_interval_secs,
            full_secs: config.full_interval_secs,
        }
    }

    /// 每多少个 fast 周期重新拉取一次远端策略（默认 10s × 30 = 5 分钟）。
    pub const REFRESH_EVERY_FAST_CYCLES: u64 = 30;
}

impl std::fmt::Display for TelemetryIntervals {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "fast={}s full={}s",
            self.fast_secs, self.full_secs
        )
    }
}

/// 从完整策略中取出并校验两档间隔。
///
/// 只校验遥测字段，避免因远端策略里的其它（node 角色）字段不合法而拒绝应用。
pub fn intervals_from_policy(policy: &AgentPolicy) -> Result<TelemetryIntervals> {
    let patch = AgentPolicyPatch {
        telemetry_fast_interval_secs: Some(policy.telemetry_fast_interval_secs),
        telemetry_full_interval_secs: Some(policy.telemetry_full_interval_secs),
        ..Default::default()
    };
    validate_policy_patch(&patch).map_err(|error| anyhow::anyhow!("远端遥测间隔非法: {error}"))?;
    Ok(TelemetryIntervals {
        fast_secs: policy.telemetry_fast_interval_secs,
        full_secs: policy.telemetry_full_interval_secs,
    })
}

/// 应用远端响应（同步部分，便于单测）：校验通过才覆盖本地间隔。
///
/// 失败时返回 `Err` 且不改动 `local`，由调用方 `warn!` 后继续使用本地值。
pub fn apply_remote(
    local: TelemetryIntervals,
    reply: &AgentConfigReply,
) -> Result<TelemetryIntervals> {
    let remote = intervals_from_policy(&reply.policy)?;
    // 远端策略即使与内置默认相同也视为“已应用”，日志里给出 revision 便于排障。
    if remote != local {
        info!(
            revision = reply.revision,
            local = %local,
            remote = %remote,
            "已应用远端遥测间隔 revision={}",
            reply.revision
        );
    }
    Ok(remote)
}

/// 通过 mTLS 拉取远端策略；仅 mTLS 模式可用。
///
/// 在内部创建一次性 tokio runtime（room-agent 主循环是同步的），`build_mtls_client`
/// 直接复用 `qe-device-pki-client` 的信任配置（设备证书 + 设备 CA 链）。
pub fn fetch_remote_policy(config: &Config) -> Result<AgentConfigReply> {
    let credentials = mtls::load_credentials(config)?;
    let transport = mtls::transport_config(&credentials, config);
    let client = build_mtls_client(&transport)
        .map_err(|error| anyhow::anyhow!("构建 agent 配置 mTLS 客户端失败: {error}"))?;
    let url = mtls::join_endpoint(&transport.mtls_endpoint, mtls::AGENT_CONFIG_PATH);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("创建 tokio runtime 失败")?;
    runtime.block_on(async move {
        let response = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("拉取远端 agent 配置失败: GET {url}"))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!(
                "拉取远端 agent 配置被拒绝: GET {url} 返回 {status}（{}）",
                body.chars().take(256).collect::<String>()
            );
        }
        serde_json::from_str::<AgentConfigReply>(&body).with_context(|| {
            format!(
                "远端 agent 配置响应解析失败: GET {url}（{}）",
                body.chars().take(256).collect::<String>()
            )
        })
    })
}

/// 启动时拉取并应用远端策略；失败只记录 warn（绝不阻塞启动）。
pub fn fetch_and_apply(config: &Config, local: TelemetryIntervals) -> TelemetryIntervals {
    match fetch_remote_policy(config) {
        Ok(reply) => match apply_remote(local, &reply) {
            Ok(intervals) => intervals,
            Err(error) => {
                warn!(
                    error = %format!("{error:#}"),
                    local = %local,
                    "远端遥测间隔非法，继续使用本地间隔"
                );
                local
            }
        },
        Err(error) => {
            warn!(
                error = %format!("{error:#}"),
                local = %local,
                "启动拉取远端 agent 配置失败，继续使用本地间隔"
            );
            local
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::agent_config::{
        TELEMETRY_FAST_INTERVAL_SECS_RANGE, TELEMETRY_FULL_INTERVAL_SECS_RANGE,
    };

    fn local() -> TelemetryIntervals {
        TelemetryIntervals {
            fast_secs: 10,
            full_secs: 300,
        }
    }

    fn reply_with(fast: u64, full: u64) -> AgentConfigReply {
        AgentConfigReply {
            revision: 7,
            policy: AgentPolicy {
                telemetry_fast_interval_secs: fast,
                telemetry_full_interval_secs: full,
                ..Default::default()
            },
        }
    }

    #[test]
    fn remote_intervals_override_local() {
        let remote = apply_remote(local(), &reply_with(20, 600)).unwrap();
        assert_eq!(
            remote,
            TelemetryIntervals {
                fast_secs: 20,
                full_secs: 600
            }
        );
    }

    #[test]
    fn invalid_remote_intervals_are_rejected() {
        // 超出 fast 范围
        assert!(apply_remote(local(), &reply_with(4, 300)).is_err());
        assert!(apply_remote(local(), &reply_with(61, 300)).is_err());
        // 超出 full 范围
        assert!(apply_remote(local(), &reply_with(10, 59)).is_err());
        assert!(apply_remote(local(), &reply_with(10, 3601)).is_err());
        // full < fast（范围外组合可构造）
        assert!(apply_remote(local(), &reply_with(60, 59)).is_err());
    }

    #[test]
    fn policy_json_without_telemetry_fields_uses_contract_defaults() {
        let reply: AgentConfigReply = serde_json::from_value(serde_json::json!({
            "revision": 3,
            "policy": {
                "lxc_auto_start": false,
                "poll_interval_secs": 2,
                "download_concurrency": 4,
                "cache_max_bytes": 1024
            }
        }))
        .unwrap();
        let intervals = intervals_from_policy(&reply.policy).unwrap();
        assert_eq!(intervals.fast_secs, 10);
        assert_eq!(intervals.full_secs, 300);
    }

    #[test]
    fn policy_json_with_telemetry_fields_is_parsed() {
        let reply: AgentConfigReply = serde_json::from_value(serde_json::json!({
            "revision": 9,
            "policy": {
                "telemetry_fast_interval_secs": 15,
                "telemetry_full_interval_secs": 900
            }
        }))
        .unwrap();
        assert_eq!(reply.revision, 9);
        assert_eq!(
            intervals_from_policy(&reply.policy).unwrap(),
            TelemetryIntervals {
                fast_secs: 15,
                full_secs: 900
            }
        );
    }

    #[test]
    fn contract_ranges_and_refresh_constant_are_sane() {
        assert_eq!(*TELEMETRY_FAST_INTERVAL_SECS_RANGE.start(), 5);
        assert_eq!(*TELEMETRY_FAST_INTERVAL_SECS_RANGE.end(), 60);
        assert_eq!(*TELEMETRY_FULL_INTERVAL_SECS_RANGE.start(), 60);
        assert_eq!(*TELEMETRY_FULL_INTERVAL_SECS_RANGE.end(), 3600);
        assert_eq!(TelemetryIntervals::REFRESH_EVERY_FAST_CYCLES, 30);
    }
}
