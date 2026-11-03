//! mTLS 持久化通讯 + 防抖重连 + 未 ACK 消息重放。
//!
//! 设计要点：
//! - **首次 mTLS 握手成功才算通讯打通**（`POST /enroll/confirm`）
//! - **指数退避**：100ms → 30s 上限，带 jitter，避免重连风暴
//! - **消息可靠性**：出站消息单调 `seq`，写 sled 后才发送，收到 ACK 才删除；
//!   重连后按 seq 顺序重放
//! - **心跳**：5s 一次，抖动不会导致上层状态翻转（服务端有防抖窗口）

use crate::creds::Credentials;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// 退避参数（与服务端 `session::Backoff` 同一语义）。
#[derive(Debug, Clone)]
pub struct BackoffPolicy {
    pub initial: Duration,
    pub max: Duration,
    /// jitter 比例（0.0–1.0），实际延迟 = base * (1 + rand * jitter)
    pub jitter: f64,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(100),
            max: Duration::from_secs(30),
            jitter: 0.2,
        }
    }
}

impl BackoffPolicy {
    /// 第 `attempt` 次（0 起）失败后的等待时长。
    pub fn delay(&self, attempt: u32) -> Duration {
        let base_ms = self.initial.as_millis() as u64;
        let exp = base_ms.saturating_mul(1u64 << attempt.min(20));
        let capped = exp.min(self.max.as_millis() as u64);
        let jitter_ms = (capped as f64 * self.jitter * rand::random::<f64>()) as u64;
        Duration::from_millis(capped + jitter_ms)
    }
}

/// 心跳间隔。
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// 未 ACK 的待发消息。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingMessage {
    pub seq: u64,
    pub kind: String,
    pub payload: String,
}

/// 出站消息队列（sled 持久化）。
pub struct OutboxQueue {
    db: sled::Db,
}

/// seq 计数器占用的键。它和消息键（`seq.to_be_bytes()`，8 字节）混在同一棵
/// sled 里，`pending()` 若把它当消息 JSON 解码必然失败，整批重放会被静默吞掉
/// （调用处写的是 `if let Ok(pending)`）——所以迭代类接口必须跳过它。
const SEQ_COUNTER_KEY: &[u8] = b"seq";

impl OutboxQueue {
    pub fn open(path: &Path) -> Result<Self> {
        let db = sled::open(path).map_err(|e| Error::Storage(format!("sled open: {e}")))?;
        Ok(Self { db })
    }

    /// 持久化一条待发消息（必须在发送前调用）。
    pub fn enqueue(&self, msg: &PendingMessage) -> Result<()> {
        let bytes = serde_json::to_vec(msg)?;
        self.db
            .insert(msg.seq.to_be_bytes(), bytes)
            .map_err(|e| Error::Storage(format!("sled insert: {e}")))?;
        self.db
            .flush()
            .map_err(|e| Error::Storage(format!("sled flush: {e}")))?;
        Ok(())
    }

    /// 收到 ACK 后删除。
    pub fn ack(&self, seq: u64) -> Result<()> {
        self.db
            .remove(seq.to_be_bytes())
            .map_err(|e| Error::Storage(format!("sled remove: {e}")))?;
        Ok(())
    }

    /// 按 seq 升序取出所有未 ACK 消息（重连后重放用）。
    pub fn pending(&self) -> Result<Vec<PendingMessage>> {
        let mut out = Vec::new();
        for item in self.db.iter() {
            let (k, v) = item.map_err(|e| Error::Storage(format!("sled iter: {e}")))?;
            if k.as_ref() == SEQ_COUNTER_KEY {
                continue;
            }
            let msg: PendingMessage = serde_json::from_slice(&v)
                .map_err(|e| Error::Storage(format!("sled decode: {e}")))?;
            out.push(msg);
        }
        out.sort_by_key(|m| m.seq);
        Ok(out)
    }

