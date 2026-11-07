//! agent 配置：环境变量 + 本地配置文件（契约 §16.5 / §17.5）。
//!
//! 必填项只有两项（`DEVICE_PKI_MTLS_URL`、`DEVICE_SLOT`），其余全部有默认值，
//! 便于在开发机上直接跑。策略类字段的优先级（低 → 高）：
//!
//! 1. 内置默认（[`AgentPolicy::default`]）
//! 2. 本地配置文件 `AGENT_CONFIG_FILE` 的 `policy` patch
//!    （默认 `<DEVICE_DATA_DIR>/agent-config.json`）
//! 3. 显式 env：`LXC_AUTO_START` / `DOWNLOAD_CONCURRENCY` / `CACHE_MAX_BYTES` / `POLL_INTERVAL_SECS`
//! 4. 远端 `GET /agent/config`（启动时拉取，见 [`crate::policy`]）
//!
//! 本文件的 [`AgentConfig::local_policy`] 是第 1–3 步的结果；远端覆盖后的策略由
//! `RuntimePolicy` 持有。mTLS 端点、slot、路径、可执行文件、libvirt pool 等
//! **本地专属字段只来自 env**，配置文件与远端都不能覆盖。
//!
//! 解析逻辑同时提供 [`AgentConfig::from_lookup`]，让测试无需改写进程环境即可
//! 断言默认值、缺失项与配置文件优先级。

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use qe_domain::agent_config::{AgentPolicy, AgentPolicyPatch};
use serde::Deserialize;

use crate::policy::validate_complete_policy;

/// `CACHE_MAX_BYTES` 默认 10 GiB。
pub const DEFAULT_CACHE_MAX_BYTES: u64 = 10 * 1024 * 1024 * 1024;
/// `DOWNLOAD_CONCURRENCY` 默认 4。
pub const DEFAULT_DOWNLOAD_CONCURRENCY: usize = 4;
/// `POLL_INTERVAL_SECS` 默认 2。
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 2;
/// 本地配置文件默认文件名（位于 `DEVICE_DATA_DIR` 下）。
pub const DEFAULT_CONFIG_FILE_NAME: &str = "agent-config.json";
/// 允许的轮询间隔范围（秒）。
pub const POLL_INTERVAL_SECS_RANGE: std::ops::RangeInclusive<u64> = 1..=60;
/// 允许的下载并发范围。
pub const DOWNLOAD_CONCURRENCY_RANGE: std::ops::RangeInclusive<usize> = 1..=32;
/// 允许的缓存上限上限（1 TiB）。
pub const MAX_CACHE_MAX_BYTES: u64 = 1024 * 1024 * 1024 * 1024;

/// agent 运行配置（契约 §16.4 的 `AgentConfig` + §16.5 的 `LIBVIRT_POOL` + §17.5 策略）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentConfig {
    /// device-pki mTLS 端点，如 `https://node:4441`。
    pub mtls_endpoint: String,
    /// 设备槽位（与证书 CN 一致）。
    pub slot: String,
    /// 客户端状态/凭证目录。
    pub data_dir: PathBuf,
    /// 分片缓存目录。
    pub cache_dir: PathBuf,
    /// 缓存上限（字节）。
    pub cache_max_bytes: u64,
    /// 并发分片数。
    pub download_concurrency: usize,
    /// `docker` 可执行文件路径。
    pub docker_bin: String,
    /// `ctr` 可执行文件路径。
    pub ctr_bin: String,
    /// `incus` 可执行文件路径（为空时 plain LXC 兜底）。
    pub incus_bin: String,
    /// `virsh` 可执行文件路径。
    pub virsh_bin: String,
    /// libvirt 连接 URI（如 `qemu:///system`）。
    pub libvirt_uri: String,
    /// libvirt storage pool 名。
    pub libvirt_pool: String,
    /// plain LXC 根目录。
    pub lxc_rootfs_dir: PathBuf,
    /// Incus 导入成功后是否自动 `incus launch` 启动实例。
    ///
    /// 这是 [`Self::local_policy`] 的镜像字段；`image.pull` 实际读取运行时策略
    /// （见 [`crate::policy::RuntimePolicy`]）。
    pub lxc_auto_start: bool,
    /// 本地配置文件路径（`AGENT_CONFIG_FILE`，默认 `<DEVICE_DATA_DIR>/agent-config.json`）。
    pub config_file: PathBuf,
    /// 合并「内置默认 < 配置文件 patch < env」后的本地策略（远端覆盖前）。
    pub local_policy: AgentPolicy,
}

