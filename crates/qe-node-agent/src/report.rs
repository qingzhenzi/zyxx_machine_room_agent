//! 上行状态上报（契约 §16.1）：`transfer.progress` / `transfer.completed`。
//!
//! 上报走 `qe_device_pki_client::transport::send_message`（mTLS）。消息先写入
//! 持久化 outbox 再发送，成功后 ACK；发送失败时消息留在 outbox，下一次上报前
//! 会重放，因此**网络抖动不会丢状态**（服务端按 `(serial, seq)` 幂等去重）。

use std::sync::Arc;

use anyhow::{Context, Result};
use qe_device_pki_client::transport::{OutboxQueue, PendingMessage, TransportConfig, send_message};
use serde::{Deserialize, Serialize};

use crate::BoxFuture;

/// 上行消息 kind。
pub const KIND_PROGRESS: &str = "transfer.progress";
/// 上行消息 kind。
pub const KIND_COMPLETED: &str = "transfer.completed";

/// 传输阶段（契约里的 `phase`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Download,
    Verify,
    Import,
}

impl Phase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::Verify => "verify",
            Self::Import => "import",
        }
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 阶段内进度事件（pull / import 用同一套结构）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressEvent {
    pub phase: Phase,
    /// 0..=100，跨阶段单调（download 0-80、verify 80-90、import 90-100）。
    pub progress: i32,
    /// 当前阶段累计字节数。
    pub bytes_done: i64,
}

impl ProgressEvent {
    pub fn new(phase: Phase, progress: i32, bytes_done: i64) -> Self {
        Self {
            phase,
            progress: progress.clamp(0, 100),
            bytes_done,
        }
    }
}

/// `transfer.progress` 的 payload（§16.1，字段一个都不能少）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressPayload {
    pub transfer_id: String,
    pub phase: Phase,
    pub progress: i32,
    pub bytes_done: i64,
    pub error: Option<String>,
}

impl ProgressPayload {
    pub fn new(transfer_id: impl Into<String>, event: ProgressEvent) -> Self {
        Self {
            transfer_id: transfer_id.into(),
            phase: event.phase,
            progress: event.progress.clamp(0, 100),
            bytes_done: event.bytes_done,
            error: None,
        }
    }

    /// 带错误信息的上报（失败时把原因一起带给控制面）。
    pub fn with_error(
        transfer_id: impl Into<String>,
        event: ProgressEvent,
        error: impl Into<String>,
    ) -> Self {
        Self {
            error: Some(error.into()),
            ..Self::new(transfer_id, event)
        }
    }
}

/// `transfer.completed` 的 payload（§16.1）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletedPayload {
    pub transfer_id: String,
    pub ok: bool,
    pub manifest_digest: String,
    pub verified: bool,
    pub imported_ref: Option<String>,
    pub error: Option<String>,
}

impl CompletedPayload {
    /// 成功：`ok=true, verified=true`，必须带导入后的本地引用。
    pub fn success(
        transfer_id: impl Into<String>,
        manifest_digest: impl Into<String>,
        imported_ref: impl Into<String>,
    ) -> Self {
        Self {
            transfer_id: transfer_id.into(),
            ok: true,
            manifest_digest: manifest_digest.into(),
            verified: true,
            imported_ref: Some(imported_ref.into()),
            error: None,
        }
    }

    /// 失败：`ok=false, verified=false`，保留 manifest digest 便于服务端定位。
    pub fn failure(
        transfer_id: impl Into<String>,
        manifest_digest: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            transfer_id: transfer_id.into(),
            ok: false,
            manifest_digest: manifest_digest.into(),
            verified: false,
            imported_ref: None,
            error: Some(error.into()),
        }
    }
}

/// 上报器抽象。实现应保证"至少一次"（成功才从 outbox 删除）。
pub trait Reporter: Send + Sync {
    fn progress<'a>(&'a self, payload: &'a ProgressPayload) -> BoxFuture<'a, Result<()>>;

    fn completed<'a>(&'a self, payload: &'a CompletedPayload) -> BoxFuture<'a, Result<()>>;
}

/// 通过 device-pki mTLS 上报的实现（outbox + `POST /messages`）。
pub struct MtlsReporter {
    transport: TransportConfig,
    outbox: Arc<OutboxQueue>,
}

impl MtlsReporter {
    pub fn new(transport: TransportConfig, outbox: Arc<OutboxQueue>) -> Self {
        Self { transport, outbox }
    }

    /// 把一条消息持久化后发送，并顺带重放历史未 ACK 消息。
    async fn send(&self, kind: &str, payload: String) -> Result<()> {
        let seq = self
            .outbox
            .next_seq()
            .map_err(|error| anyhow::anyhow!("分配上行 seq 失败: {error}"))?;
        let message = PendingMessage {
            seq,
            kind: kind.to_owned(),
            payload,
        };
        self.outbox
            .enqueue(&message)
            .map_err(|error| anyhow::anyhow!("上行消息落盘失败: {error}"))?;
        self.flush().await;
        Ok(())
    }

    /// 顺序重放 outbox，成功一条 ACK 一条；失败即停（保持顺序）。
    ///
    /// 返回本次 ACK 的条数。失败不影响调用方：消息仍在 outbox 里等下次重放。
    pub async fn flush(&self) -> usize {
        let pending = match self.outbox.pending() {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!(error = %error, "读取 outbox 失败，跳过本轮重放");
                return 0;
            }
        };
        let mut acked = 0usize;
        for message in pending {
            match send_message(&self.transport, &message).await {
                Ok(_) => match self.outbox.ack(message.seq) {
                    Ok(()) => acked += 1,
                    Err(error) => {
                        tracing::warn!(
                            seq = message.seq,
                            error = %error,
                            "上行消息 ACK 落盘失败，可能重复上报"
                        );
                    }
                },
                Err(error) => {
                    tracing::warn!(
                        seq = message.seq,
                        kind = %message.kind,
                        error = %error,
                        "上行消息发送失败，保留在 outbox 等重放"
                    );
                    break;
                }
            }
        }
        acked
    }
}

