//! agent 配置：环境变量 + 命令行（契约 `docs/machine-telemetry.md` v1.1）。
//!
//! 优先级：显式 CLI > 环境变量 > 内置默认值。
//!
//! 两种上报模式（`REPORT_MODE`）：
//! - `mtls`（默认）：用 `DEVICE_PKI_MTLS_URL` + `DEVICE_SLOT` + `DEVICE_DATA_DIR`
//!   定位 `<data>/creds/<slot>` 的设备证书，`POST {mtls}/machine/report`；
//! - `bearer`（兼容旧部署）：`MACHINE_API_BASE` + `MACHINE_AGENT_TOKEN`。
//!
//! 两档采集间隔（远端策略可覆盖，见 [`crate::policy`]）：
//! - fast：内置 /proc、df、hwmon，默认 10s（5..=60）；
//! - full：额外含 platform-health / diagnose / qeaudit / docker / virsh，默认 300s（60..=3600）。
//!
//! 必填项：
//! - `DEVICE_SLOT`（或 `--device-slot`）；
//! - mTLS 模式：`DEVICE_PKI_MTLS_URL`（`--print` 模式下可省略，因为不会发请求）；
//! - Bearer 模式：`MACHINE_AGENT_TOKEN`（`--print` 模式下可省略）。
//!
//! 其余全部有默认值，便于在开发机上直接 `--once --print` 排查。
//! 外部工具可执行文件与参数都可配；`ROOM_AGENT_DISABLE_*` 可关闭单项。

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use qe_domain::agent_config::{
    DEFAULT_TELEMETRY_FAST_INTERVAL_SECS, DEFAULT_TELEMETRY_FULL_INTERVAL_SECS,
    TELEMETRY_FAST_INTERVAL_SECS_RANGE, TELEMETRY_FULL_INTERVAL_SECS_RANGE,
};

use crate::cli::Cli;

/// 默认 Bearer 上报基础地址（含 API 版本前缀）。
pub const DEFAULT_API_BASE: &str = "http://127.0.0.1:8080/api/v1";
/// fast 档默认上报间隔（秒）。
pub const DEFAULT_FAST_INTERVAL_SECS: u64 = DEFAULT_TELEMETRY_FAST_INTERVAL_SECS;
/// fast 档允许的上报间隔范围（秒）。
pub const FAST_INTERVAL_RANGE: std::ops::RangeInclusive<u64> = TELEMETRY_FAST_INTERVAL_SECS_RANGE;
/// full 档默认上报间隔（秒）。
pub const DEFAULT_FULL_INTERVAL_SECS: u64 = DEFAULT_TELEMETRY_FULL_INTERVAL_SECS;
/// full 档允许的上报间隔范围（秒）。
pub const FULL_INTERVAL_RANGE: std::ops::RangeInclusive<u64> = TELEMETRY_FULL_INTERVAL_SECS_RANGE;
/// 默认数据目录（设备凭证在 `<data>/creds/<slot>`）。
pub const DEFAULT_DATA_DIR: &str = "./data";
/// 默认单个外部工具/命令超时（秒）。qeaudit 全量扫描本机约 20s，给足 45s。
pub const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 45;
/// 默认 platform-diagnose 刷新间隔（秒）：结果缓存，不每轮都跑。
pub const DEFAULT_DIAGNOSE_INTERVAL_SECS: u64 = 60;
/// 默认 qeaudit 刷新间隔（秒）：全量安全扫描较重，5 分钟一次。
pub const DEFAULT_SECURITY_INTERVAL_SECS: u64 = 300;
/// 外部工具缓存间隔范围（秒，0 = 每轮都跑）。
pub const TOOL_CACHE_RANGE: std::ops::RangeInclusive<u64> = 0..=3600;
/// 允许的外部工具超时范围（秒）。
pub const TOOL_TIMEOUT_RANGE: std::ops::RangeInclusive<u64> = 1..=300;
/// `device_slot` 长度上限（契约 §2）。
pub const MAX_DEVICE_SLOT_CHARS: usize = 128;