impl AgentConfig {
    /// 从进程环境读取配置。
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|key| env::var(key).ok())
    }

    /// 用给定的查找函数解析配置（测试用）。
    ///
    /// 空白值视同未设置，这样 systemd 的 `Environment=FOO=` 不会静默覆盖默认值。
    /// 配置文件不存在视为「没有 patch」；存在但无法解析/取值越界则直接报错，
    /// 避免带着错误配置静默启动。
    pub fn from_lookup<F>(lookup: F) -> Result<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        let get = |key: &str| -> Option<String> {
            lookup(key)
                .map(|raw| raw.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        // INCUS_BIN 是唯一允许"显式置空"的变量：空值 = 禁用 Incus、走 plain LXC 兜底
        // （契约 §16.4）。因此这里不做空白过滤。
        let incus_bin = lookup("INCUS_BIN")
            .map(|raw| raw.trim().to_owned())
            .unwrap_or_else(|| "incus".to_owned());

        let mtls_endpoint = required(&get, "DEVICE_PKI_MTLS_URL")?;
        if !(mtls_endpoint.starts_with("https://") || mtls_endpoint.starts_with("http://")) {
            bail!("DEVICE_PKI_MTLS_URL 必须是以 http:// 或 https:// 开头的 URL");
        }
        let slot = required(&get, "DEVICE_SLOT")?;

        let data_dir = path_or(&get, "DEVICE_DATA_DIR", "./data");
        let config_file = get("AGENT_CONFIG_FILE")
            .map_or_else(|| data_dir.join(DEFAULT_CONFIG_FILE_NAME), PathBuf::from);

        // 优先级：内置默认 < 配置文件 patch < 显式 env。
        let mut policy = AgentPolicy::default();
        load_config_file(&config_file)?.apply_to(&mut policy);
        apply_env_policy(&get, &mut policy)?;
        validate_local_policy(&policy)?;

        Ok(Self {
            mtls_endpoint,
            slot,
            data_dir: data_dir.clone(),
            cache_dir: path_or(&get, "CACHE_DIR", "./cache"),
            cache_max_bytes: policy.cache_max_bytes,
            download_concurrency: policy.download_concurrency,
            docker_bin: string_or(&get, "DOCKER_BIN", "docker"),
            ctr_bin: string_or(&get, "CTR_BIN", "ctr"),
            incus_bin,
            virsh_bin: string_or(&get, "VIRSH_BIN", "virsh"),
            libvirt_uri: string_or(&get, "LIBVIRT_URI", "qemu:///system"),
            libvirt_pool: string_or(&get, "LIBVIRT_POOL", "default"),
            lxc_rootfs_dir: path_or(&get, "LXC_ROOTFS_DIR", "/var/lib/lxc"),
            lxc_auto_start: policy.lxc_auto_start,
            config_file,
            local_policy: policy,
        })
    }
}

/// 本地配置文件格式：`{"policy": {...patch...}}`。
#[derive(Debug, Clone, Default, Deserialize)]
struct AgentConfigFile {
    #[serde(default)]
    policy: AgentPolicyPatch,
}

/// 读取本地配置文件的 policy patch；文件不存在返回空 patch。
fn load_config_file(path: &Path) -> Result<AgentPolicyPatch> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AgentPolicyPatch::default());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("读取 agent 配置文件失败: {}", path.display()));
        }
    };
    let file: AgentConfigFile = serde_json::from_slice(&bytes)
        .with_context(|| format!("解析 agent 配置文件失败: {}", path.display()))?;
    Ok(file.policy)
}

