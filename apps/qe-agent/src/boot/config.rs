//! boot 阶段配置：来自 `<data_dir>/agent-config.json` 的 `boot` 段 +
//! 环境变量覆盖。文件不存在时全部用默认值。
//!
//! ```json
//! {
//!   "boot": {
//!     "display": { "enabled": true, "bin": "/usr/bin/boot-display" },
//!     "checks_interval_secs": 600
//!   }
//! }
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// 允许的检查间隔范围：30s ~ 24h。
pub const CHECKS_INTERVAL_RANGE: std::ops::RangeInclusive<u64> = 30..=86_400;
/// 默认周期检查间隔：10 分钟。
pub const DEFAULT_CHECKS_INTERVAL_SECS: u64 = 600;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", default)]
pub struct BootConfig {
    /// 展示层（boot-display）配置。
    pub display: DisplayConfig,
    /// 周期检查间隔（秒）。连上控制服务器后可被远端配置覆盖。
    pub checks_interval_secs: u64,
    /// 是否在 boot 时应用防火墙。
    pub firewall_enabled: bool,
}

impl Default for BootConfig {
    fn default() -> Self {
        Self {
            display: DisplayConfig::default(),
            checks_interval_secs: DEFAULT_CHECKS_INTERVAL_SECS,
            firewall_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", default)]
pub struct DisplayConfig {
    pub enabled: bool,
    /// 可执行文件路径；空 = 从 PATH 找 `boot-display`。
    pub bin: Option<PathBuf>,
    /// boot 流水线跑完后是否退出 boot-display（把桌面还给用户）。
    /// 默认 false（保留常驻展示层）；安装器会为桌面机器设为 true。
    pub exit_after_boot: bool,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bin: None,
            exit_after_boot: false,
        }
    }
}

impl BootConfig {
    /// 加载顺序（低 → 高）：默认值 → 配置文件 `boot` 段 → 环境变量。
    ///
    /// 环境变量：`QE_BOOT_DISPLAY`（开关）、`QE_BOOT_DISPLAY_BIN`（路径）、
    /// `QE_BOOT_CHECKS_INTERVAL_SECS`（间隔）。
    pub fn load(data_dir: &Path) -> Self {
        let mut config = Self::default();

        let file = data_dir.join("agent-config.json");
        if let Ok(raw) = std::fs::read_to_string(&file)
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw)
            && let Some(section) = value.get("boot")
            && let Ok(parsed) = serde_json::from_value::<BootConfig>(section.clone())
        {
            config = parsed;
        }

        if let Ok(value) = std::env::var("QE_BOOT_DISPLAY") {
            config.display.enabled = !matches!(value.as_str(), "0" | "false" | "off");
        }
        if let Some(bin) = std::env::var_os("QE_BOOT_DISPLAY_BIN") {
            config.display.bin = Some(PathBuf::from(bin));
        }
        if let Ok(value) = std::env::var("QE_BOOT_DISPLAY_EXIT_AFTER_BOOT") {
            config.display.exit_after_boot = matches!(value.as_str(), "1" | "true" | "on");
        }
        if let Ok(value) = std::env::var("QE_BOOT_CHECKS_INTERVAL_SECS")
            && let Ok(secs) = value.parse::<u64>()
        {
            config.checks_interval_secs = secs;
        }

        if !CHECKS_INTERVAL_RANGE.contains(&config.checks_interval_secs) {
            config.checks_interval_secs = DEFAULT_CHECKS_INTERVAL_SECS;
        }
        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let config = BootConfig::default();
        assert!(config.display.enabled);
        assert_eq!(config.checks_interval_secs, DEFAULT_CHECKS_INTERVAL_SECS);
        assert!(config.firewall_enabled);
    }

    #[test]
    fn parses_file_section() {
        let raw = r#"{
            "policy": {},
            "boot": {
                "display": { "enabled": false },
                "checks_interval_secs": 120,
                "firewall_enabled": false
            }
        }"#;
        let value: serde_json::Value = serde_json::from_str(raw).unwrap();
        let config: BootConfig = serde_json::from_value(value["boot"].clone()).unwrap();
        assert!(!config.display.enabled);
        assert_eq!(config.checks_interval_secs, 120);
        assert!(!config.firewall_enabled);
    }

    #[test]
    fn clamps_interval_file() {
        // 配置文件越界值回退默认（load 内 clamp）。
        let dir = std::env::temp_dir().join(format!("qe-boot-clamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent-config.json"),
            r#"{"boot": {"checks_interval_secs": 1}}"#,
        )
        .unwrap();
        let config = BootConfig::load(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(config.checks_interval_secs, DEFAULT_CHECKS_INTERVAL_SECS);
    }

    #[test]
    fn ignores_malformed_file() {
        let dir = std::env::temp_dir().join(format!("qe-boot-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent-config.json"), "not json{{").unwrap();
        let config = BootConfig::load(&dir);
        assert_eq!(config, BootConfig::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
