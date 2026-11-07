//! noVNC 阶段 2（agent 侧）：`vnc.info` 信息查询 + `vnc.open` mTLS WS ↔ 本地 VNC TCP 桥。
//!
//! 协议见 `room_node/docs/novnc-design.md` §5 与 `docs/novnc-agent.md`：
//!
//! ```text
//! qe-api / device-pki  ── wss://<DEVICE_PKI_MTLS>/vnc/<serial>?session=<uuid> ──► agent
//! agent ── 127.0.0.1:<libvirt VNC port> (TCP) ──► qemu VNC server
//! ```
//!
//! agent 只做**字节管道**：VNC 协议/RFB 握手与密码认证由浏览器 noVNC 完成；
//! 本模块负责建立 mTLS WebSocket、连接本机回环 VNC 端口、双向转发二进制帧，
//! 并对单帧大小、空闲时间与任一端关闭做兜底。

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use uuid::Uuid;

use qe_device_pki_client::channel;
use qe_device_pki_client::transport::TransportConfig;
use virt_crate::{GraphicsInfo, GraphicsKind};

/// 单个 WS 二进制帧上限（契约 2 MiB）。
pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
/// 空闲上限：30 分钟无双向数据即关闭（契约）。
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// TCP → WS 的读取块大小（远小于 2 MiB 上限）。
const TCP_READ_BUF: usize = 64 * 1024;
/// 本地 VNC TCP 连接超时。
const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// `vnc.open` 下行 payload。
#[derive(Debug, Clone, Deserialize)]
pub struct VncOpen {
    pub session_id: Uuid,
    pub vm: String,
    /// 设备 serial（可选；WS 路径始终使用本机证书的 serial，见 [`effective_serial`]）。
    #[serde(default)]
    pub serial: Option<String>,
}

impl VncOpen {
    /// 解析并校验 payload。
    pub fn parse(payload: &str) -> Result<Self> {
        let open: Self =
            serde_json::from_str(payload).context("vnc.open payload 不是合法 JSON / 缺字段")?;
        open.validate()?;
        Ok(open)
    }

    /// 入参校验。
    pub fn validate(&self) -> Result<()> {
        if !crate::vm_control::is_valid_vm_name(self.vm.as_str()) {
            bail!("vnc.open 的 vm 名非法: {:?}", self.vm);
        }
        if let Some(serial) = self.serial.as_deref() {
            let serial = serial.trim();
            if serial.is_empty() || serial.len() > 128 {
                bail!("vnc.open 的 serial 非法: {:?}", self.serial);
            }
        }
        Ok(())
    }
}

/// 隧道 WS 路径使用哪个 serial：**以设备证书 serial 为准**（device-pki 会校验
/// 证书 serial 与路径一致）；payload 里的 serial 仅用于交叉校验/日志。
pub fn effective_serial(payload_serial: Option<&str>, certificate_serial: &str) -> String {
    let serial = certificate_serial.trim();
    let payload = payload_serial
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(payload) = payload
        && payload != serial
    {
        tracing::warn!(
            payload_serial = %payload,
            certificate_serial = %serial,
            "vnc.open payload serial 与设备证书不一致，使用证书 serial"
        );
    }
    serial.to_owned()
}

/// `vnc.open` 的 WS 路径：`/vnc/<percent-encoded serial>?session=<uuid>`。
pub fn vnc_ws_path(serial: &str, session_id: &Uuid) -> Result<String> {
    if serial.trim().is_empty() {
        bail!("vnc 隧道 serial 不能为空");
    }
    // device-pki 内部端与 qe-api ticket 使用 32 位紧凑 hex（Uuid::simple），
    // 两端必须用同一形式，否则配对表匹配不到（UUID 默认带连字符）。
    Ok(format!(
        "/vnc/{}?session={}",
        encode_path_segment(serial.trim()),
        session_id.simple()
    ))
}