/// 显式 env 覆盖（只覆盖真正设置过的字段）。
fn apply_env_policy<F>(get: &F, policy: &mut AgentPolicy) -> Result<()>
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(raw) = get("LXC_AUTO_START") {
        policy.lxc_auto_start = parse_bool("LXC_AUTO_START", &raw)?;
    }
    if let Some(raw) = get("DOWNLOAD_CONCURRENCY") {
        policy.download_concurrency = raw
            .parse::<usize>()
            .with_context(|| format!("DOWNLOAD_CONCURRENCY 不是合法整数: `{raw}`"))?;
    }
    if let Some(raw) = get("CACHE_MAX_BYTES") {
        policy.cache_max_bytes = raw
            .parse::<u64>()
            .with_context(|| format!("CACHE_MAX_BYTES 不是合法字节数: `{raw}`"))?;
    }
    if let Some(raw) = get("POLL_INTERVAL_SECS") {
        policy.poll_interval_secs = raw
            .parse::<u64>()
            .with_context(|| format!("POLL_INTERVAL_SECS 不是合法整数: `{raw}`"))?;
    }
    Ok(())
}

/// 最终本地策略的显式范围检查（错误信息带 env 名，便于运维定位）。
fn validate_local_policy(policy: &AgentPolicy) -> Result<()> {
    if !POLL_INTERVAL_SECS_RANGE.contains(&policy.poll_interval_secs) {
        bail!(
            "POLL_INTERVAL_SECS 必须在 1..=60 之间，实际 {}",
            policy.poll_interval_secs
        );
    }
    if !DOWNLOAD_CONCURRENCY_RANGE.contains(&policy.download_concurrency) {
        bail!(
            "DOWNLOAD_CONCURRENCY 必须在 1..=32 之间，实际 {}",
            policy.download_concurrency
        );
    }
    if policy.cache_max_bytes > MAX_CACHE_MAX_BYTES {
        bail!(
            "CACHE_MAX_BYTES 不能超过 1 TiB（{MAX_CACHE_MAX_BYTES}），实际 {}",
            policy.cache_max_bytes
        );
    }
    validate_complete_policy(policy).context("本地策略非法")?;
    Ok(())
}

fn required<F>(get: &F, key: &str) -> Result<String>
where
    F: Fn(&str) -> Option<String>,
{
    match get(key) {
        Some(value) => Ok(value),
        None => bail!("缺少必填环境变量 {key}"),
    }
}

fn string_or<F>(get: &F, key: &str, default: &str) -> String
where
    F: Fn(&str) -> Option<String>,
{
    get(key).unwrap_or_else(|| default.to_owned())
}

fn parse_bool(key: &str, raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => anyhow::bail!("{key} 不是合法布尔值: {other}"),
    }
}

