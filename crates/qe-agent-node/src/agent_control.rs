//! 主机电源控制（`agent.command` 下行）：只支持关机/重启，且默认关闭。
//!
//! 安全开关：
//! - `QE_AGENT_ALLOW_POWEROFF=1` 才允许执行；默认不设置 → 返回 error，不触碰主机。
//! - `QE_AGENT_POWEROFF_PROGRAM` 可覆盖执行程序（默认 `systemctl`），仅用于
//!   隔离环境/测试；生产不要改。
//! - `QE_AGENT_POWEROFF_DELAY_SECS` 默认 2 秒，给命令结果回传留时间。

use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

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

fn poweroff_allowed() -> bool {
    std::env::var("QE_AGENT_ALLOW_POWEROFF")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn poweroff_program() -> String {
    std::env::var("QE_AGENT_POWEROFF_PROGRAM").unwrap_or_else(|_| "systemctl".to_owned())
}

fn poweroff_delay_secs() -> u64 {
    std::env::var("QE_AGENT_POWEROFF_DELAY_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2)
        .min(60)
}

/// 解析并执行一条 `agent.command`（仅 poweroff/reboot）。
///
/// 成功时只**调度**电源动作，先让调用方回传命令结果，再延迟执行；
/// 失败（未启用/非法 op）不触碰系统。
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
    if !poweroff_allowed() {
        return Ok(AgentCommandResult {
            request_id: request.request_id,
            op,
            ok: false,
            data: None,
            error: Some(
                "主机电源命令未启用：在 /etc/qe-agent.env 设置 QE_AGENT_ALLOW_POWEROFF=1 后重启 daemon"
                    .to_owned(),
            ),
        });
    }

    let program = poweroff_program();
    let program_for_result = program.clone();
    let delay = poweroff_delay_secs();
    let action = op.clone();
    // 先返回结果给控制面；电源动作在后台延迟执行。
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(delay)).await;
        match tokio::process::Command::new(&program).arg(&action).status().await {
            Ok(status) => tracing::warn!(program = %program, action = %action, code = ?status.code(), "agent 电源命令已执行"),
            Err(error) => tracing::error!(program = %program, action = %action, %error, "agent 电源命令执行失败"),
        }
    });

    Ok(AgentCommandResult {
        request_id: request.request_id,
        op,
        ok: true,
        data: Some(json!({
            "scheduled": true,
            "delay_secs": delay,
            "program": program_for_result,
        })),
        error: None,
    })
}

/// 供单元测试/诊断：当前是否允许执行主机电源命令。
pub fn allow_poweroff() -> bool {
    poweroff_allowed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_invalid_op_without_touching_host() {
        let result = execute_payload(r#"{"request_id":"0b6f9d4e-4a41-4b6e-9d0b-4f8b0f3c2a11","op":"format-disk"}"#)
            .await
            .unwrap();
        assert!(!result.ok);
        assert!(result.error.unwrap().contains("仅支持"));
    }

    #[tokio::test]
    async fn disabled_by_default() {
        // 测试环境不应预设 QE_AGENT_ALLOW_POWEROFF；若显式设为 1 则跳过该断言。
        if !allow_poweroff() {
            let result = execute_payload(r#"{"request_id":"0b6f9d4e-4a41-4b6e-9d0b-4f8b0f3c2a11","op":"poweroff"}"#)
                .await
                .unwrap();
            assert!(!result.ok);
            assert!(result.error.unwrap().contains("未启用"));
        }
    }
}
