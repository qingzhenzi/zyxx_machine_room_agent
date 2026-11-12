//! 宿主机电源动作（关机 / 重启 / 伪关机）。
//!
//! 所有路径共用同一套「先停虚拟机/容器，再真正断电」的编排：
//! - 控制面 `agent.command` 下行
//! - 本机 IPC（agent-client 页面）
//! - `qe-agent cli power`（无 GUI / 远程 SSH）
//!
//! 伪关机只写状态文件，不真正断电，也不会停 VM/容器；用于演示/测试。

use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};
use tokio::process::Command;

const CMD_TIMEOUT: Duration = Duration::from_secs(12);
const VM_SHUTDOWN_WAIT: Duration = Duration::from_secs(8);
const FAKE_FLAG: &str = "/run/qe-agent/fake-shutdown";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Shutdown,
    Reboot,
    FakeShutdown,
}

impl PowerAction {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "shutdown" | "poweroff" | "power_off" => Some(Self::Shutdown),
            "reboot" | "restart" => Some(Self::Reboot),
            "fake" | "fake_shutdown" | "fake-poweroff" | "pseudo" => Some(Self::FakeShutdown),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shutdown => "shutdown",
            Self::Reboot => "reboot",
            Self::FakeShutdown => "fake_shutdown",
        }
    }

    const fn systemctl_action(self) -> &'static str {
        match self {
            Self::Shutdown => "poweroff",
            Self::Reboot => "reboot",
            Self::FakeShutdown => "",
        }
    }
}