/// 默认 platform-health 可执行文件。
pub const DEFAULT_PLATFORM_HEALTH_BIN: &str = "platform-health";
/// 默认 platform-diagnose 可执行文件。
pub const DEFAULT_PLATFORM_DIAGNOSE_BIN: &str = "platform-diagnose";
/// 默认 qeaudit 可执行文件。
pub const DEFAULT_QEAUDIT_BIN: &str = "qeaudit";
/// 默认 docker 可执行文件。
pub const DEFAULT_DOCKER_BIN: &str = "docker";
/// 默认 virsh 可执行文件。
pub const DEFAULT_VIRSH_BIN: &str = "virsh";

/// platform-health 默认参数：采集一次、附 top 进程。
pub const DEFAULT_PLATFORM_HEALTH_ARGS: &str = "collect --processes 10";
/// platform-diagnose 默认参数：generic 场景 3 个采样、JSON 输出。
pub const DEFAULT_PLATFORM_DIAGNOSE_ARGS: &str = "-s generic -d 3 --json";
/// qeaudit 默认参数：扫描并输出 JSON。
pub const DEFAULT_QEAUDIT_ARGS: &str = "scan --format json";

/// 上报模式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReportMode {
    /// device-pki mTLS 上报（默认）。
    #[default]
    Mtls,
    /// qe-api Bearer token 上报（旧部署/过渡期）。
    Bearer,
}

impl ReportMode {
    /// 序列化到日志/`sections.agent` 的名字。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mtls => "mtls",
            Self::Bearer => "bearer",
        }
    }

    /// 解析 `REPORT_MODE` / `--report-mode`。
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "mtls" => Ok(Self::Mtls),
            "bearer" => Ok(Self::Bearer),
            other => bail!("REPORT_MODE 只能是 mtls / bearer，实际 `{other}`"),
        }
    }
}

/// room-agent 运行配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// 上报模式（默认 mTLS）。
    pub report_mode: ReportMode,
    /// qe-api 基础地址（仅 Bearer 模式），如 `http://127.0.0.1:8080/api/v1`（不带末尾 `/`）。
    pub api_base: String,
    /// 上报 Bearer token（仅 Bearer 模式）；`--print` 模式下允许为 None。
    pub token: Option<String>,
    /// device-pki mTLS 端点（仅 mTLS 模式），如 `https://node:4441`（不带末尾 `/`）。
    pub mtls_endpoint: Option<String>,
    /// 数据目录（设备凭证在 `<data_dir>/creds/<slot>`）。
    pub data_dir: PathBuf,
    /// 设备槽位（1–128 字符）。
    pub device_slot: String,
    /// fast 档上报间隔（秒，5..=60）；远端策略可覆盖。
    pub fast_interval_secs: u64,
    /// full 档上报间隔（秒，60..=3600）；远端策略可覆盖。
    pub full_interval_secs: u64,
    /// 单个外部工具的硬超时（秒，默认 45）。
    pub tool_timeout_secs: u64,
    /// platform-diagnose 结果缓存/刷新间隔（秒，0 = 每轮）。
    pub diagnose_interval_secs: u64,
    /// qeaudit 结果缓存/刷新间隔（秒，0 = 每轮）。
    pub security_interval_secs: u64,
    /// platform-health 可执行文件。
    pub platform_health_bin: String,
    /// platform-health 参数。
    pub platform_health_args: Vec<String>,
    /// platform-diagnose 可执行文件。
    pub platform_diagnose_bin: String,
    /// platform-diagnose 参数。
    pub platform_diagnose_args: Vec<String>,
    /// qeaudit 可执行文件。
    pub qeaudit_bin: String,
    /// qeaudit 参数。
    pub qeaudit_args: Vec<String>,
    /// docker 可执行文件。
    pub docker_bin: String,
    /// virsh 可执行文件。
    pub virsh_bin: String,
    /// 关闭 platform-health 采集。
    pub disable_health: bool,
    /// 关闭 platform-diagnose 采集。
    pub disable_diagnose: bool,
    /// 关闭 qeaudit 安全扫描。
    pub disable_security: bool,
    /// 关闭 docker 容器列表。
    pub disable_docker: bool,
    /// 关闭 virsh 虚拟机列表。
    pub disable_vms: bool,
    /// `--once`：只采集/上报一份 full 后退出。
    pub once: bool,
    /// `--print`：打印 payload 而不发送。
    pub print: bool,
}