/// 把 `DEVICE_PKI_MTLS_URL`（`https://host:port`）转成完整 wss URL（日志/单测用；
/// 实际握手由 [`channel::connect_path`] 完成，避免 URL 解析差异）。
pub fn vnc_ws_url(endpoint: &str, serial: &str, session_id: &Uuid) -> Result<String> {
    let (scheme, rest) = split_scheme(endpoint)?;
    let host_port = rest.trim_end_matches('/');
    if host_port.is_empty() {
        bail!("DEVICE_PKI_MTLS_URL 缺主机名/端口: {endpoint:?}");
    }
    Ok(format!(
        "{scheme}://{host_port}{}",
        vnc_ws_path(serial, session_id)?
    ))
}

fn split_scheme(endpoint: &str) -> Result<(&'static str, &str)> {
    let endpoint = endpoint.trim();
    if let Some(rest) = endpoint.strip_prefix("https://") {
        Ok(("wss", rest))
    } else if let Some(rest) = endpoint.strip_prefix("wss://") {
        Ok(("wss", rest))
    } else if let Some(rest) = endpoint.strip_prefix("http://") {
        Ok(("ws", rest))
    } else if let Some(rest) = endpoint.strip_prefix("ws://") {
        Ok(("ws", rest))
    } else {
        bail!("DEVICE_PKI_MTLS_URL 必须是 http(s):// 或 ws(s)://，收到 {endpoint:?}")
    }
}

/// percent-encode 一个 URL 路径段（RFC 3986 unreserved 之外全部转义，大写 hex）。
pub fn encode_path_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// 从 libvirt graphics 中解析本地 VNC TCP 目标 `(host, port)`。
///
/// - 只接受 VNC 且 `port > 0`，否则报"VM 无 VNC 图形控制台"；
/// - `0.0.0.0` / `::` / `localhost` 统一映射到 `127.0.0.1`；其它非回环 host 拒绝
///   （VNC 不应该暴露到局域网）。
pub fn vnc_target_from_graphics(vm: &str, graphics: &[GraphicsInfo]) -> Result<(String, u16)> {
    let vnc = graphics
        .iter()
        .find(|info| info.kind == GraphicsKind::Vnc)
        .ok_or_else(|| anyhow::anyhow!("VM {vm:?} 无 VNC 图形控制台"))?;
    if vnc.port == 0 {
        bail!("VM {vm:?} 的 VNC 端口未分配（port=0），确认 VM 已启动且有图形控制台");
    }
    let host = normalize_loopback(&vnc.host)
        .with_context(|| format!("VM {vm:?} 的 VNC host {:?} 不是回环地址", vnc.host))?;
    Ok((host, vnc.port))
}

/// `vnc.info` 的 data：`{name, host, port, password|null}`。
pub fn vnc_info_from_graphics(name: &str, graphics: &[GraphicsInfo]) -> Result<Value> {
    let vnc = graphics
        .iter()
        .find(|info| info.kind == GraphicsKind::Vnc)
        .ok_or_else(|| anyhow::anyhow!("VM {name:?} 无 VNC 图形控制台"))?;
    if vnc.port == 0 {
        bail!("VM {name:?} 的 VNC 端口未分配（port=0），确认 VM 已启动且有图形控制台");
    }
    let host = normalize_loopback(&vnc.host)
        .with_context(|| format!("VM {name:?} 的 VNC host {:?} 不是回环地址", vnc.host))?;
    Ok(json!({
        "name": name,
        "host": host,
        "port": vnc.port,
        // 密码来自 libvirt <graphics passwd=...>；没有密码时为 null。
        "password": vnc.password_value,
    }))
}

fn normalize_loopback(host: &str) -> Result<String> {
    let host = host.trim();
    let loopback = matches!(
        host,
        "" | "127.0.0.1"
            | "localhost"
            | "0.0.0.0"
            | "::"
            | "::1"
            | "0:0:0:0:0:0:0:0"
            | "[::]"
            | "[::1]"
    );
    if loopback {
        Ok("127.0.0.1".to_owned())
    } else {
        bail!("VNC host {host:?} 不是回环地址（只允许 127.0.0.1/0.0.0.0/::/localhost）")
    }
}