    pub fn len(&self) -> usize {
        self.db
            .iter()
            .flatten()
            .filter(|(k, _)| k.as_ref() != SEQ_COUNTER_KEY)
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 下一个 seq（单调递增，持久化）。
    pub fn next_seq(&self) -> Result<u64> {
        let key = SEQ_COUNTER_KEY;
        let cur = self
            .db
            .get(key)
            .map_err(|e| Error::Storage(format!("sled get: {e}")))?
            .map(|v| {
                let mut buf = [0u8; 8];
                let n = v.len().min(8);
                buf[8 - n..].copy_from_slice(&v[..n]);
                u64::from_be_bytes(buf)
            })
            .unwrap_or(0);
        let next = cur + 1;
        self.db
            .insert(key, next.to_be_bytes().to_vec())
            .map_err(|e| Error::Storage(format!("sled insert: {e}")))?;
        Ok(next)
    }
}

/// mTLS 传输层配置来源。
pub struct TransportConfig {
    pub mtls_endpoint: String,
    pub creds: Credentials,
    pub backoff: BackoffPolicy,
    pub heartbeat: Duration,
    pub state_dir: PathBuf,
}

impl TransportConfig {
    pub fn new(creds: Credentials, state_dir: PathBuf) -> Self {
        Self {
            mtls_endpoint: creds.mtls_endpoint.clone(),
            creds,
            backoff: BackoffPolicy::default(),
            heartbeat: HEARTBEAT_INTERVAL,
            state_dir,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ConfirmReply {
    pub status: String,
    pub serial: String,
    pub mtls: String,
}

/// 构建 mTLS 的 reqwest 客户端（客户端证书 + CA 链）。
///
/// **必须真正验证服务端证书**：这里把设备 CA（`ca_chain_pem`）加进信任根，
/// 握手会做链验证 + 主机名校验。之前是 `danger_accept_invalid_certs(true)`，
/// 而凭证里的 `ca_roots()` 全项目无人调用 —— 契约 §1 要求的"阶段 2 双向验证"
/// 里，服务端身份其实是零校验，任何 MITM 都能完成握手。
pub fn build_mtls_client(cfg: &TransportConfig) -> Result<reqwest::Client> {
    let mut identity_pem = cfg.creds.cert_pem.clone();
    identity_pem.push_str(&cfg.creds.key_pem);

    let ident = reqwest::Identity::from_pem(identity_pem.as_bytes())
        .map_err(|e| Error::Crypto(format!("build identity: {e}")))?;

    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        .identity(ident)
        .timeout(Duration::from_secs(15));
    for der in cfg.creds.ca_roots()? {
        let cert = reqwest::Certificate::from_der(der.as_ref())
            .map_err(|e| Error::Crypto(format!("build ca root: {e}")))?;
        builder = builder.add_root_certificate(cert);
    }
    builder.build().map_err(|e| Error::Transport(e.to_string()))
}

/// `POST /enroll/confirm` —— 用正式证书完成一次 mTLS 往返。
///
/// 这一步成功，才意味着通讯真正打通（服务端把 issued 推进到 active）。
pub async fn confirm(cfg: &TransportConfig) -> Result<ConfirmReply> {
    let client = build_mtls_client(cfg)?;
    let url = format!("{}/enroll/confirm", cfg.mtls_endpoint);
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "slot": cfg.creds.device_slot }))
        .send()
        .await
        .map_err(|e| Error::Transport(format!("confirm 请求失败（mTLS 未打通）: {e}")))?;

    if !resp.status().is_success() {
        return Err(Error::Unexpected(format!("confirm 返回 {}", resp.status())));
    }
    resp.json()
        .await
        .map_err(|e| Error::Transport(e.to_string()))
}

/// 上行消息 ACK（服务端 `POST /messages` 的响应）。
#[derive(Debug, Deserialize)]
pub struct MessageAck {
    pub status: String,
    pub seq: u64,
    pub duplicate: bool,
}

/// `POST /messages` —— 用正式证书发一条上行消息（mTLS）。
///
/// 服务端按 `(serial, seq)` 幂等去重，所以发送失败时可以放心重试，
/// 重放同一条不会重复触发副作用。
pub async fn send_message(cfg: &TransportConfig, msg: &PendingMessage) -> Result<MessageAck> {
    let client = build_mtls_client(cfg)?;
    let url = format!("{}/messages", cfg.mtls_endpoint);
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "seq": msg.seq,
            "kind": msg.kind,
            "payload": msg.payload,
        }))
        .send()
        .await
        .map_err(|e| Error::Transport(format!("消息发送失败: {e}")))?;

    if !resp.status().is_success() {
        return Err(Error::Unexpected(format!(
            "POST /messages 返回 {}",
            resp.status()
        )));
    }
    resp.json()
        .await
        .map_err(|e| Error::Transport(e.to_string()))
}

