//! 主机电源控制（`agent.command` 下行 / 本机 IPC / CLI）。
//!
//! 控制面只允许 `poweroff` / `reboot`；本机 IPC/CLI 额外支持伪关机。
//! 默认关闭：`QE_AGENT_ALLOW_POWEROFF=1` 才执行。
//!
//! 真正关机/重启前会先停 libvirt VM 和 Incus 容器（见 [`crate::host_power`]），
//! 避免 ACPI 不响应的 VM 把 host 卡在 systemd shutdown。

use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::host_power::{self, PowerAction};

/// `agent.command` payload。
#[derive(Debug, Deserialize)]
pub struct AgentCommandRequest {
    pub request_id: Uuid,
    pub op: String,
}

/// 执行结果（与控制面 `machine_command_results` 的通用结果形状一致）。
#[derive(Debug, Clone)]
pub struct AgentCommandResult {
    pub request_id: Uuid,
    pub op: String,
    pub ok: bool,
    pub data: Option<Value>,
    pub error: Option<String>,
}

/// 本机入口：agent-client 页面 / `qe-agent cli power`。
pub async fn execute_local(action: &str, graceful: bool) -> anyhow::Result<Value> {
    let action = PowerAction::parse(action)
        .ok_or_else(|| anyhow::anyhow!("不支持的动作：{action}（可选 shutdown/reboot/fake）"))?;
    host_power::execute(action, graceful).await
}

/// 解析并执行一条控制面 `agent.command`（仅 poweroff/reboot）。
pub async fn execute_payload(payload: &str) -> anyhow::Result<AgentCommandResult> {
    let request: AgentCommandRequest = serde_json::from_str(payload)
        .map_err(|error| anyhow::anyhow!("agent.command payload 非法: {error}"))?;
    let op = request.op.trim().to_ascii_lowercase();
    if op != "poweroff" && op != "reboot" {
        return Ok(AgentCommandResult {
            request_id: request.request_id,
            op,
            ok: false,
            data: None,
            error: Some("agent.command 仅支持 poweroff/reboot".to_owned()),
        });
    }

    let action = if op == "reboot" {
        PowerAction::Reboot
    } else {
        PowerAction::Shutdown
    };
    match host_power::execute(action, true).await {
        Ok(data) => Ok(AgentCommandResult {
            request_id: request.request_id,
            op,
            ok: true,
            data: Some(data),
            error: None,
        }),
        Err(error) => Ok(AgentCommandResult {
            request_id: request.request_id,
            op,
            ok: false,
            data: None,
            error: Some(error.to_string()),
        }),
    }
}

/// 供单元测试/诊断：当前是否允许执行主机电源命令。
pub fn allow_poweroff() -> bool {
    std::env::var("QE_AGENT_ALLOW_POWEROFF")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_invalid_op_without_touching_host() {
        let result = execute_payload(
            r#"{"request_id":"0b6f9d4e-4a41-4b6e-9d0b-4f8b0f3c2a11","op":"format-disk"}"#,
        )
        .await
        .unwrap();
        assert!(!result.ok);
        assert!(result.error.unwrap().contains("仅支持"));
    }

    #[test]
    fn parses_local_actions() {
        assert!(PowerAction::parse("shutdown").is_some());
        assert!(PowerAction::parse("fake").is_some());
        assert!(PowerAction::parse("nope").is_none());
    }
}
