//! `qe-nftables` — QEStudio agent 的 nftables 防火墙适配层。
//!
//! 目标：一条声明式防火墙配置（[`FirewallSpec`]），两种后端：
//!
//! * **cmd 后端**（默认）：把 [`FirewallSpec`] 渲染成 `nft -f -` 脚本并执行。
//!   只依赖系统里有 `nft` 二进制，适合设备首次部署。
//! * **json 后端**（feature `json-backend`）：通过 libnftables JSON API 原子应用。
//!
//! 设计原则：
//! * 幂等 — 每次应用都从自己的专用表（`inet qe_agent`）重建规则，
//!   不碰系统里其它表。
//! * 失败可降级 — [`apply`] 返回 [`Report`]，绝不 panic；
//!   防火墙失败不应阻止 agent 继续初始化（boot 流水线记录状态后继续）。
//! * 可查询 — [`Report`] 序列化为 JSON 后经 IPC 暴露给 GUI。

pub mod error;
pub mod render;
pub mod spec;

pub use error::Error;
pub use render::render_script;
pub use spec::{FirewallSpec, PortRule, ServicePreset};

/// 专用表：agent 拥有的所有规则都在这张表里，应用即整表重建。
pub const TABLE: &str = "qe_agent";

/// 一次应用的输入与输出记录（可序列化，供 IPC `/v1/state` 展示）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Report {
    /// 是否成功生效。
    pub applied: bool,
    /// 使用的后端：`cmd` | `json`。
    pub backend: String,
    /// 应用耗时（毫秒）。
    pub elapsed_ms: u64,
    /// 失败原因（成功时为空）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    /// 规则条数摘要。
    pub rules: RuleCounts,
}

/// 规则摘要。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RuleCounts {
    pub ports: usize,
    pub blocked: usize,
}

impl Report {
    fn failed(backend: &str, elapsed_ms: u64, error: impl std::fmt::Display) -> Self {
        Self {
            applied: false,
            backend: backend.to_owned(),
            elapsed_ms,
            error: error.to_string(),
            rules: RuleCounts::default(),
        }
    }
}

/// 应用防火墙配置（cmd 后端）。
///
/// 需要 root 或 `CAP_NET_ADMIN`。失败返回 [`Report`] 且 `applied == false`。
pub fn apply(spec: &FirewallSpec) -> Report {
    let started = std::time::Instant::now();
    let script = match render_script(spec) {
        Ok(script) => script,
        Err(error) => return Report::failed("cmd", 0, error),
    };

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let mut child = match Command::new("nft")
            .arg("-f")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => return Report::failed("cmd", 0, format!("启动 nft 失败: {error}")),
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(script.as_bytes());
        }
        match child.wait_with_output() {
            Ok(output) if output.status.success() => Report {
                applied: true,
                backend: "cmd".into(),
                elapsed_ms: started.elapsed().as_millis() as u64,
                error: String::new(),
                rules: RuleCounts {
                    ports: spec.ports.len(),
                    blocked: spec.blocked_ports.len(),
                },
            },
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Report::failed("cmd", started.elapsed().as_millis() as u64, stderr.trim())
            }
            Err(error) => Report::failed("cmd", started.elapsed().as_millis() as u64, error),
        }
    }

    #[cfg(not(unix))]
    {
        let _ = spec;
        Report::failed("cmd", 0, "仅支持 Unix 平台")
    }
}

/// 检查 `nft` 二进制是否可用。
pub fn nft_available() -> bool {
    which::which("nft").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use spec::PortRule;

    #[test]
    fn renders_and_counts() {
        let spec = FirewallSpec::default().with_preset(ServicePreset::Lan);
        let script = render_script(&spec).unwrap();
        assert!(script.contains(&format!("table inet {TABLE}")));
        assert!(script.contains("tcp dport 22 accept\n"));
        // 预设端口不写入 `ports`，由 effective_ports 展开产生。
        assert_eq!(spec.ports.len(), 0);
        assert_eq!(spec.effective_ports().len(), 6);
    }

    #[test]
    fn rejects_bad_port() {
        let spec = FirewallSpec::default()
            .with_preset(ServicePreset::Minimal)
            .with_port(PortRule::tcp(0));
        assert!(render_script(&spec).is_err());
    }
}