impl Config {
    /// 从进程环境 + CLI 读取配置。
    pub fn from_env(cli: &Cli) -> Result<Self> {
        Self::from_lookup(cli, |key| env::var(key).ok())
    }

    /// 用给定查找函数解析配置（测试用，无需改写进程环境）。
    ///
    /// 空白值视同未设置；查不到时用内置默认值。
    pub fn from_lookup<F>(cli: &Cli, lookup: F) -> Result<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        let get = |key: &str| -> Option<String> {
            lookup(key)
                .map(|raw| raw.trim().to_owned())
                .filter(|value| !value.is_empty())
        };

        let report_mode = match cli.report_mode.clone().or_else(|| get("REPORT_MODE")) {
            Some(raw) => ReportMode::parse(&raw)?,
            None => ReportMode::Mtls,
        };

        let api_base = get("MACHINE_API_BASE").unwrap_or_else(|| DEFAULT_API_BASE.to_owned());
        let api_base = api_base.trim_end_matches('/').to_owned();
        let mtls_endpoint =
            get("DEVICE_PKI_MTLS_URL").map(|raw| raw.trim_end_matches('/').to_owned());

        // 只在对应模式下发请求时才需要端点/凭据；--print 只打印 JSON，不需要。
        if !cli.print {
            match report_mode {
                ReportMode::Mtls => {
                    if mtls_endpoint.is_none() {
                        bail!(
                            "mTLS 模式缺少必填环境变量 DEVICE_PKI_MTLS_URL（或改用 REPORT_MODE=bearer，或用 --print）"
                        );
                    }
                }
                ReportMode::Bearer => {}
            }
        }
        if let Some(endpoint) = &mtls_endpoint
            && !(endpoint.starts_with("http://") || endpoint.starts_with("https://"))
        {
            bail!("DEVICE_PKI_MTLS_URL 必须是以 http:// 或 https:// 开头的 URL，实际 `{endpoint}`");
        }
        if report_mode == ReportMode::Bearer
            && !(api_base.starts_with("http://") || api_base.starts_with("https://"))
        {
            bail!("MACHINE_API_BASE 必须是以 http:// 或 https:// 开头的 URL，实际 `{api_base}`");
        }

        let mut token = get("MACHINE_AGENT_TOKEN");
        if report_mode == ReportMode::Mtls {
            // mTLS 模式不使用 token；显式配置了也不报错，只是忽略。
            token = None;
        } else if !cli.print && token.is_none() {
            bail!("Bearer 模式缺少必填环境变量 MACHINE_AGENT_TOKEN（或改用 --print 只打印不发送）");
        }

        let device_slot = match cli.device_slot.clone().or_else(|| get("DEVICE_SLOT")) {
            Some(slot) => slot,
            None => bail!("缺少必填环境变量 DEVICE_SLOT（或传 --device-slot）"),
        };
        let slot_len = device_slot.chars().count();
        if slot_len == 0 || slot_len > MAX_DEVICE_SLOT_CHARS {
            bail!("DEVICE_SLOT 长度必须在 1..={MAX_DEVICE_SLOT_CHARS} 字符之间，实际 {slot_len}");
        }

