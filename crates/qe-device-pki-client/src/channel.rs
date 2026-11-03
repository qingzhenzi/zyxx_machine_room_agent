//! 设备侧 WebSocket 隧道客户端：一条连接完成「注册激活 + 双向消息」。
//!
//! 服务端契约见 `qe_device_pki::channel`。要点：
//! - **建连即激活**：WS 升级成功就意味着服务端把 enrollment 推到了 `active`，
//!   不再需要单独跑一次 `confirm`
//! - 上行：从 outbox 取未 ACK 消息逐条发 `{"type":"msg",...}`，收到 `ack_msg`
//!   才删除（语义与 HTTP `POST /messages` 完全一致）
//! - 下行：收到 `{"type":"down",...}` 就交给本地 Dispatcher，处理成功回
//!   `{"type":"ack","up_to":id}`；游标落盘，重启后继续
//! - 断线：指数退避重连，未 ACK 的消息与未确认的下行都会在新连接上重投
//!
//! TLS 与注册通道同一套约束：**用 `ca_chain_pem` 做链验证 + 主机名校验**，
//! 不允许 `danger_accept_invalid_certs`。

use crate::downlink::{self, Dispatcher};
use crate::error::{Error, Result};
use crate::transport::{OutboxQueue, TransportConfig};
use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;

/// 隧道所用的 WebSocket 连接类型（mTLS 之上的 HTTP/1.1 升级）。
pub type WsStream = WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>;

/// 心跳间隔（与服务端 `CHANNEL_HEARTBEAT` 对齐，略短以便主动探活）。
pub const HEARTBEAT: Duration = Duration::from_secs(15);
/// 重连退避。
pub const RETRY_INITIAL: Duration = Duration::from_millis(200);
pub const RETRY_MAX: Duration = Duration::from_secs(30);

// ---------------- 帧 ----------------

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    Msg {
        seq: u64,
        kind: String,
        payload: String,
    },
    Ack {
        up_to: i64,
    },
    Ping,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    Ready {
        status: String,
        serial: String,
    },
    AckMsg {
        seq: u64,
        status: String,
        duplicate: bool,
    },
    Down {
        id: i64,
        kind: String,
        payload: String,
    },
    Pong,
    Err {
        code: String,
        message: String,
    },
}

// ---------------- 建连 ----------------

/// 用设备证书与 CA 链建一条到 `/channel` 的 mTLS WebSocket。
pub async fn connect(cfg: &TransportConfig) -> Result<WsStream> {
    let tls = build_tls_config(cfg)?;
    let connector = TlsConnector::from(Arc::new(tls));

    let (host_port, server_name) = endpoint_parts(&cfg.mtls_endpoint)?;
    let tcp = TcpStream::connect(&host_port)
        .await
        .map_err(|e| Error::Transport(format!("连接 {host_port} 失败: {e}")))?;
    let tls_stream = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| Error::Transport(format!("TLS 握手失败: {e}")))?;

    let request = format!("wss://{host_port}/channel")
        .into_client_request()
        .map_err(|e| Error::Transport(format!("构造握手请求失败: {e}")))?;
    let (ws, _) = tokio_tungstenite::client_async(request, tls_stream)
        .await
        .map_err(|e| Error::Transport(format!("WebSocket 握手失败: {e}")))?;
    Ok(ws)
}

/// rustls 配置：设备证书 + CA 链信任根（不做任何"接受无效证书"的妥协）。
fn build_tls_config(cfg: &TransportConfig) -> Result<rustls::ClientConfig> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let chain: Vec<CertificateDer<'static>> = cfg.creds.cert_chain_der()?;
    let key: PrivateKeyDer<'static> = cfg.creds.private_key_der()?;
    let mut roots = rustls::RootCertStore::empty();
    for der in cfg.creds.ca_roots()? {
        roots
            .add(der)
            .map_err(|e| Error::Crypto(format!("加载 CA 根证书失败: {e}")))?;
    }

    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(chain, key)
        .map_err(|e| Error::Crypto(format!("装配客户端证书失败: {e}")))?;
    // WebSocket 走 HTTP/1.1 升级，明确只提供这个 ALPN
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

/// 把 `https://host:port/...` 拆成连接地址与 rustls ServerName。
fn endpoint_parts(endpoint: &str) -> Result<(String, ServerName<'static>)> {
    let host_port = endpoint
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("wss://")
        .trim_end_matches('/');
    let host = host_port
        .split(':')
        .next()
        .ok_or_else(|| Error::Transport(format!("无法解析端点 {endpoint}")))?;
    let server_name = ServerName::try_from(host.to_owned())
        .map_err(|e| Error::Transport(format!("非法主机名 {host}: {e}")))?;
    Ok((host_port.to_owned(), server_name))
}

