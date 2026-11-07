//! 运行时策略（契约 §17.5）：本地/远端策略的持有、启动拉取与 `config.update` 应用。
//!
//! 策略放在 `std::sync::RwLock` 里（不是 async 锁），持锁时间只覆盖一次
//! `clone`/`replace`，**绝不跨 `await`**；这样轮询循环与 `image.pull` handler
//! 都能随时读到最新值。
//!
//! - [`RuntimePolicy`]：当前生效的 [`AgentPolicy`]
//! - [`MtlsAgentConfigClient`]：设备证书 mTLS `GET {mtls_endpoint}/agent/config`
//! - [`fetch_and_apply`]：启动拉取；失败时不改动本地策略（调用方 warn 后继续）
//! - [`handle_config_update`]：下行 `config.update`，校验完整策略后原子替换
//!
//! 远端/下行只覆盖策略字段（`AgentPolicy`）；mTLS 端点、slot、路径、可执行文件、
//! libvirt pool 等本地专属配置永远只来自 env。

use std::sync::RwLock;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use qe_device_pki_client::transport::{TransportConfig, build_mtls_client};
use qe_domain::agent_config::{AgentPolicy, AgentPolicyPatch};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::BoxFuture;

/// device-pki mTLS 上的远端策略路径（契约 §17.3）。
pub const DEFAULT_AGENT_CONFIG_PATH: &str = "/agent/config";

/// `GET /agent/config` 的响应（契约 §17.3）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfigReply {
    /// 已合并的全局/区域/房间配置 revision。
    pub revision: i64,
    /// 合并后的完整策略。
    pub policy: AgentPolicy,
}

/// 下行 `config.update` 的 payload（契约 §17.4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigUpdatePayload {
    pub revision: i64,
    pub policy: AgentPolicy,
}

impl ConfigUpdatePayload {
    pub fn from_json(payload_json: &str) -> Result<Self> {
        serde_json::from_str(payload_json).context("config.update payload JSON 解析失败")
    }
}

/// 一次策略切换的结果（用于日志与“需重启”提示）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyChange {
    pub previous: AgentPolicy,
    pub current: AgentPolicy,
    /// 远端/下行 payload 携带的 revision（本地配置为 0）。
    pub revision: i64,
}

impl PolicyChange {
    /// `download_concurrency` / `cache_max_bytes` 在启动时已固化进 Puller/缓存，
    /// 变更只提示重启，不重建。
    pub fn restart_required(&self) -> bool {
        self.previous.download_concurrency != self.current.download_concurrency
            || self.previous.cache_max_bytes != self.current.cache_max_bytes
    }
}

/// 当前生效策略。用 std `RwLock`，避免 async 锁跨 `await`。
#[derive(Debug)]
pub struct RuntimePolicy {
    current: RwLock<AgentPolicy>,
}

impl RuntimePolicy {
    pub fn new(initial: AgentPolicy) -> Self {
        Self {
            current: RwLock::new(initial),
        }
    }

    /// 读取当前策略快照（持锁仅一次 clone）。
    pub fn current(&self) -> AgentPolicy {
        self.current
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    /// 主循环轮询间隔。范围在写入时已校验；这里再兜底 `>= 1s`。
    pub fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.current().poll_interval_secs.max(1))
    }

    /// 原子替换策略，返回替换前的值。
    pub fn replace(&self, next: AgentPolicy) -> AgentPolicy {
        let mut guard = self
            .current
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        std::mem::replace(&mut *guard, next)
    }
}

/// 远端策略客户端抽象；生产走 mTLS，测试用 mock 即可覆盖失败回退。
pub trait AgentConfigClient: Send + Sync {
    fn fetch<'a>(&'a self) -> BoxFuture<'a, Result<AgentConfigReply>>;
}

/// 通过 device-pki mTLS 拉取生效策略的客户端。
pub struct MtlsAgentConfigClient {
    client: reqwest::Client,
    url: String,
}

impl MtlsAgentConfigClient {
    /// 用设备证书构建 mTLS 客户端，使用契约默认路径。
    pub fn new(transport: &TransportConfig) -> Result<Self> {
        Self::with_path(transport, DEFAULT_AGENT_CONFIG_PATH)
    }

    /// 指定路径构造（测试/自定义部署用）。
    pub fn with_path(transport: &TransportConfig, path: impl AsRef<str>) -> Result<Self> {
        let client = build_mtls_client(transport)
            .map_err(|error| anyhow!("构建 agent 配置 mTLS 客户端失败: {error}"))?;
        Ok(Self {
            client,
            url: crate::presign::join_endpoint(&transport.mtls_endpoint, path.as_ref()),
        })
    }