        let data_dir = cli
            .data_dir
            .clone()
            .or_else(|| get("DEVICE_DATA_DIR").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR));
        if data_dir.as_os_str().is_empty() {
            bail!("DEVICE_DATA_DIR 不能为空");
        }

        // fast 档：--fast-interval > --interval（旧）> ROOM_AGENT_FAST_INTERVAL_SECS
        //          > MACHINE_REPORT_INTERVAL_SECS（旧）> 默认 10s。
        let fast_interval_secs = match cli.fast_interval_secs.or(cli.interval_secs) {
            Some(value) => value,
            None => match interval_env(&get, "ROOM_AGENT_FAST_INTERVAL_SECS")? {
                Some(value) => value,
                None => match interval_env(&get, "MACHINE_REPORT_INTERVAL_SECS")? {
                    Some(value) => value,
                    None => DEFAULT_FAST_INTERVAL_SECS,
                },
            },
        };
        if !FAST_INTERVAL_RANGE.contains(&fast_interval_secs) {
            bail!(
                "fast 上报间隔必须在 {}..={} 秒之间，实际 {fast_interval_secs}",
                FAST_INTERVAL_RANGE.start(),
                FAST_INTERVAL_RANGE.end()
            );
        }

        // full 档：--full-interval > ROOM_AGENT_FULL_INTERVAL_SECS > 默认 300s。
        let full_interval_secs = match cli.full_interval_secs {
            Some(value) => value,
            None => match interval_env(&get, "ROOM_AGENT_FULL_INTERVAL_SECS")? {
                Some(value) => value,
                None => DEFAULT_FULL_INTERVAL_SECS,
            },
        };
        if !FULL_INTERVAL_RANGE.contains(&full_interval_secs) {
            bail!(
                "full 上报间隔必须在 {}..={} 秒之间，实际 {full_interval_secs}",
                FULL_INTERVAL_RANGE.start(),
                FULL_INTERVAL_RANGE.end()
            );
        }
        // 范围本身已保证 fast <= 60 <= full（再兜一层，防止常量将来被改坏）。
        if full_interval_secs < fast_interval_secs {
            bail!(
                "full 上报间隔（{full_interval_secs}s）不能小于 fast 上报间隔（{fast_interval_secs}s）"
            );
        }

        let tool_timeout_secs = match get("ROOM_AGENT_TOOL_TIMEOUT_SECS") {
            Some(raw) => raw.parse::<u64>().map_err(|_| {
                anyhow::anyhow!("ROOM_AGENT_TOOL_TIMEOUT_SECS 不是合法整数: `{raw}`")
            })?,
            None => DEFAULT_TOOL_TIMEOUT_SECS,
        };
        if !TOOL_TIMEOUT_RANGE.contains(&tool_timeout_secs) {
            bail!(
                "ROOM_AGENT_TOOL_TIMEOUT_SECS 必须在 {}..={} 秒之间，实际 {tool_timeout_secs}",
                TOOL_TIMEOUT_RANGE.start(),
                TOOL_TIMEOUT_RANGE.end()
            );
        }

        let diagnose_interval_secs = cached_interval(
            &get,
            "ROOM_AGENT_DIAGNOSE_INTERVAL_SECS",
            DEFAULT_DIAGNOSE_INTERVAL_SECS,
        )?;
        let security_interval_secs = cached_interval(
            &get,
            "ROOM_AGENT_SECURITY_INTERVAL_SECS",
            DEFAULT_SECURITY_INTERVAL_SECS,
        )?;

        Ok(Self {
            report_mode,
            api_base,
            token,
            mtls_endpoint,
            data_dir,
            device_slot,
            fast_interval_secs,
            full_interval_secs,
            tool_timeout_secs,
            diagnose_interval_secs,
            security_interval_secs,
            platform_health_bin: string_or(
                &get,
                "PLATFORM_HEALTH_BIN",
                DEFAULT_PLATFORM_HEALTH_BIN,
            ),
            platform_health_args: args_or(
                &get,
                "PLATFORM_HEALTH_ARGS",
                DEFAULT_PLATFORM_HEALTH_ARGS,
            ),
            platform_diagnose_bin: string_or(
                &get,
                "PLATFORM_DIAGNOSE_BIN",
                DEFAULT_PLATFORM_DIAGNOSE_BIN,
            ),
            platform_diagnose_args: args_or(
                &get,
                "PLATFORM_DIAGNOSE_ARGS",
                DEFAULT_PLATFORM_DIAGNOSE_ARGS,
            ),
            qeaudit_bin: string_or(&get, "QEAUDIT_BIN", DEFAULT_QEAUDIT_BIN),
            qeaudit_args: args_or(&get, "QEAUDIT_ARGS", DEFAULT_QEAUDIT_ARGS),
            docker_bin: string_or(&get, "DOCKER_BIN", DEFAULT_DOCKER_BIN),
            virsh_bin: string_or(&get, "VIRSH_BIN", DEFAULT_VIRSH_BIN),
            disable_health: bool_or(&get, "ROOM_AGENT_DISABLE_HEALTH")?,
            disable_diagnose: bool_or(&get, "ROOM_AGENT_DISABLE_DIAGNOSE")?,
            disable_security: bool_or(&get, "ROOM_AGENT_DISABLE_SECURITY")?,
            disable_docker: bool_or(&get, "ROOM_AGENT_DISABLE_DOCKER")?,
            disable_vms: bool_or(&get, "ROOM_AGENT_DISABLE_VMS")?,
            once: cli.once,
            print: cli.print,
        })
    }

    /// 是否 mTLS 上报模式。
    pub fn is_mtls(&self) -> bool {
        self.report_mode == ReportMode::Mtls
    }

    /// fast 档上报周期。
    pub fn fast_interval(&self) -> Duration {
        Duration::from_secs(self.fast_interval_secs)
    }

    /// full 档上报周期。
    pub fn full_interval(&self) -> Duration {
        Duration::from_secs(self.full_interval_secs)
    }

    /// 设备凭证目录：`<data_dir>/creds/<slot>`（与 device-pki-client 一致）。
    pub fn creds_dir(&self) -> Result<PathBuf> {
        qe_device_pki_client::creds::locate(&self.device_slot, Some(&self.data_dir))
            .map_err(|error| anyhow::anyhow!("{error}"))
    }
}