/// 空闲判断（纯函数，单测用）。
pub fn is_idle(last_activity: Instant, now: Instant, idle: Duration) -> bool {
    now.saturating_duration_since(last_activity) >= idle
}

/// 一次隧道的统计。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayStats {
    /// WS → 本地 VNC 的字节数（键鼠/客户端事件）。
    pub bytes_to_vnc: u64,
    /// 本地 VNC → WS 的字节数（画面帧）。
    pub bytes_to_ws: u64,
    /// 结束原因：`idle` / `ws_closed` / `tcp_closed`。
    pub reason: &'static str,
}

/// 建立隧道并双向泵数据（带 session id，用于 WS 路径）。
pub async fn relay_session(
    transport: &TransportConfig,
    session_id: Uuid,
    serial: &str,
    vm: &str,
    target: (&str, u16),
    idle: Duration,
) -> Result<RelayStats> {
    let path = vnc_ws_path(serial, &session_id)?;
    tracing::info!(
        session_id = %session_id,
        vm = %vm,
        serial = %serial,
        target = %format!("{}:{}", target.0, target.1),
        "vnc 隧道建立中（mTLS WS）"
    );
    let ws = channel::connect_path(transport, &path)
        .await
        .with_context(|| format!("连接 device-pki VNC 隧道失败（path={path}）"))?;

    let tcp = tokio::time::timeout(
        TCP_CONNECT_TIMEOUT,
        TcpStream::connect((target.0, target.1)),
    )
    .await
    .map_err(|_| anyhow::anyhow!("连接本地 VNC {}:{} 超时", target.0, target.1))?
    .with_context(|| format!("连接本地 VNC {}:{} 失败", target.0, target.1))?;
    let _ = tcp.set_nodelay(true);

    let (mut ws_sink, mut ws_stream) = ws.split();
    let (mut tcp_read, mut tcp_write) = tcp.into_split();
    let mut buffer = vec![0u8; TCP_READ_BUF];
    let mut bytes_to_vnc: u64 = 0;
    let mut bytes_to_ws: u64 = 0;
    // 用 tokio 的 Instant，便于 reset sleep。
    let mut last_activity = tokio::time::Instant::now();

    // 以最后活动时间 + idle 作为空闲截止时间；数据到达时 reset。
    let idle_sleep = tokio::time::sleep_until(last_activity + idle);
    tokio::pin!(idle_sleep);

    let reason: &'static str = loop {
        tokio::select! {
            _ = &mut idle_sleep => break "idle",
            message = ws_stream.next() => {
                match message {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(data))) => {
                        if data.len() > MAX_FRAME_BYTES {
                            bail!("WS 二进制帧过大：{} 字节 > {MAX_FRAME_BYTES}", data.len());
                        }
                        if !data.is_empty() {
                            tcp_write.write_all(&data).await.context("写入本地 VNC 失败")?;
                            bytes_to_vnc += data.len() as u64;
                            last_activity = tokio::time::Instant::now();
                            idle_sleep.as_mut().reset(last_activity + idle);
                        }
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                        // VNC 数据必须走二进制帧；文本帧忽略，避免协议污染。
                        tracing::debug!(len = text.len(), "忽略 vnc 隧道文本帧");
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(_)))
                    | Some(Ok(tokio_tungstenite::tungstenite::Message::Pong(_))) => {}
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(frame))) => {
                        tracing::debug!(?frame, "vnc 隧道 WS 关闭");
                        break "ws_closed";
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Frame(_))) => {
                        // 底层原始帧（不会由公共 API 产出），忽略。
                    }
                    Some(Err(error)) => {
                        return Err(anyhow::anyhow!("WS 读取失败: {error}"));
                    }
                    None => break "ws_closed",
                }
            }
            read = tcp_read.read(&mut buffer) => {
                match read.context("读取本地 VNC 失败")? {
                    0 => break "tcp_closed",
                    n => {
                        ws_sink
                            .send(tokio_tungstenite::tungstenite::Message::Binary(
                                buffer[..n].to_vec().into(),
                            ))
                            .await
                            .context("WS 发送失败")?;
                        bytes_to_ws += n as u64;
                        last_activity = tokio::time::Instant::now();
                        idle_sleep.as_mut().reset(last_activity + idle);
                    }
                }
            }
        }
    };

    // 任一端结束：尽力关闭另一端，释放资源（不把关闭错误当失败）。
    let _ = tcp_write.shutdown().await;
    let _ = ws_sink.close().await;

    tracing::info!(
        session_id = %session_id,
        vm = %vm,
        serial = %serial,
        bytes_to_vnc,
        bytes_to_ws,
        reason,
        "vnc 隧道结束"
    );
    Ok(RelayStats {
        bytes_to_vnc,
        bytes_to_ws,
        reason,
    })
}