    /// 实际请求的 URL（日志/测试用）。
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl AgentConfigClient for MtlsAgentConfigClient {
    fn fetch<'a>(&'a self) -> BoxFuture<'a, Result<AgentConfigReply>> {
        Box::pin(async move {
            let response = self
                .client
                .get(&self.url)
                .send()
                .await
                .with_context(|| format!("拉取远端 agent 配置失败: GET {}", self.url))?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                bail!(
                    "拉取远端 agent 配置被拒绝: GET {} 返回 {status}（{}）",
                    self.url,
                    body.chars().take(256).collect::<String>()
                );
            }
            response
                .json::<AgentConfigReply>()
                .await
                .with_context(|| format!("远端 agent 配置响应解析失败: GET {}", self.url))
        })
    }
}

/// 把完整策略转成 patch（`validate_policy_patch` 的输入）。
pub fn complete_policy_patch(policy: &AgentPolicy) -> AgentPolicyPatch {
    AgentPolicyPatch {
        lxc_auto_start: Some(policy.lxc_auto_start),
        poll_interval_secs: Some(policy.poll_interval_secs),
        download_concurrency: Some(policy.download_concurrency),
        cache_max_bytes: Some(policy.cache_max_bytes),
        telemetry_fast_interval_secs: Some(policy.telemetry_fast_interval_secs),
        telemetry_full_interval_secs: Some(policy.telemetry_full_interval_secs),
    }
}

/// 校验一份完整策略（复用 domain 的完整策略校验：范围 + fast/full 顺序）。
pub fn validate_complete_policy(policy: &AgentPolicy) -> Result<()> {
    policy
        .validate()
        .map_err(|error| anyhow!("策略超出允许范围: {error}"))
}

/// 应用远端响应（同步部分，便于单测）：校验 → 替换 → 记录 revision 与值。
pub fn apply_remote(runtime: &RuntimePolicy, reply: AgentConfigReply) -> Result<PolicyChange> {
    validate_complete_policy(&reply.policy).context("远端 agent 配置非法")?;
    let previous = runtime.replace(reply.policy.clone());
    let change = PolicyChange {
        previous,
        current: reply.policy,
        revision: reply.revision,
    };
    info!(
        revision = change.revision,
        lxc_auto_start = change.current.lxc_auto_start,
        poll_interval_secs = change.current.poll_interval_secs,
        download_concurrency = change.current.download_concurrency,
        cache_max_bytes = change.current.cache_max_bytes,
        "已应用远端策略 revision={}",
        change.revision
    );
    Ok(change)
}

/// 启动时拉取远端策略；任何失败都返回 `Err` 且**不改动**当前（本地）策略，
/// 由调用方 warn 后继续启动。
pub async fn fetch_and_apply(
    client: &dyn AgentConfigClient,
    runtime: &RuntimePolicy,
) -> Result<PolicyChange> {
    let reply = client
        .fetch()
        .await
        .context("启动拉取远端 agent 配置失败")?;
    apply_remote(runtime, reply)
}

/// 处理下行 `config.update`：校验完整策略后替换；坏 payload/非法值返回 `Err`
/// （调用方不 ack，触发重投）。
pub fn handle_config_update(payload_json: &str, runtime: &RuntimePolicy) -> Result<PolicyChange> {
    let payload = ConfigUpdatePayload::from_json(payload_json)?;
    validate_complete_policy(&payload.policy).context("config.update 策略非法")?;

    let previous = runtime.replace(payload.policy.clone());
    let change = PolicyChange {
        previous,
        current: payload.policy,
        revision: payload.revision,
    };
    info!(
        revision = change.revision,
        lxc_auto_start = change.current.lxc_auto_start,
        poll_interval_secs = change.current.poll_interval_secs,
        download_concurrency = change.current.download_concurrency,
        cache_max_bytes = change.current.cache_max_bytes,
        "config 已更新 revision={}",
        change.revision
    );
    if change.restart_required() {
        warn!(
            revision = change.revision,
            download_concurrency = change.current.download_concurrency,
            cache_max_bytes = change.current.cache_max_bytes,
            "download_concurrency/cache_max_bytes 变更需重启生效（当前仅更新策略，不重建 Puller/缓存）"
        );
    }
    Ok(change)
}