// ---------------- 会话循环 ----------------

/// 一条隧道连接上的完整会话：上行重放 + 下行接收 + 心跳。
///
/// 返回 `Ok(())` 表示服务端主动关闭（干净退出），`Err` 表示连接断了（调用方重连）。
/// 服务端 ready 帧的内容：注册已激活的凭据。
#[derive(Debug, Clone)]
pub struct Ready {
    pub serial: String,
    pub status: String,
}

/// 建连 + 读取 ready 帧。返回连接与激活信息。
///
/// ready 帧是"建连即注册完成"的证据：拿到它说明服务端已把 enrollment
/// 推到 `active`，客户端不再需要单独跑 `confirm`。
pub async fn open(cfg: &TransportConfig) -> Result<(WsStream, Ready)> {
    let mut ws = connect(cfg).await?;
    let ready = match ws.next().await {
        Some(Ok(Message::Text(text))) => {
            let frame: ServerFrame = serde_json::from_str(&text)
                .map_err(|e| Error::Unexpected(format!("首帧不是 ready: {e}")))?;
            match frame {
                ServerFrame::Ready { status, serial } => Ready { serial, status },
                ServerFrame::Err { code, message } => {
                    return Err(Error::Unexpected(format!("{code}: {message}")));
                }
                other => {
                    return Err(Error::Unexpected(format!("首帧应为 ready，实际 {other:?}")));
                }
            }
        }
        Some(Ok(Message::Close(_))) | None => {
            return Err(Error::Transport("服务端在 ready 前关闭了连接".into()));
        }
        Some(Ok(_)) => {
            return Err(Error::Unexpected("服务端在 ready 前发了非文本帧".into()));
        }
        Some(Err(e)) => return Err(Error::Transport(format!("读取首帧失败: {e}"))),
    };
    tracing::info!(serial = %ready.serial, status = %ready.status, "隧道已建立，注册已激活");
    Ok((ws, ready))
}

/// 在已建好的连接上跑会话循环（上行重放 + 下行接收 + 心跳）。
///
/// 返回 `Ok(())` 表示服务端主动关闭（干净退出），`Err` 表示连接断了（调用方重连）。
pub async fn run_on(
    mut ws: WsStream,
    outbox: &OutboxQueue,
    dispatcher: &Dispatcher,
    state_file: &Path,
) -> Result<()> {
    let mut heartbeat =
        tokio::time::interval_at(tokio::time::Instant::now() + HEARTBEAT, HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut cursor = downlink::load_state(state_file)?.last_id;

    // 建连后立刻把队列里积压的上行消息推出去（不用等第一次心跳）
    push_pending(outbox, &mut ws).await?;

    loop {
        let tick = tokio::select! {
            frame = ws.next() => Tick::Frame(frame),
            _ = heartbeat.tick() => Tick::Heartbeat,
        };

        match tick {
            Tick::Frame(Some(Ok(Message::Text(text)))) => {
                let frame: ServerFrame = match serde_json::from_str(&text) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!(error = %e, "无法解析服务端帧");
                        continue;
                    }
                };
                match frame {
                    ServerFrame::Ready { .. } => {}
                    ServerFrame::AckMsg {
                        seq,
                        status,
                        duplicate,
                    } => {
                        // 上行被服务端确认 → 从 outbox 删除
                        outbox.ack(seq)?;
                        tracing::debug!(seq, status = %status, duplicate, "上行已确认");
                    }
                    ServerFrame::Down { id, kind, payload } => {
                        let msg = crate::transport::DownlinkMessage {
                            id,
                            kind,
                            payload,
                            created_at: None,
                        };
                        match dispatcher.dispatch(msg).await {
                            Ok(()) => {
                                // 处理成功才 ack；游标落盘（重启后不重复投递已确认的）
                                let ack = ClientFrame::Ack { up_to: id };
                                ws.send(Message::Text(serde_json::to_string(&ack)?.into()))
                                    .await
                                    .map_err(|e| Error::Transport(format!("发送 ack 失败: {e}")))?;
                                cursor = cursor.max(id);
                                downlink::save_state(
                                    &downlink::DownlinkState { last_id: cursor },
                                    state_file,
                                )?;
                            }
                            Err(e) => {
                                // 不 ack：服务端会在重连后重投（至少一次）
                                tracing::error!(id, error = %e, "下行消息处理失败，不 ack");
                                return Err(e);
                            }
                        }
                    }
                    ServerFrame::Pong => tracing::trace!("收到 pong"),
                    ServerFrame::Err { code, message } => {
                        tracing::warn!(code = %code, message = %message, "服务端返回错误");
                    }
                }
            }
            Tick::Frame(Some(Ok(Message::Ping(payload)))) => {
                ws.send(Message::Pong(payload))
                    .await
                    .map_err(|e| Error::Transport(format!("回 pong 失败: {e}")))?;
            }
            Tick::Frame(Some(Ok(Message::Pong(_)))) => {}
            Tick::Frame(Some(Ok(Message::Close(_)))) | Tick::Frame(None) => {
                return Err(Error::Transport("服务端关闭了连接".into()));
            }
            Tick::Frame(Some(Ok(_))) => {}
            Tick::Frame(Some(Err(e))) => return Err(Error::Transport(format!("读取失败: {e}"))),
            Tick::Heartbeat => {
                // 主动探活：连发两次没回应就交给上层重连
                let ping = ClientFrame::Ping;
                ws.send(Message::Text(serde_json::to_string(&ping)?.into()))
                    .await
                    .map_err(|e| Error::Transport(format!("心跳发送失败: {e}")))?;
                push_pending(outbox, &mut ws).await?;
            }
        }

        // 每轮结束顺手把 outbox 里的消息推出去（新消息可能刚入队）
        push_pending(outbox, &mut ws).await?;
    }
}