/// 解析 graphics 并跑一次完整隧道（`vnc.open` 的入口）。
pub async fn run_session(
    executor: std::sync::Arc<dyn crate::vm_control::VmExecutor>,
    transport: &TransportConfig,
    open: &VncOpen,
    serial: &str,
) -> Result<RelayStats> {
    let vm = open.vm.clone();
    let vm_for_task = vm.clone();
    let graphics = tokio::task::spawn_blocking(move || executor.graphics(&vm_for_task))
        .await
        .context("读取 libvirt graphics 任务失败")??;
    let (host, port) = vnc_target_from_graphics(&vm, &graphics)?;
    relay_session(
        transport,
        open.session_id,
        serial,
        &vm,
        (&host, port),
        IDLE_TIMEOUT,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graphics(kind: GraphicsKind, host: &str, port: u16, password: Option<&str>) -> GraphicsInfo {
        GraphicsInfo {
            kind,
            host: host.to_owned(),
            port,
            password: password.is_some(),
            password_value: password.map(str::to_owned),
        }
    }

    fn session() -> Uuid {
        Uuid::parse_str("0b6f9d4e4a414b6e9d0b4f8b0f3c2a11").unwrap()
    }

    #[test]
    fn parses_vnc_open_payload() {
        let payload = r#"{"session_id":"0b6f9d4e4a414b6e9d0b4f8b0f3c2a11",
            "vm":"vm-1","serial":"abc123"}"#;
        let open = VncOpen::parse(payload).unwrap();
        assert_eq!(open.vm, "vm-1");
        assert_eq!(open.serial.as_deref(), Some("abc123"));
        assert_eq!(open.session_id, session());

        // serial 可选（设计稿 payload 只有 session_id/vm）
        let open =
            VncOpen::parse(r#"{"session_id":"0b6f9d4e4a414b6e9d0b4f8b0f3c2a11","vm":"vm-1"}"#)
                .unwrap();
        assert!(open.serial.is_none());
    }

    #[test]
    fn rejects_bad_vnc_open_payloads() {
        for payload in [
            "{}",
            r#"{"vm":"vm-1"}"#,
            r#"{"session_id":"nope","vm":"vm-1"}"#,
            r#"{"session_id":"0b6f9d4e4a414b6e9d0b4f8b0f3c2a11","vm":"../x"}"#,
            r#"{"session_id":"0b6f9d4e4a414b6e9d0b4f8b0f3c2a11","vm":"vm-1","serial":"  "}"#,
        ] {
            assert!(VncOpen::parse(payload).is_err(), "{payload}");
        }
    }

    #[test]
    fn effective_serial_prefers_certificate() {
        assert_eq!(effective_serial(Some("abc"), "abc"), "abc");
        // payload 与证书不一致 / 缺失 → 用证书 serial
        assert_eq!(
            effective_serial(Some("other"), "cert-serial"),
            "cert-serial"
        );
        assert_eq!(effective_serial(None, "cert-serial"), "cert-serial");
    }

    #[test]
    fn builds_ws_path_with_escaping() {
        assert_eq!(
            vnc_ws_path("0102030405", &session()).unwrap(),
            "/vnc/0102030405?session=0b6f9d4e4a414b6e9d0b4f8b0f3c2a11"
        );
        // 特殊字符必须 percent-encode，避免路径/查询注入
        assert_eq!(
            vnc_ws_path("a/b c", &session()).unwrap(),
            "/vnc/a%2Fb%20c?session=0b6f9d4e4a414b6e9d0b4f8b0f3c2a11"
        );
        assert!(vnc_ws_path("   ", &session()).is_err());
    }

    #[test]
    fn builds_wss_url_from_https_endpoint() {
        assert_eq!(
            vnc_ws_url("https://node.example:4441", "abc", &session()).unwrap(),
            "wss://node.example:4441/vnc/abc?session=0b6f9d4e4a414b6e9d0b4f8b0f3c2a11"
        );
        // 尾部斜杠
        assert_eq!(
            vnc_ws_url("https://node.example:4441/", "abc", &session()).unwrap(),
            "wss://node.example:4441/vnc/abc?session=0b6f9d4e4a414b6e9d0b4f8b0f3c2a11"
        );
        // 本地明文调试：http → ws
        assert_eq!(
            vnc_ws_url("http://127.0.0.1:4441", "abc", &session()).unwrap(),
            "ws://127.0.0.1:4441/vnc/abc?session=0b6f9d4e4a414b6e9d0b4f8b0f3c2a11"
        );
        assert!(vnc_ws_url("ftp://x:1", "abc", &session()).is_err());
        assert!(vnc_ws_url("https://", "abc", &session()).is_err());
    }

    #[test]
    fn resolves_local_vnc_target() {
        let vnc = graphics(GraphicsKind::Vnc, "127.0.0.1", 5901, None);
        assert_eq!(
            vnc_target_from_graphics("vm-1", &[vnc]).unwrap(),
            ("127.0.0.1".to_owned(), 5901)
        );
        for host in ["0.0.0.0", "::", "localhost", "::1"] {
            let vnc = graphics(GraphicsKind::Vnc, host, 5900, None);
            assert_eq!(
                vnc_target_from_graphics("vm-1", &[vnc]).unwrap(),
                ("127.0.0.1".to_owned(), 5900),
                "{host}"
            );
        }
    }

    #[test]
    fn rejects_non_loopback_or_missing_vnc() {
        let public = graphics(GraphicsKind::Vnc, "192.168.1.10", 5901, None);
        let error = vnc_target_from_graphics("vm-1", &[public]).unwrap_err();
        assert!(error.to_string().contains("回环"), "{error}");

        let unallocated = graphics(GraphicsKind::Vnc, "127.0.0.1", 0, None);
        assert!(vnc_target_from_graphics("vm-1", &[unallocated]).is_err());

        let spice = graphics(GraphicsKind::Spice, "127.0.0.1", 5900, None);
        assert!(vnc_target_from_graphics("vm-1", &[spice]).is_err());

        assert!(vnc_target_from_graphics("vm-1", &[]).is_err());
    }

    #[test]
    fn vnc_info_shape() {
        let vnc = graphics(GraphicsKind::Vnc, "127.0.0.1", 5902, Some("s3cret"));
        let value = vnc_info_from_graphics("vm-1", &[vnc]).unwrap();
        assert_eq!(value["name"], "vm-1");
        assert_eq!(value["host"], "127.0.0.1");
        assert_eq!(value["port"], 5902);
        assert_eq!(value["password"], "s3cret");

        let no_password = graphics(GraphicsKind::Vnc, "0.0.0.0", 5903, None);
        let value = vnc_info_from_graphics("vm-2", &[no_password]).unwrap();
        assert_eq!(value["host"], "127.0.0.1");
        assert!(value["password"].is_null());
    }

    #[test]
    fn idle_detection() {
        let start = Instant::now();
        assert!(!is_idle(start, start, IDLE_TIMEOUT));
        assert!(is_idle(start, start + IDLE_TIMEOUT, IDLE_TIMEOUT));
        assert!(!is_idle(
            start,
            start + IDLE_TIMEOUT - Duration::from_secs(1),
            IDLE_TIMEOUT
        ));
        assert_eq!(IDLE_TIMEOUT, Duration::from_secs(1800));
        assert!(MAX_FRAME_BYTES == 2 * 1024 * 1024);
    }
}