/// 一条下行消息（服务端 → 设备）。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DownlinkMessage {
    /// 全局游标：处理成功后用它 ack
    pub id: i64,
    pub kind: String,
    pub payload: String,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DownlinkReply {
    messages: Vec<DownlinkMessage>,
    next_after: i64,
}

#[derive(Debug, Deserialize)]
struct DownlinkAckReply {
    acked: u64,
}

/// `GET /messages/downlink` —— 拉取下行消息。
///
/// **拉取不等于已投递**：处理成功后必须再调用 [`ack_downlink`]，
/// 否则服务端下次还会给同一批（至少一次，与上行对称）。
pub async fn fetch_downlink(
    cfg: &TransportConfig,
    after: i64,
    limit: i64,
) -> Result<Vec<DownlinkMessage>> {
    let client = build_mtls_client(cfg)?;
    let url = format!("{}/messages/downlink", cfg.mtls_endpoint);
    let resp = client
        .get(&url)
        .query(&[("after", after.to_string()), ("limit", limit.to_string())])
        .send()
        .await
        .map_err(|e| Error::Transport(format!("拉取下行消息失败: {e}")))?;

    if !resp.status().is_success() {
        return Err(Error::Unexpected(format!(
            "GET /messages/downlink 返回 {}",
            resp.status()
        )));
    }
    let reply: DownlinkReply = resp
        .json()
        .await
        .map_err(|e| Error::Transport(e.to_string()))?;
    let _ = reply.next_after;
    Ok(reply.messages)
}

/// `POST /messages/downlink/ack` —— 确认已处理到 `up_to`（含）。
pub async fn ack_downlink(cfg: &TransportConfig, up_to: i64) -> Result<u64> {
    let client = build_mtls_client(cfg)?;
    let url = format!("{}/messages/downlink/ack", cfg.mtls_endpoint);
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "up_to": up_to }))
        .send()
        .await
        .map_err(|e| Error::Transport(format!("ack 下行消息失败: {e}")))?;

    if !resp.status().is_success() {
        return Err(Error::Unexpected(format!(
            "POST /messages/downlink/ack 返回 {}",
            resp.status()
        )));
    }
    let reply: DownlinkAckReply = resp
        .json()
        .await
        .map_err(|e| Error::Transport(e.to_string()))?;
    Ok(reply.acked)
}

/// 重连状态机事件（供上层观察/日志）。
#[derive(Debug, Clone)]
pub enum ConnectEvent {
    Connected,
    Disconnected { reason: String, attempt: u32 },
    Replaying { count: usize },
    Replayed { count: usize },
}