enum Tick {
    Frame(Option<std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>),
    Heartbeat,
}

/// 把 outbox 里未 ACK 的消息按 seq 逐条发出去（顺序不可乱）。
async fn push_pending(outbox: &OutboxQueue, ws: &mut WsStream) -> Result<()> {
    let pending = outbox.pending()?;
    for msg in pending {
        let frame = ClientFrame::Msg {
            seq: msg.seq,
            kind: msg.kind,
            payload: msg.payload,
        };
        ws.send(Message::Text(serde_json::to_string(&frame)?.into()))
            .await
            .map_err(|e| Error::Transport(format!("发送上行消息失败: {e}")))?;
        tracing::debug!(seq = msg.seq, "已发送上行消息，等待 ack_msg");
    }
    Ok(())
}

/// 带退避的隧道主循环：断线自动重连，直到被 abort。
pub async fn run_with_reconnect(
    cfg: TransportConfig,
    outbox: Arc<OutboxQueue>,
    dispatcher: Arc<Dispatcher>,
    state_file: PathBuf,
) {
    let mut delay = RETRY_INITIAL;
    loop {
        let result = match open(&cfg).await {
            Ok((ws, _ready)) => run_on(ws, &outbox, &dispatcher, &state_file).await,
            Err(e) => Err(e),
        };
        match result {
            Ok(()) => {
                tracing::info!("隧道会话结束");
                delay = RETRY_INITIAL;
            }
            Err(e) => {
                tracing::warn!(error = %e, "隧道断开，{delay:?} 后重连");
            }
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(RETRY_MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_parts_parses_variants() {
        let (addr, name) = endpoint_parts("https://node.example:4441").unwrap();
        assert_eq!(addr, "node.example:4441");
        assert_eq!(name.to_str(), "node.example");

        let (addr, _) = endpoint_parts("wss://127.0.0.1:4441/").unwrap();
        assert_eq!(addr, "127.0.0.1:4441");

        assert!(endpoint_parts("https://").is_err());
    }

    #[test]
    fn frame_wire_format_matches_server() {
        // 与服务端 `qe_device_pki::channel` 的 serde 约定必须逐字节一致
        let msg = ClientFrame::Msg {
            seq: 3,
            kind: "heartbeat".into(),
            payload: "{}".into(),
        };
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"type":"msg","seq":3,"kind":"heartbeat","payload":"{}"}"#
        );
        let ack = ClientFrame::Ack { up_to: 9 };
        assert_eq!(
            serde_json::to_string(&ack).unwrap(),
            r#"{"type":"ack","up_to":9}"#
        );

        let ready: ServerFrame =
            serde_json::from_str(r#"{"type":"ready","status":"active","serial":"abc"}"#).unwrap();
        assert!(matches!(ready, ServerFrame::Ready { .. }));
        let down: ServerFrame =
            serde_json::from_str(r#"{"type":"down","id":5,"kind":"k","payload":"p"}"#).unwrap();
        assert!(matches!(down, ServerFrame::Down { id: 5, .. }));
        // 未知 type 必须被拒
        assert!(serde_json::from_str::<ServerFrame>(r#"{"type":"nope"}"#).is_err());
    }
}