fn poweroff_allowed() -> bool {
    std::env::var("QE_AGENT_ALLOW_POWEROFF")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn program() -> String {
    std::env::var("QE_AGENT_POWEROFF_PROGRAM").unwrap_or_else(|_| "systemctl".to_owned())
}

fn delay_secs() -> u64 {
    std::env::var("QE_AGENT_POWEROFF_DELAY_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3)
        .min(60)
}

async fn run(program: &str, args: &[&str]) -> Option<String> {
    let output = tokio::time::timeout(CMD_TIMEOUT, Command::new(program).args(args).output())
        .await
        .ok()?
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        None
    }
}

async fn running_vms() -> Vec<String> {
    run("virsh", &["list", "--name", "--state-running"])
        .await
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

async fn running_containers() -> Vec<String> {
    let mut names = Vec::new();
    if let Some(out) = run("incus", &["list", "--format", "csv", "-c", "ns"]).await {
        for line in out.lines() {
            let mut fields = line.split(',');
            if let (Some(name), Some(state)) = (fields.next(), fields.next())
                && state.trim().eq_ignore_ascii_case("running")
            {
                names.push(name.trim().to_owned());
            }
        }
    }
    names
}

/// 关机/重启前重新武装 WOL。部分网卡驱动在 shutdown 时会清掉 Wake-on 设置，
/// 只在开机时跑 ethtool 不够，所以真正断电前再写一遍。
fn arm_wol() {
    let iface = std::env::var("QE_AGENT_WOL_INTERFACE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            let out = std::process::Command::new("ip")
                .args(["-o", "-4", "route", "show", "default"])
                .output()
                .ok()?;
            let text = String::from_utf8_lossy(&out.stdout);
            let dev = text
                .split("dev ")
                .nth(1)?
                .split_whitespace()
                .next()?
                .to_owned();
            Some(dev)
        });
    let Some(iface) = iface else { return };
    match std::process::Command::new("ethtool")
        .args(["-s", &iface, "wol", "g"])
        .status()
    {
        Ok(status) => tracing::info!(%iface, code = ?status.code(), "关机前已重新武装 WOL"),
        Err(error) => tracing::warn!(%iface, %error, "关机前 arm WOL 失败（ethtool 不存在？）"),
    }
    let wakeup = format!("/sys/class/net/{iface}/device/power/wakeup");
    if let Ok(current) = std::fs::read_to_string(&wakeup)
        && current.trim() != "enabled"
    {
        let _ = std::fs::write(&wakeup, "enabled");
    }
}

/// 停 VM / 容器；返回本机实际处理的名，供日志和页面展示。
pub async fn cleanup() -> Value {
    let vms = running_vms().await;
    for vm in &vms {
        let _ = run("virsh", &["shutdown", vm]).await;
    }
    if !vms.is_empty() {
        tokio::time::sleep(VM_SHUTDOWN_WAIT).await;
    }
    // ACPI 不响应的 VM（例：无可启动 OS）超时后强制关机，避免 host 卡死。
    for vm in &running_vms().await {
        let _ = run("virsh", &["destroy", vm]).await;
    }

    let containers = running_containers().await;
    for name in &containers {
        let _ = run("incus", &["stop", "-f", name]).await;
    }

    json!({ "vms": vms, "containers": containers })
}

pub fn fake_shutdown_active() -> bool {
    std::path::Path::new(FAKE_FLAG).exists()
}

pub fn clear_fake_shutdown() {
    let _ = std::fs::remove_file(FAKE_FLAG);
}

fn write_state(action: PowerAction, cleanup: Option<&Value>) -> Result<()> {
    let dir = std::path::Path::new("/run/qe-agent");
    std::fs::create_dir_all(dir).ok();
    let payload = json!({
        "action": action.as_str(),
        "at": chrono::Utc::now(),
        "cleanup": cleanup,
    });
    std::fs::write(
        dir.join("last-power.json"),
        serde_json::to_vec_pretty(&payload)?,
    )?;
    Ok(())
}

/// 执行一次电源动作。真正关机/重启时会先清理 VM/容器，再延迟调度 systemctl。
pub async fn execute(action: PowerAction, graceful: bool) -> Result<Value> {
    if !poweroff_allowed() {
        anyhow::bail!(
            "主机电源命令未启用：在 /etc/qe-agent.env 设置 QE_AGENT_ALLOW_POWEROFF=1 后重启 daemon"
        );
    }

    if action == PowerAction::FakeShutdown {
        std::fs::create_dir_all("/run/qe-agent").ok();
        std::fs::write(FAKE_FLAG, b"1")?;
        write_state(action, None)?;
        return Ok(json!({
            "action": action.as_str(),
            "fake": true,
            "cleanup": null,
            "message": "已进入伪关机状态（未真正断电，VM/容器保持运行）",
        }));
    }

    arm_wol();
    let cleanup = if graceful {
        Some(cleanup().await)
    } else {
        None
    };
    write_state(action, cleanup.as_ref())?;

    let program = program();
    let program_for_result = program.clone();
    let action_arg = action.systemctl_action();
    let delay = delay_secs();
    // 不能只 spawn 线程/任务：CLI 进程返回后会把它们一起结束。
    // 交给 systemd 的 transient timer，daemon / CLI 都能真正执行。
    let unit = format!("qe-agent-power-{}", std::process::id());
    let delay_arg = format!("--on-active={delay}s");
    let launched = std::process::Command::new("systemd-run")
        .args([
            "--quiet",
            "--collect",
            &delay_arg,
            "--unit",
            &unit,
            &program,
            action_arg,
        ])
        .status();
    if !matches!(launched, Ok(status) if status.success()) {
        // 兜底：setsid + nohup，至少脱离当前进程组。
        let _ = std::process::Command::new("setsid")
            .arg("sh")
            .arg("-c")
            .arg(format!("sleep {delay}; exec {program} {action_arg}"))
            .status();
    }

    Ok(json!({
        "action": action.as_str(),
        "fake": false,
        "cleanup": cleanup,
        "scheduled": true,
        "delay_secs": delay,
        "program": program_for_result,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_actions() {
        assert_eq!(PowerAction::parse("shutdown"), Some(PowerAction::Shutdown));
        assert_eq!(PowerAction::parse("reboot"), Some(PowerAction::Reboot));
        assert_eq!(PowerAction::parse("fake"), Some(PowerAction::FakeShutdown));
        assert_eq!(PowerAction::parse("format"), None);
    }
}