fn path_or<F>(get: &F, key: &str, default: &str) -> PathBuf
where
    F: Fn(&str) -> Option<String>,
{
    PathBuf::from(get(key).unwrap_or_else(|| default.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key| map.get(key).cloned()
    }

    /// 指向不存在的配置文件，保证默认值断言不受工作目录影响。
    const NO_FILE: &str = "/nonexistent/qe-agent/agent-config.json";

    fn minimal() -> Vec<(&'static str, &'static str)> {
        vec![
            ("DEVICE_PKI_MTLS_URL", "https://node:4441"),
            ("DEVICE_SLOT", "slot-1"),
            ("AGENT_CONFIG_FILE", NO_FILE),
        ]
    }

    #[test]
    fn defaults_apply_when_only_required_are_set() {
        let config = AgentConfig::from_lookup(lookup_from(&minimal())).unwrap();

        assert_eq!(config.mtls_endpoint, "https://node:4441");
        assert_eq!(config.slot, "slot-1");
        assert_eq!(config.data_dir, PathBuf::from("./data"));
        assert_eq!(config.cache_dir, PathBuf::from("./cache"));
        assert_eq!(config.cache_max_bytes, 10_737_418_240);
        assert_eq!(config.cache_max_bytes, DEFAULT_CACHE_MAX_BYTES);
        assert_eq!(config.download_concurrency, 4);
        assert_eq!(config.docker_bin, "docker");
        assert_eq!(config.ctr_bin, "ctr");
        assert_eq!(config.incus_bin, "incus");
        assert_eq!(config.virsh_bin, "virsh");
        assert_eq!(config.libvirt_uri, "qemu:///system");
        assert_eq!(config.libvirt_pool, "default");
        assert_eq!(config.lxc_rootfs_dir, PathBuf::from("/var/lib/lxc"));
        assert_eq!(config.config_file, PathBuf::from(NO_FILE));
        assert!(!config.lxc_auto_start);
        assert_eq!(config.local_policy, AgentPolicy::default());
    }

    #[test]
    fn overrides_are_parsed() {
        let mut pairs = minimal();
        pairs.extend([
            ("DEVICE_DATA_DIR", "/var/lib/qe"),
            ("CACHE_DIR", "/var/cache/qe"),
            ("CACHE_MAX_BYTES", "1024"),
            ("DOWNLOAD_CONCURRENCY", "8"),
            ("DOCKER_BIN", "/usr/local/bin/docker"),
            ("CTR_BIN", "/usr/local/bin/ctr"),
            ("INCUS_BIN", ""),
            ("VIRSH_BIN", "/usr/bin/virsh"),
            ("LIBVIRT_POOL", "images"),
            ("LXC_ROOTFS_DIR", "/srv/lxc"),
            ("LXC_AUTO_START", "true"),
            ("POLL_INTERVAL_SECS", "5"),
        ]);
        let config = AgentConfig::from_lookup(lookup_from(&pairs)).unwrap();

        assert_eq!(config.data_dir, PathBuf::from("/var/lib/qe"));
        assert_eq!(config.cache_dir, PathBuf::from("/var/cache/qe"));
        assert_eq!(config.cache_max_bytes, 1024);
        assert_eq!(config.download_concurrency, 8);
        assert_eq!(config.docker_bin, "/usr/local/bin/docker");
        assert_eq!(config.ctr_bin, "/usr/local/bin/ctr");
        // 显式置空 = 禁用 Incus（plain LXC 兜底），其余空白值仍视同未设置
        assert_eq!(config.incus_bin, "");
        assert_eq!(config.virsh_bin, "/usr/bin/virsh");
        assert_eq!(config.libvirt_pool, "images");
        assert_eq!(config.lxc_rootfs_dir, PathBuf::from("/srv/lxc"));
        assert!(config.lxc_auto_start);
        assert_eq!(config.local_policy.poll_interval_secs, 5);
        assert_eq!(config.local_policy.download_concurrency, 8);
        assert_eq!(config.local_policy.cache_max_bytes, 1024);
        assert!(config.local_policy.lxc_auto_start);
    }

    #[test]
    fn missing_mtls_endpoint_is_rejected() {
        let err = AgentConfig::from_lookup(lookup_from(&[
            ("DEVICE_SLOT", "slot-1"),
            ("AGENT_CONFIG_FILE", NO_FILE),
        ]))
        .expect_err("缺少 DEVICE_PKI_MTLS_URL 必须报错");
        assert!(err.to_string().contains("DEVICE_PKI_MTLS_URL"), "{err}");
    }

    #[test]
    fn missing_slot_is_rejected() {
        let err = AgentConfig::from_lookup(lookup_from(&[
            ("DEVICE_PKI_MTLS_URL", "https://node:4441"),
            ("AGENT_CONFIG_FILE", NO_FILE),
        ]))
        .expect_err("缺少 DEVICE_SLOT 必须报错");
        assert!(err.to_string().contains("DEVICE_SLOT"), "{err}");
    }

    #[test]
    fn blank_required_value_counts_as_missing() {
        let err = AgentConfig::from_lookup(lookup_from(&[
            ("DEVICE_PKI_MTLS_URL", "   "),
            ("DEVICE_SLOT", "slot-1"),
            ("AGENT_CONFIG_FILE", NO_FILE),
        ]))
        .expect_err("空白值应视为未设置");
        assert!(err.to_string().contains("DEVICE_PKI_MTLS_URL"), "{err}");
    }

    #[test]
    fn malformed_numbers_are_rejected_with_context() {
        let mut pairs = minimal();
        pairs.push(("CACHE_MAX_BYTES", "10GB"));
        let err = AgentConfig::from_lookup(lookup_from(&pairs)).unwrap_err();
        assert!(err.to_string().contains("CACHE_MAX_BYTES"), "{err}");

        let mut pairs = minimal();
        pairs.push(("DOWNLOAD_CONCURRENCY", "many"));
        let err = AgentConfig::from_lookup(lookup_from(&pairs)).unwrap_err();
        assert!(err.to_string().contains("DOWNLOAD_CONCURRENCY"), "{err}");

        let mut pairs = minimal();
        pairs.push(("DOWNLOAD_CONCURRENCY", "0"));
        let err = AgentConfig::from_lookup(lookup_from(&pairs)).unwrap_err();
        assert!(err.to_string().contains("DOWNLOAD_CONCURRENCY"), "{err}");
    }

    #[test]
    fn out_of_range_strategy_values_are_rejected() {
        let mut pairs = minimal();
        pairs.push(("POLL_INTERVAL_SECS", "0"));
        let err = AgentConfig::from_lookup(lookup_from(&pairs)).unwrap_err();
        assert!(err.to_string().contains("POLL_INTERVAL_SECS"), "{err}");

        let mut pairs = minimal();
        pairs.push(("POLL_INTERVAL_SECS", "61"));
        let err = AgentConfig::from_lookup(lookup_from(&pairs)).unwrap_err();
        assert!(err.to_string().contains("POLL_INTERVAL_SECS"), "{err}");

        let mut pairs = minimal();
        pairs.push(("DOWNLOAD_CONCURRENCY", "33"));
        let err = AgentConfig::from_lookup(lookup_from(&pairs)).unwrap_err();
        assert!(err.to_string().contains("DOWNLOAD_CONCURRENCY"), "{err}");

        let too_large = (MAX_CACHE_MAX_BYTES + 1).to_string();
        let mut pairs = minimal();
        pairs.push(("CACHE_MAX_BYTES", &too_large));
        let err = AgentConfig::from_lookup(lookup_from(&pairs)).unwrap_err();
        assert!(err.to_string().contains("CACHE_MAX_BYTES"), "{err}");
    }

    #[test]
    fn mtls_endpoint_must_be_a_url() {
        let err = AgentConfig::from_lookup(lookup_from(&[
            ("DEVICE_PKI_MTLS_URL", "node:4441"),
            ("DEVICE_SLOT", "slot-1"),
            ("AGENT_CONFIG_FILE", NO_FILE),
        ]))
        .expect_err("缺少 scheme 的端点必须拒绝");
        assert!(err.to_string().contains("http"), "{err}");
    }

    #[test]
    fn values_are_trimmed() {
        let config = AgentConfig::from_lookup(lookup_from(&[
            ("DEVICE_PKI_MTLS_URL", "  https://node:4441  "),
            ("DEVICE_SLOT", " slot-1\t"),
            ("AGENT_CONFIG_FILE", NO_FILE),
        ]))
        .unwrap();
        assert_eq!(config.mtls_endpoint, "https://node:4441");
        assert_eq!(config.slot, "slot-1");
    }

    #[test]
    fn config_file_defaults_to_data_dir() {
        let config = AgentConfig::from_lookup(lookup_from(&[
            ("DEVICE_PKI_MTLS_URL", "https://node:4441"),
            ("DEVICE_SLOT", "slot-1"),
            ("DEVICE_DATA_DIR", "/var/lib/qe-agent"),
        ]))
        .unwrap();
        assert_eq!(
            config.config_file,
            PathBuf::from("/var/lib/qe-agent/agent-config.json")
        );
        assert_eq!(
            config.config_file,
            config.data_dir.join(DEFAULT_CONFIG_FILE_NAME)
        );
    }
}
