//! 声明式防火墙配置：agent 只描述"要开哪些端口/要挡哪些端口"。

use serde::{Deserialize, Serialize};

/// 常见部署预设。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ServicePreset {
    /// 局域网设备默认：SSH + P2P(Swarm 端口段) + 局域网视频。
    Lan,
    /// 最小化：只留 SSH。
    Minimal,
    /// 不做任何预设，完全由 `ports` 决定。
    #[default]
    None,
}

/// 单条放行规则。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortRule {
    pub protocol: Protocol,
    pub port: u16,
    /// 可选来源 CIDR，如 `192.168.0.0/16`。为空表示不限来源。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

impl PortRule {
    pub const fn tcp(port: u16) -> Self {
        Self {
            protocol: Protocol::Tcp,
            port,
            from: None,
        }
    }

    pub const fn udp(port: u16) -> Self {
        Self {
            protocol: Protocol::Udp,
            port,
            from: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

/// 完整防火墙声明。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirewallSpec {
    /// 是否启用（关闭时应用只清空专用表）。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 预设（先展开成端口，再合并 `ports`）。
    #[serde(default)]
    pub preset: ServicePreset,
    /// 放行端口。
    #[serde(default)]
    pub ports: Vec<PortRule>,
    /// 显式丢弃端口（优先于放行）。
    #[serde(default)]
    pub blocked_ports: Vec<PortRule>,
}

fn default_true() -> bool {
    true
}

impl Default for FirewallSpec {
    fn default() -> Self {
        Self {
            enabled: true,
            preset: ServicePreset::default(),
            ports: Vec::new(),
            blocked_ports: Vec::new(),
        }
    }
}

impl FirewallSpec {
    pub fn with_preset(mut self, preset: ServicePreset) -> Self {
        self.preset = preset;
        self
    }

    pub fn with_port(mut self, rule: PortRule) -> Self {
        self.ports.push(rule);
        self
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// 展开预设得到最终放行列表。
    pub fn effective_ports(&self) -> Vec<PortRule> {
        let mut ports = match self.preset {
            ServicePreset::Lan => vec![
                PortRule::tcp(22),    // SSH
                PortRule::udp(1337),  // qep2p Swarm 默认端口
                PortRule::tcp(47989), // lan_video 常用段示意
                PortRule::tcp(47990),
                PortRule::udp(47998),
                PortRule::udp(47999),
            ],
            ServicePreset::Minimal => vec![PortRule::tcp(22)],
            ServicePreset::None => Vec::new(),
        };
        ports.extend(self.ports.iter().cloned());
        ports
    }
}