/// 读取整数型间隔环境变量；未设置返回 `None`，非法值报错。
fn interval_env<F>(get: &F, key: &str) -> Result<Option<u64>>
where
    F: Fn(&str) -> Option<String>,
{
    match get(key) {
        Some(raw) => raw
            .parse::<u64>()
            .map(Some)
            .map_err(|_| anyhow::anyhow!("{key} 不是合法整数: `{raw}`")),
        None => Ok(None),
    }
}

fn string_or<F>(get: &F, key: &str, default: &str) -> String
where
    F: Fn(&str) -> Option<String>,
{
    get(key).unwrap_or_else(|| default.to_owned())
}

/// 读取参数串并按空白切分；显式空值回退默认参数。
fn args_or<F>(get: &F, key: &str, default: &str) -> Vec<String>
where
    F: Fn(&str) -> Option<String>,
{
    get(key)
        .unwrap_or_else(|| default.to_owned())
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

fn bool_or<F>(get: &F, key: &str) -> Result<bool>
where
    F: Fn(&str) -> Option<String>,
{
    match get(key) {
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" | "y" => Ok(true),
            "0" | "false" | "no" | "off" | "n" => Ok(false),
            other => bail!("{key} 不是合法布尔值: `{other}`（可用 true/false/1/0）"),
        },
        None => Ok(false),
    }
}

