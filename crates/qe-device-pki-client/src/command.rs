//! 设备命令结果回传：`POST /machine/command/result`（mTLS）。
//!
//! 契约（Phase1 VM 远程控制）：
//! - 控制面下行 `vm.command` 时在 `machine_command_results` 落一行 `pending`；
//! - agent 执行完（成功/失败/超时）用本接口回传 `{request_id, ok, data, error}`；
//! - 未知 request_id → 404（控制面会重试/告警）；已终态 → 200 幂等 no-op。

use crate::error::{Error, Result};
use crate::transport::{TransportConfig, build_mtls_client};
use serde::Deserialize;
use uuid::Uuid;

/// `POST /machine/command/result` 的响应。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CommandResultReply {
    #[serde(default)]
    pub ok: bool,
    /// 服务端处理结果：`ok` / `error` / `timeout` / `noop`。
    #[serde(default)]
    pub status: Option<String>,
}

/// 回传一次命令执行结果（mTLS，设备身份来自证书）。
pub async fn report_command_result(
    cfg: &TransportConfig,
    request_id: &Uuid,
    ok: bool,
    data: Option<&serde_json::Value>,
    error: Option<&str>,
) -> Result<CommandResultReply> {
    let client = build_mtls_client(cfg)?;
    let url = format!("{}/machine/command/result", cfg.mtls_endpoint);
    let body = build_body(request_id, ok, data, error);
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Transport(format!("回传命令结果失败（mTLS）: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::Unexpected(format!(
            "POST /machine/command/result 返回 {status}: {body}"
        )));
    }
    resp.json()
        .await
        .map_err(|e| Error::Transport(format!("解析命令结果响应失败: {e}")))
}

/// 构造请求体（单独抽出便于单测字段形状）。
pub fn build_body(
    request_id: &Uuid,
    ok: bool,
    data: Option<&serde_json::Value>,
    error: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "request_id": request_id,
        "ok": ok,
        "data": data,
        "error": error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_matches_contract() {
        let id = Uuid::parse_str("0b6f9d4e-4a41-4b6e-9d0b-4f8b0f3c2a11").unwrap();
        let data = serde_json::json!({"name": "vm-1", "result": "ok"});
        let body = build_body(&id, true, Some(&data), None);
        assert_eq!(body["request_id"], id.to_string());
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["name"], "vm-1");
        assert!(body["error"].is_null());
    }

    #[test]
    fn failure_body_keeps_error() {
        let id = Uuid::new_v4();
        let body = build_body(&id, false, None, Some("libvirt 不可用"));
        assert_eq!(body["ok"], false);
        assert_eq!(body["error"], "libvirt 不可用");
        assert!(body["data"].is_null());
    }

    #[test]
    fn reply_parses() {
        let reply: CommandResultReply =
            serde_json::from_str(r#"{"ok":true,"status":"ok"}"#).unwrap();
        assert!(reply.ok);
        assert_eq!(reply.status.as_deref(), Some("ok"));
        let noop: CommandResultReply =
            serde_json::from_str(r#"{"ok":true,"status":"noop"}"#).unwrap();
        assert_eq!(noop.status.as_deref(), Some("noop"));
    }
}