/// 带防抖的重连循环。
///
/// - `probe` 返回 Err 视为断线，按指数退避重试
/// - `probe` 成功后：**真正按 seq 逐条重发**未 ACK 消息，服务端确认后才从
///   outbox 删除（此前只是打日志，消息留在队列里永不发出）
/// - 重放失败即停止本轮，保证顺序；下一轮连接会从同一条继续
/// - 成功一轮后按心跳间隔再探，而不是退避下限
pub async fn run_with_reconnect<F, Fut, S, SFut>(
    policy: BackoffPolicy,
    outbox: Arc<OutboxQueue>,
    mut probe: F,
    mut send: S,
    events: Option<mpsc::Sender<ConnectEvent>>,
) -> !
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
    S: FnMut(PendingMessage) -> SFut,
    SFut: std::future::Future<Output = Result<()>>,
{
    let mut attempt = 0u32;
    loop {
        match probe().await {
            Ok(()) => {
                if attempt > 0 {
                    attempt = 0;
                }
                if let Some(tx) = &events {
                    let _ = tx.send(ConnectEvent::Connected).await;
                }
                // 重放未 ACK 消息（读失败必须喊出来：静默吞掉等于重放永远不发生）
                match outbox.pending() {
                    Ok(pending) if !pending.is_empty() => {
                        if let Some(tx) = &events {
                            let _ = tx
                                .send(ConnectEvent::Replaying {
                                    count: pending.len(),
                                })
                                .await;
                        }
                        let mut sent = 0usize;
                        for msg in pending {
                            match send(msg.clone()).await {
                                Ok(()) => {
                                    sent += 1;
                                    match outbox.ack(msg.seq) {
                                        Ok(()) => tracing::info!(
                                            seq = msg.seq,
                                            kind = %msg.kind,
                                            "重放成功并已 ACK"
                                        ),
                                        Err(e) => tracing::error!(
                                            seq = msg.seq,
                                            error = %e,
                                            "ACK 落盘失败，服务端已收到但本地可能重发"
                                        ),
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        seq = msg.seq,
                                        error = %e,
                                        "重放失败，停止本轮（保序），下轮从这条继续"
                                    );
                                    break;
                                }
                            }
                        }
                        if let Some(tx) = &events {
                            let _ = tx.send(ConnectEvent::Replayed { count: sent }).await;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => tracing::error!(error = %e, "读取未 ACK 消息失败，本轮跳过重放"),
                }
                // 连接保持阶段：probe 返回即视为断线。成功一轮后按**心跳间隔**再探，
                // 而不是退避下限 —— 否则 confirm 会被打成 ~10 次/秒，
                // 违反契约 §6 的 5s 心跳并冲击服务端防抖。
                if let Some(tx) = &events {
                    let _ = tx
                        .send(ConnectEvent::Disconnected {
                            reason: "connection closed".into(),
                            attempt,
                        })
                        .await;
                }
                tokio::time::sleep(HEARTBEAT_INTERVAL).await;
                attempt = 0;
            }
            Err(e) => {
                if let Some(tx) = &events {
                    let _ = tx
                        .send(ConnectEvent::Disconnected {
                            reason: e.to_string(),
                            attempt,
                        })
                        .await;
                }
                tokio::time::sleep(policy.delay(attempt)).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_monotonic_and_capped() {
        let p = BackoffPolicy {
            initial: Duration::from_millis(100),
            max: Duration::from_secs(1),
            jitter: 0.0,
        };
        let d0 = p.delay(0);
        let d1 = p.delay(1);
        let d2 = p.delay(2);
        assert_eq!(d0, Duration::from_millis(100));
        assert_eq!(d1, Duration::from_millis(200));
        assert_eq!(d2, Duration::from_millis(400));
        assert_eq!(p.delay(20), Duration::from_millis(1000));
    }

    #[test]
    fn backoff_jitter_stays_within_bounds() {
        let p = BackoffPolicy {
            initial: Duration::from_millis(100),
            max: Duration::from_millis(100),
            jitter: 0.2,
        };
        for _ in 0..100 {
            let d = p.delay(5);
            assert!(d >= Duration::from_millis(100));
            assert!(d <= Duration::from_millis(120));
        }
    }

    #[test]
    fn outbox_persists_and_replays_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let q = OutboxQueue::open(dir.path()).unwrap();
        // 故意乱序入队
        for seq in [3u64, 1, 2] {
            q.enqueue(&PendingMessage {
                seq,
                kind: "hello".into(),
                payload: format!("p{seq}"),
            })
            .unwrap();
        }
        let pending = q.pending().unwrap();
        assert_eq!(
            pending.iter().map(|m| m.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(q.len(), 3);
    }

    #[test]
    fn outbox_ack_removes_message() {
        let dir = tempfile::tempdir().unwrap();
        let q = OutboxQueue::open(dir.path()).unwrap();
        q.enqueue(&PendingMessage {
            seq: 1,
            kind: "k".into(),
            payload: "v".into(),
        })
        .unwrap();
        q.ack(1).unwrap();
        assert!(q.pending().unwrap().is_empty());
        assert!(q.is_empty());
    }

    #[test]
    fn outbox_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let q = OutboxQueue::open(dir.path()).unwrap();
            q.enqueue(&PendingMessage {
                seq: 7,
                kind: "k".into(),
                payload: "v".into(),
            })
            .unwrap();
        }
        let q2 = OutboxQueue::open(dir.path()).unwrap();
        let pending = q2.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].seq, 7);
    }

    /// 之前 `next_seq` 的计数器键和消息键混在一起，`pending()` 把它的 u64 原始
    /// 字节当 JSON 解码必然失败，整批重放被调用处的 `if let Ok` 静默吞掉。
    #[test]
    fn seq_counter_does_not_pollute_pending_or_len() {
        let dir = tempfile::tempdir().unwrap();
        let q = OutboxQueue::open(dir.path()).unwrap();
        let seq = q.next_seq().unwrap();
        q.enqueue(&PendingMessage {
            seq,
            kind: "k".into(),
            payload: "v".into(),
        })
        .unwrap();

        let pending = q.pending().expect("计数器不应让 pending() 整体失败");
        assert_eq!(pending.len(), 1, "计数器不应被当成一条消息");
        assert_eq!(q.len(), 1, "len() 不应把计数器算进去");
        assert_eq!(q.next_seq().unwrap(), seq + 1);
    }

    #[test]
    fn seq_is_monotonic_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let first = {
            let q = OutboxQueue::open(dir.path()).unwrap();
            (q.next_seq().unwrap(), q.next_seq().unwrap())
        };
        assert_eq!(first, (1, 2));
        let q2 = OutboxQueue::open(dir.path()).unwrap();
        assert_eq!(q2.next_seq().unwrap(), 3);
    }

    #[tokio::test]
    async fn reconnect_replays_pending_after_failure() {
        let dir = tempfile::tempdir().unwrap();
        let q = Arc::new(OutboxQueue::open(dir.path()).unwrap());
        q.enqueue(&PendingMessage {
            seq: 1,
            kind: "k".into(),
            payload: "v".into(),
        })
        .unwrap();

        let (tx, mut rx) = mpsc::channel(64);
        let q2 = Arc::clone(&q);
        let q_check = Arc::clone(&q);
        let policy = BackoffPolicy {
            initial: Duration::from_millis(1),
            max: Duration::from_millis(5),
            jitter: 0.0,
        };
        let sent = Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
        let sent_in_task = Arc::clone(&sent);
        let mut calls = 0;
        let handle = tokio::spawn(async move {
            run_with_reconnect(
                policy,
                q2,
                move || {
                    calls += 1;
                    let n = calls;
                    async move {
                        if n == 1 {
                            Err(Error::Transport("boom".into()))
                        } else {
                            Ok(())
                        }
                    }
                },
                move |msg: PendingMessage| {
                    sent_in_task.lock().unwrap().push(msg.seq);
                    async move { Ok::<(), Error>(()) }
                },
                Some(tx),
            )
            .await
        });

        // 期待: Disconnected -> Connected -> Replaying(1) -> Replayed(1)
        let mut seen = Vec::new();
        for _ in 0..4 {
            if let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
                seen.push(format!("{ev:?}"));
            }
        }
        handle.abort();
        let joined = seen.join(" ");
        assert!(joined.contains("Disconnected"), "{joined}");
        assert!(joined.contains("Connected"), "{joined}");
        assert!(joined.contains("Replaying"), "{joined}");
        assert!(joined.contains("count: 1"), "{joined}");
        assert_eq!(
            *sent.lock().unwrap(),
            vec![1],
            "重放必须真正调用发送，而不是只打日志"
        );
        assert!(
            q_check.pending().unwrap().is_empty(),
            "发送成功后消息必须 ACK 并移出 outbox"
        );
    }
}