/// 解析「缓存刷新间隔」环境变量：0 表示每轮都跑。
fn cached_interval<F>(get: &F, key: &str, default: u64) -> Result<u64>
where
    F: Fn(&str) -> Option<String>,
{
    let value = match get(key) {
        Some(raw) => raw
            .parse::<u64>()
            .map_err(|_| anyhow::anyhow!("{key} 不是合法整数: `{raw}`"))?,
        None => default,
    };
    if !TOOL_CACHE_RANGE.contains(&value) {
        bail!(
            "{key} 必须在 {}..={} 秒之间，实际 {value}",
            TOOL_CACHE_RANGE.start(),
            TOOL_CACHE_RANGE.end()
        );
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn cli() -> Cli {
        Cli::default()
    }

    /// mTLS 模式的最小必填项。
    fn minimal() -> Vec<(&'static str, &'static str)> {
        vec![
            ("DEVICE_PKI_MTLS_URL", "https://node:4441"),
            ("DEVICE_SLOT", "room-101"),
        ]
    }

    fn bearer_minimal() -> Vec<(&'static str, &'static str)> {
        vec![
            ("REPORT_MODE", "bearer"),
            ("MACHINE_AGENT_TOKEN", "fleet-token"),
            ("DEVICE_SLOT", "room-101"),
        ]
    }

    #[test]
    fn defaults_apply_when_only_required_are_set() {
        let config = Config::from_lookup(&cli(), lookup_from(&minimal())).unwrap();
        assert_eq!(config.report_mode, ReportMode::Mtls);
        assert_eq!(config.mtls_endpoint.as_deref(), Some("https://node:4441"));
        assert_eq!(config.api_base, DEFAULT_API_BASE);
        assert!(config.token.is_none());
        assert_eq!(config.device_slot, "room-101");
        assert_eq!(config.data_dir, PathBuf::from(DEFAULT_DATA_DIR));
        assert_eq!(config.fast_interval_secs, DEFAULT_FAST_INTERVAL_SECS);
        assert_eq!(config.fast_interval_secs, 10);
        assert_eq!(config.full_interval_secs, DEFAULT_FULL_INTERVAL_SECS);
        assert_eq!(config.full_interval_secs, 300);
        assert_eq!(config.tool_timeout_secs, DEFAULT_TOOL_TIMEOUT_SECS);
        assert_eq!(config.platform_health_bin, "platform-health");
        assert_eq!(
            config.platform_health_args,
            vec!["collect", "--processes", "10"]
        );
        assert_eq!(config.platform_diagnose_bin, "platform-diagnose");
        assert_eq!(
            config.platform_diagnose_args,
            vec!["-s", "generic", "-d", "3", "--json"]
        );
        assert_eq!(config.qeaudit_bin, "qeaudit");
        assert_eq!(config.qeaudit_args, vec!["scan", "--format", "json"]);
        assert_eq!(config.docker_bin, "docker");
        assert_eq!(config.virsh_bin, "virsh");
        assert!(!config.disable_health);
        assert!(!config.disable_diagnose);
        assert!(!config.disable_security);
        assert!(!config.disable_docker);
        assert!(!config.disable_vms);
        assert!(!config.once);
        assert!(!config.print);
        assert_eq!(
            config.creds_dir().unwrap(),
            PathBuf::from("./data/creds/room-101")
        );
    }

    #[test]
    fn overrides_are_parsed() {
        let mut pairs = minimal();
        pairs.extend([
            ("REPORT_MODE", "bearer"),
            ("MACHINE_API_BASE", "https://qe.example.com/api/v1/"),
            ("MACHINE_AGENT_TOKEN", "fleet-token"),
            ("ROOM_AGENT_FAST_INTERVAL_SECS", "20"),
            ("ROOM_AGENT_FULL_INTERVAL_SECS", "1200"),
            ("DEVICE_DATA_DIR", "/var/lib/qe"),
            ("ROOM_AGENT_TOOL_TIMEOUT_SECS", "45"),
            ("PLATFORM_HEALTH_BIN", "/opt/qe/platform-health"),
            ("PLATFORM_HEALTH_ARGS", "collect --processes 3 --no-users"),
            ("PLATFORM_DIAGNOSE_BIN", "/opt/qe/platform-diagnose"),
            ("PLATFORM_DIAGNOSE_ARGS", "-s storage -d 1 --json"),
            ("QEAUDIT_BIN", "/opt/qe/qeaudit"),
            ("QEAUDIT_ARGS", "scan --profile cis --format json"),
            ("DOCKER_BIN", "/usr/local/bin/docker"),
            ("VIRSH_BIN", "/usr/bin/virsh"),
            ("ROOM_AGENT_DISABLE_HEALTH", "true"),
            ("ROOM_AGENT_DISABLE_DIAGNOSE", "1"),
            ("ROOM_AGENT_DISABLE_SECURITY", "on"),
            ("ROOM_AGENT_DISABLE_DOCKER", "yes"),
            ("ROOM_AGENT_DISABLE_VMS", "TRUE"),
        ]);
        let config = Config::from_lookup(&cli(), lookup_from(&pairs)).unwrap();

        // 末尾斜杠被去掉，避免拼出 `//machine/report`。
        assert_eq!(config.api_base, "https://qe.example.com/api/v1");
        assert_eq!(config.report_mode, ReportMode::Bearer);
        assert_eq!(config.token.as_deref(), Some("fleet-token"));
        assert_eq!(config.fast_interval_secs, 20);
        assert_eq!(config.full_interval_secs, 1200);
        assert_eq!(config.data_dir, PathBuf::from("/var/lib/qe"));
        assert_eq!(config.tool_timeout_secs, 45);
        assert_eq!(config.platform_health_bin, "/opt/qe/platform-health");
        assert_eq!(
            config.platform_health_args,
            vec!["collect", "--processes", "3", "--no-users"]
        );
        assert_eq!(
            config.platform_diagnose_args,
            vec!["-s", "storage", "-d", "1", "--json"]
        );
        assert_eq!(
            config.qeaudit_args,
            vec!["scan", "--profile", "cis", "--format", "json"]
        );
        assert_eq!(config.docker_bin, "/usr/local/bin/docker");
        assert_eq!(config.virsh_bin, "/usr/bin/virsh");
        assert!(config.disable_health);
        assert!(config.disable_diagnose);
        assert!(config.disable_security);
        assert!(config.disable_docker);
        assert!(config.disable_vms);
    }

    #[test]
    fn legacy_interval_env_maps_to_fast() {
        let mut pairs = minimal();
        pairs.push(("MACHINE_REPORT_INTERVAL_SECS", "30"));
        let config = Config::from_lookup(&cli(), lookup_from(&pairs)).unwrap();
        assert_eq!(config.fast_interval_secs, 30);
        assert_eq!(config.full_interval_secs, DEFAULT_FULL_INTERVAL_SECS);
    }

    #[test]
    fn cli_overrides_env() {
        let cli = Cli {
            once: true,
            print: true,
            interval_secs: Some(15),
            fast_interval_secs: Some(25),
            full_interval_secs: Some(900),
            report_mode: Some("mtls".to_owned()),
            device_slot: Some("room-cli".to_owned()),
            data_dir: Some(PathBuf::from("/tmp/qe-cli")),
            help: false,
        };
        let mut pairs = minimal();
        pairs.push(("ROOM_AGENT_FAST_INTERVAL_SECS", "30"));
        pairs.push(("ROOM_AGENT_FULL_INTERVAL_SECS", "600"));
        pairs.push(("DEVICE_DATA_DIR", "/tmp/qe-env"));
        let config = Config::from_lookup(&cli, lookup_from(&pairs)).unwrap();
        assert_eq!(config.fast_interval_secs, 25);
        assert_eq!(config.full_interval_secs, 900);
        assert_eq!(config.device_slot, "room-cli");
        assert_eq!(config.data_dir, PathBuf::from("/tmp/qe-cli"));
        assert!(config.once);
        assert!(config.print);
    }

    #[test]
    fn legacy_interval_cli_maps_to_fast() {
        let cli = Cli {
            interval_secs: Some(15),
            ..Cli::default()
        };
        let config = Config::from_lookup(&cli, lookup_from(&minimal())).unwrap();
        assert_eq!(config.fast_interval_secs, 15);
    }

    #[test]
    fn missing_token_is_rejected_unless_print() {
        let pairs = [("REPORT_MODE", "bearer"), ("DEVICE_SLOT", "room-101")];
        let err = Config::from_lookup(&cli(), lookup_from(&pairs)).unwrap_err();
        assert!(err.to_string().contains("MACHINE_AGENT_TOKEN"), "{err}");

        let print = Cli {
            print: true,
            once: true,
            ..Cli::default()
        };
        let config = Config::from_lookup(&print, lookup_from(&pairs)).unwrap();
        assert!(config.token.is_none());
        assert_eq!(config.report_mode, ReportMode::Bearer);
    }

    #[test]
    fn mtls_endpoint_is_required_unless_print() {
        let pairs = [("DEVICE_SLOT", "room-101")];
        let err = Config::from_lookup(&cli(), lookup_from(&pairs)).unwrap_err();
        assert!(err.to_string().contains("DEVICE_PKI_MTLS_URL"), "{err}");

        let print = Cli {
            print: true,
            once: true,
            ..Cli::default()
        };
        let config = Config::from_lookup(&print, lookup_from(&pairs)).unwrap();
        assert_eq!(config.report_mode, ReportMode::Mtls);
        assert!(config.mtls_endpoint.is_none());
        assert!(config.token.is_none());
    }

    #[test]
    fn missing_slot_is_rejected() {
        let err = Config::from_lookup(
            &cli(),
            lookup_from(&[("DEVICE_PKI_MTLS_URL", "https://node:4441")]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("DEVICE_SLOT"), "{err}");
    }

    #[test]
    fn blank_values_count_as_unset() {
        let err = Config::from_lookup(
            &cli(),
            lookup_from(&[("DEVICE_PKI_MTLS_URL", "  "), ("DEVICE_SLOT", "room-101")]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("DEVICE_PKI_MTLS_URL"), "{err}");
    }

    #[test]
    fn invalid_intervals_are_rejected() {
        for (key, raw) in [
            ("ROOM_AGENT_FAST_INTERVAL_SECS", "abc"),
            ("ROOM_AGENT_FAST_INTERVAL_SECS", "4"),
            ("ROOM_AGENT_FAST_INTERVAL_SECS", "61"),
            ("ROOM_AGENT_FULL_INTERVAL_SECS", "abc"),
            ("ROOM_AGENT_FULL_INTERVAL_SECS", "59"),
            ("ROOM_AGENT_FULL_INTERVAL_SECS", "3601"),
            ("MACHINE_REPORT_INTERVAL_SECS", "4"),
        ] {
            let mut pairs = minimal();
            pairs.push((key, raw));
            let error = Config::from_lookup(&cli(), lookup_from(&pairs)).unwrap_err();
            let message = error.to_string();
            assert!(
                message.contains(key) || message.contains("上报间隔"),
                "{key}={raw}: {message}"
            );
        }

        let cli = Cli {
            fast_interval_secs: Some(4),
            ..Cli::default()
        };
        let err = Config::from_lookup(&cli, lookup_from(&minimal())).unwrap_err();
        assert!(err.to_string().contains("fast 上报间隔"), "{err}");
    }

    #[test]
    fn invalid_tool_timeout_is_rejected() {
        let mut pairs = minimal();
        pairs.push(("ROOM_AGENT_TOOL_TIMEOUT_SECS", "abc"));
        let err = Config::from_lookup(&cli(), lookup_from(&pairs)).unwrap_err();
        assert!(
            err.to_string().contains("ROOM_AGENT_TOOL_TIMEOUT_SECS"),
            "{err}"
        );

        for raw in ["0", "301"] {
            let mut pairs = minimal();
            pairs.push(("ROOM_AGENT_TOOL_TIMEOUT_SECS", raw));
            let err = Config::from_lookup(&cli(), lookup_from(&pairs)).unwrap_err();
            assert!(
                err.to_string().contains("ROOM_AGENT_TOOL_TIMEOUT_SECS"),
                "{raw}: {err}"
            );
        }
    }

    #[test]
    fn invalid_bool_url_and_mode_are_rejected() {
        let mut pairs = minimal();
        pairs.push(("ROOM_AGENT_DISABLE_DOCKER", "maybe"));
        let err = Config::from_lookup(&cli(), lookup_from(&pairs)).unwrap_err();
        assert!(
            err.to_string().contains("ROOM_AGENT_DISABLE_DOCKER"),
            "{err}"
        );

        let mut pairs = minimal();
        pairs.push(("DEVICE_PKI_MTLS_URL", "node:4441"));
        let err = Config::from_lookup(&cli(), lookup_from(&pairs)).unwrap_err();
        assert!(err.to_string().contains("DEVICE_PKI_MTLS_URL"), "{err}");

        let mut pairs = minimal();
        pairs.push(("REPORT_MODE", "ftp"));
        let err = Config::from_lookup(&cli(), lookup_from(&pairs)).unwrap_err();
        assert!(err.to_string().contains("REPORT_MODE"), "{err}");

        let mut pairs = bearer_minimal();
        pairs.push(("MACHINE_API_BASE", "127.0.0.1:8080"));
        let err = Config::from_lookup(&cli(), lookup_from(&pairs)).unwrap_err();
        assert!(err.to_string().contains("MACHINE_API_BASE"), "{err}");
    }

    #[test]
    fn overly_long_slot_is_rejected() {
        let slot = "x".repeat(MAX_DEVICE_SLOT_CHARS + 1);
        let err = Config::from_lookup(
            &cli(),
            lookup_from(&[
                ("DEVICE_PKI_MTLS_URL", "https://node:4441"),
                ("DEVICE_SLOT", &slot),
            ]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("DEVICE_SLOT"), "{err}");
    }

    #[test]
    fn report_mode_parse_and_as_str_roundtrip() {
        assert_eq!(ReportMode::parse("MTLS").unwrap(), ReportMode::Mtls);
        assert_eq!(ReportMode::parse(" bearer ").unwrap(), ReportMode::Bearer);
        assert!(ReportMode::parse("other").is_err());
        assert_eq!(ReportMode::default(), ReportMode::Mtls);
        assert_eq!(ReportMode::Mtls.as_str(), "mtls");
        assert_eq!(ReportMode::Bearer.as_str(), "bearer");
    }
}