impl Reporter for MtlsReporter {
    fn progress<'a>(&'a self, payload: &'a ProgressPayload) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let json = serde_json::to_string(payload).context("transfer.progress 序列化失败")?;
            self.send(KIND_PROGRESS, json).await
        })
    }

    fn completed<'a>(&'a self, payload: &'a CompletedPayload) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let json = serde_json::to_string(payload).context("transfer.completed 序列化失败")?;
            self.send(KIND_COMPLETED, json).await
        })
    }
}

/// 测试用上报器：记录所有 payload，供 job / pull 测试断言。
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    pub(crate) struct RecordingReporter {
        pub progress: Mutex<Vec<ProgressPayload>>,
        pub completed: Mutex<Vec<CompletedPayload>>,
    }

    impl RecordingReporter {
        pub(crate) fn progress_events(&self) -> Vec<ProgressPayload> {
            self.progress.lock().unwrap().clone()
        }

        pub(crate) fn completed_events(&self) -> Vec<CompletedPayload> {
            self.completed.lock().unwrap().clone()
        }
    }

    impl Reporter for RecordingReporter {
        fn progress<'a>(&'a self, payload: &'a ProgressPayload) -> BoxFuture<'a, Result<()>> {
            Box::pin(async move {
                self.progress.lock().unwrap().push(payload.clone());
                Ok(())
            })
        }

        fn completed<'a>(&'a self, payload: &'a CompletedPayload) -> BoxFuture<'a, Result<()>> {
            Box::pin(async move {
                self.completed.lock().unwrap().push(payload.clone());
                Ok(())
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::test_support::RecordingReporter;

    #[test]
    fn progress_payload_matches_contract() {
        let event = ProgressEvent::new(Phase::Download, 40, 123);
        let payload = ProgressPayload::new("t-1", event);
        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "transfer_id": "t-1",
                "phase": "download",
                "progress": 40,
                "bytes_done": 123,
                "error": null,
            })
        );

        let verify = serde_json::to_value(ProgressPayload::new(
            "t-1",
            ProgressEvent::new(Phase::Verify, 80, 123),
        ))
        .unwrap();
        assert_eq!(verify["phase"], "verify");
    }

    #[test]
    fn progress_phase_serializes_lowercase() {
        for (phase, text) in [
            (Phase::Download, "download"),
            (Phase::Verify, "verify"),
            (Phase::Import, "import"),
        ] {
            assert_eq!(serde_json::to_value(phase).unwrap(), text);
            assert_eq!(phase.as_str(), text);
        }
    }

    #[test]
    fn progress_is_clamped() {
        let payload = ProgressPayload::new("t", ProgressEvent::new(Phase::Download, 999, 0));
        assert_eq!(payload.progress, 100);
        let payload = ProgressPayload::new("t", ProgressEvent::new(Phase::Download, -5, 0));
        assert_eq!(payload.progress, 0);
    }

    #[test]
    fn progress_with_error_keeps_message() {
        let payload =
            ProgressPayload::with_error("t", ProgressEvent::new(Phase::Import, 90, 10), "boom");
        assert_eq!(payload.error.as_deref(), Some("boom"));
        assert_eq!(payload.progress, 90);
    }

    #[test]
    fn completed_success_payload_matches_contract() {
        let payload = CompletedPayload::success("t-1", "a".repeat(64), "docker:debian:12");
        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "transfer_id": "t-1",
                "ok": true,
                "manifest_digest": "a".repeat(64),
                "verified": true,
                "imported_ref": "docker:debian:12",
                "error": null,
            })
        );
    }

    #[test]
    fn completed_failure_payload_matches_contract() {
        let payload = CompletedPayload::failure("t-1", "b".repeat(64), "校验失败");
        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "transfer_id": "t-1",
                "ok": false,
                "manifest_digest": "b".repeat(64),
                "verified": false,
                "imported_ref": null,
                "error": "校验失败",
            })
        );
    }

    #[test]
    fn payloads_roundtrip() {
        let progress = ProgressPayload::new("t", ProgressEvent::new(Phase::Import, 95, 7));
        let encoded = serde_json::to_string(&progress).unwrap();
        assert_eq!(
            serde_json::from_str::<ProgressPayload>(&encoded).unwrap(),
            progress
        );

        let completed = CompletedPayload::success("t", "c".repeat(64), "lxc:debian");
        let encoded = serde_json::to_string(&completed).unwrap();
        assert_eq!(
            serde_json::from_str::<CompletedPayload>(&encoded).unwrap(),
            completed
        );
    }

    #[tokio::test]
    async fn recording_reporter_captures_reports() {
        let reporter = RecordingReporter::default();
        reporter
            .progress(&ProgressPayload::new(
                "t",
                ProgressEvent::new(Phase::Download, 10, 1),
            ))
            .await
            .unwrap();
        reporter
            .completed(&CompletedPayload::success("t", "d".repeat(64), "x"))
            .await
            .unwrap();
        assert_eq!(reporter.progress.lock().unwrap().len(), 1);
        assert_eq!(reporter.completed.lock().unwrap().len(), 1);
    }
}
