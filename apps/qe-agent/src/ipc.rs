//! 本机 IPC：GUI（agent-client）通过 Unix domain socket 访问 daemon。
//!
//! 协议见 `room_agent/docs/agent-ipc.md`。只监听本机 UDS（不监听 TCP），socket 权限 `0660`；
//! 私钥/证书内容永远不经过 IPC。
//!
//! 当前实现（P3 MVP）：`/v1/healthz`、`/v1/status`、`/v1/policy`、`/v1/logs`、`/v1/enroll`。

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use qe_device_pki_client::creds::{self, Credentials};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::logging::LogBuffer;

/// `POST /v1/display` 请求体：agent 内部模块推送展示层模板。
#[derive(Debug, Deserialize)]
struct DisplayPush {
    template: String,
    #[serde(default)]
    params: Value,
}

/// `POST /v1/power` 请求体：本机 GUI / CLI 触发的电源动作。
#[derive(Debug, Deserialize)]
struct PowerRequest {
    action: String,
    #[serde(default = "default_true")]
    graceful: bool,
}

fn default_true() -> bool {
    true
}

/// 请求头上限。
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// 请求体上限（注册链接很小，256KiB 足够）。
const MAX_BODY_BYTES: usize = 256 * 1024;
/// 日志接口默认/最大返回行数。
const DEFAULT_LOG_LINES: usize = 200;

/// IPC 服务端状态（跨线程共享）。
#[derive(Clone)]
pub struct IpcState {
    pub socket: PathBuf,
    pub data_dir: PathBuf,
    pub slot: Option<String>,
    pub roles: Vec<String>,
    pub report_mode: Option<String>,
    /// machine 角色的两档间隔（node-only 时为 None）。
    pub intervals: Option<(u64, u64)>,
    pub started_at: Instant,
    pub logs: LogBuffer,
    pub enroll_lock: Arc<tokio::sync::Mutex<()>>,
    /// boot 流水线状态（None = 未启用 boot 流水线，如 cli 模式）。
    pub boot: Option<crate::boot::PipelineHandle>,
}

impl IpcState {
    pub fn new(
        socket: PathBuf,
        data_dir: PathBuf,
        slot: Option<String>,
        roles: Vec<String>,
        report_mode: Option<String>,
        intervals: Option<(u64, u64)>,
        logs: LogBuffer,
    ) -> Self {
        Self {
            socket,
            data_dir,
            slot,
            roles,
            report_mode,
            intervals,
            started_at: Instant::now(),
            logs,
            enroll_lock: Arc::new(tokio::sync::Mutex::new(())),
            boot: None,
        }
    }
}

/// 在后台线程里启动 UDS HTTP 服务；返回时 socket 已绑定可用。
pub fn spawn(state: IpcState) -> Result<()> {
    #[cfg(unix)]
    {
        if let Some(parent) = state.socket.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建 IPC 目录失败: {}", parent.display()))?;
        }
        if state.socket.exists() {
            std::fs::remove_file(&state.socket)
                .with_context(|| format!("清理旧 IPC socket 失败: {}", state.socket.display()))?;
        }

        let listener = std::os::unix::net::UnixListener::bind(&state.socket)
            .with_context(|| format!("绑定 IPC socket 失败: {}", state.socket.display()))?;
        listener
            .set_nonblocking(true)
            .context("设置 IPC socket 非阻塞失败")?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&state.socket, std::fs::Permissions::from_mode(0o660))
                .context("设置 IPC socket 权限失败")?;
        }

        let socket_path = state.socket.display().to_string();
        std::thread::Builder::new()
            .name("qe-agent-ipc".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        eprintln!("创建 IPC runtime 失败: {error}");
                        return;
                    }
                };
                runtime.block_on(serve(listener, state));
            })
            .context("启动 IPC 线程失败")?;

        eprintln!("qe-agent IPC 已监听 {socket_path}");
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = state;
        bail!("本机 IPC 目前只支持 Linux/macOS");
    }
}

#[cfg(unix)]
async fn serve(std_listener: std::os::unix::net::UnixListener, state: IpcState) {
    let listener = match UnixListener::from_std(std_listener) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("初始化 IPC listener 失败: {error}");
            return;
        }
    };

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    handle(stream, state).await;
                });
            }
            Err(error) => {
                eprintln!("IPC accept 失败: {error}");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

struct Request {
    method: String,
    target: String,
    body: Vec<u8>,
}

async fn handle(mut stream: UnixStream, state: IpcState) {
    let request = match read_request(&mut stream).await {
        Ok(Some(request)) => request,
        Ok(None) => return,
        Err(error) => {
            let _ = write_response(
                &mut stream,
                400,
                error_json("bad_request", format!("{error:#}")),
            )
            .await;
            return;
        }
    };

    let (status, body) = route(&state, &request.method, &request.target, &request.body).await;
    let _ = write_response(&mut stream, status, body).await;
}

async fn read_request(stream: &mut UnixStream) -> Result<Option<Request>> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];

    let header_end = loop {
        if let Some(index) = find_bytes(&buffer, b"\r\n\r\n") {
            break index + 4;
        }
        if buffer.len() > MAX_HEADER_BYTES {
            bail!("请求头过大");
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..read]);
    };

    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_ascii_uppercase();
    let target = parts.next().unwrap_or_default().to_owned();
    if method.is_empty() || target.is_empty() {
        bail!("非法请求行");
    }

    let mut content_length = 0usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse::<usize>().unwrap_or(0);
        }
    }
    if content_length > MAX_BODY_BYTES {
        bail!("请求体过大");
    }

    let mut body = buffer[header_end..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);

    Ok(Some(Request {
        method,
        target,
        body,
    }))
}

async fn write_response(stream: &mut UnixStream, status: u16, body: Value) -> io::Result<()> {
    let payload = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&payload).await?;
    stream.flush().await
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 路由（与 socket 解耦，便于单测）。
pub(crate) async fn route(
    state: &IpcState,
    method: &str,
    target: &str,
    body: &[u8],
) -> (u16, Value) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    match (method, path) {
        ("GET", "/v1/healthz") => (200, json!({ "ok": true })),
        ("GET", "/v1/status") => (200, status_json(state)),
        ("GET", "/v1/policy") => (200, policy_json(state)),
        ("GET", "/v1/logs") => {
            let limit = query_param(query, "limit")
                .and_then(|raw| raw.parse::<usize>().ok())
                .unwrap_or(DEFAULT_LOG_LINES)
                .clamp(1, 2000);
            (200, json!({ "items": state.logs.snapshot(limit) }))
        }
        ("POST", "/v1/enroll") => enroll(state, body).await,
        ("POST", "/v1/checks-interval") => {
            let Some(boot) = &state.boot else {
                return (503, error_json("unavailable", "boot 流水线未启用"));
            };
            match serde_json::from_slice::<serde_json::Value>(body) {
                Ok(value) => match value.get("interval_secs").and_then(|v| v.as_u64()) {
                    Some(secs) => {
                        boot.set_checks_interval(secs);
                        (
                            200,
                            json!({ "ok": true, "interval_secs": boot.monitor_interval_secs() }),
                        )
                    }
                    None => (400, error_json("bad_request", "缺少 interval_secs")),
                },
                Err(error) => (400, error_json("bad_request", format!("解析失败: {error}"))),
            }
        }
        ("POST", "/v1/display/unlock") => {
            // 调试解锁：boot-display 已校验密码，这里停止 supervise 并结束展示层。
            crate::boot::display::shutdown();
            (200, json!({ "ok": true }))
        }
        ("POST", "/v1/power") => {
            match serde_json::from_slice::<PowerRequest>(body) {
                Ok(request) => match qe_agent_node::agent_control::execute_local(&request.action, request.graceful).await {
                    Ok(data) => (200, json!({ "ok": true, "data": data })),
                    Err(error) => (400, error_json("power_failed", error.to_string())),
                },
                Err(error) => (400, error_json("bad_request", format!("解析失败: {error}"))),
            }
        }
        ("POST", "/v1/display") => {
            let Some(boot) = &state.boot else {
                return (503, error_json("unavailable", "boot 流水线未启用"));
            };
            match serde_json::from_slice::<DisplayPush>(body) {
                Ok(push) => {
                    boot.push_display(&push.template, push.params);
                    (200, json!({ "ok": true }))
                }
                Err(error) => (400, error_json("bad_request", format!("解析失败: {error}"))),
            }
        }
        ("GET", "/v1/state") => (
            200,
            state
                .boot
                .as_ref()
                .map(|boot| boot.state_json())
                .unwrap_or_else(|| {
                    json!({ "overall": "unavailable", "config_sufficient": false, "phases": [] })
                }),
        ),
        _ => (
            404,
            error_json("not_found", format!("未知 IPC 端点: {method} {path}")),
        ),
    }
}

fn status_json(state: &IpcState) -> Value {
    let credentials = load_credentials(state);
    let mut device_id: Option<String> = None;
    let (device_slot, serial, mtls_endpoint) = match &credentials {
        Some(credentials) => {
            // 新证书 CN=device_id（UUID）；旧证书 CN=slot。
            let cn = Credentials::extract_cn_from_cert_pem(&credentials.cert_pem).ok();
            let slot = match cn.as_deref() {
                Some(cn) if uuid::Uuid::parse_str(cn).is_ok() => {
                    device_id = Some(cn.to_owned());
                    Some(credentials.device_slot.clone())
                }
                Some(cn) => Some(cn.to_owned()),
                None => Some(credentials.device_slot.clone()),
            };
            (
                slot,
                Some(credentials.serial.clone()),
                Some(credentials.mtls_endpoint.clone()),
            )
        }
        None => (
            state.slot.clone(),
            None,
            std::env::var("DEVICE_PKI_MTLS_URL")
                .ok()
                .map(|raw| raw.trim_end_matches('/').to_owned()),
        ),
    };
    let uptime_secs = state.started_at.elapsed().as_secs();
    let started_at = unix_now().saturating_sub(uptime_secs);

    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "roles": state.roles,
        "report_mode": state.report_mode,
        "enrolled": credentials.is_some(),
        "device_id": device_id,
        "device_slot": device_slot,
        "serial": serial,
        "mtls_endpoint": mtls_endpoint,
        "data_dir": state.data_dir.display().to_string(),
        "socket": state.socket.display().to_string(),
        "started_at": started_at,
        "uptime_secs": uptime_secs,
        "intervals": state.intervals.map(|(fast, full)| json!({
            "fast_interval_secs": fast,
            "full_interval_secs": full,
        })),
    })
}

fn policy_json(state: &IpcState) -> Value {
    match state.intervals {
        Some((fast, full)) => json!({
            "source": "local",
            "revision": Value::Null,
            "policy": {
                "telemetry_fast_interval_secs": fast,
                "telemetry_full_interval_secs": full,
            },
        }),
        None => json!({
            "source": "local",
            "revision": Value::Null,
            "policy": {},
        }),
    }
}

#[derive(Debug, Deserialize)]
struct EnrollRequest {
    link: String,
    #[serde(default)]
    trust: bool,
    #[serde(default = "default_wait_secs")]
    wait_secs: u64,
}

fn default_wait_secs() -> u64 {
    300
}

async fn enroll(state: &IpcState, body: &[u8]) -> (u16, Value) {
    let request: EnrollRequest = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(error) => {
            return (
                400,
                error_json("bad_request", format!("注册请求不是合法 JSON: {error}")),
            );
        }
    };

    if request.link.trim().is_empty() {
        return (400, error_json("bad_request", "注册链接不能为空"));
    }

    let Ok(_guard) = state.enroll_lock.try_lock() else {
        return (409, error_json("busy", "已有注册请求正在进行"));
    };

    let options = qe_device_pki_client::enroll::EnrollOptions {
        approval_wait: Some(Duration::from_secs(request.wait_secs.clamp(1, 3600))),
        trust_confirmed: request.trust,
        data_dir: Some(state.data_dir.clone()),
    };

    match qe_device_pki_client::enroll::enroll_link(&request.link, &options).await {
        Ok(qe_device_pki_client::enroll::EnrollOutcome::Issued(credentials)) => (
            200,
            json!({
                "status": "issued",
                "device_slot": credentials.device_slot,
                "serial": credentials.serial,
                "mtls_endpoint": credentials.mtls_endpoint,
            }),
        ),
        Ok(qe_device_pki_client::enroll::EnrollOutcome::PendingApproval { enroll_id }) => {
            (202, json!({ "status": "pending", "enroll_id": enroll_id }))
        }
        Err(error) => (
            400,
            error_json("enroll_failed", format!("注册失败: {error:#}")),
        ),
    }
}

/// 读取本地设备凭证；槽位未知时只在 `creds/` 下恰好一个目录时采用。
fn load_credentials(state: &IpcState) -> Option<Credentials> {
    let dir = match state.slot.as_deref() {
        Some(slot) => creds::locate(slot, Some(&state.data_dir)).ok()?,
        None => {
            let root = state.data_dir.join("creds");
            let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
                .ok()?
                .flatten()
                .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
                .map(|entry| entry.path())
                .collect();
            dirs.sort();
            if dirs.len() != 1 {
                return None;
            }
            dirs.remove(0)
        }
    };
    Credentials::load(&dir).ok()
}

fn error_json(code: &str, message: impl Into<String>) -> Value {
    json!({ "error": { "code": code, "message": message.into() } })
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then_some(value)
    })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn test_state(dir: &Path) -> IpcState {
        IpcState::new(
            dir.join("agent.sock"),
            dir.to_path_buf(),
            Some("room-101".to_owned()),
            vec!["machine".to_owned()],
            Some("mtls".to_owned()),
            Some((10, 300)),
            LogBuffer::default(),
        )
    }

    #[tokio::test]
    async fn healthz_and_status_are_served() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_state(dir.path());

        let (status, body) = route(&state, "GET", "/v1/healthz", b"").await;
        assert_eq!(status, 200);
        assert_eq!(body["ok"], true);

        let (status, body) = route(&state, "GET", "/v1/status", b"").await;
        assert_eq!(status, 200);
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(body["enrolled"], false, "临时目录里没有凭证");
        assert_eq!(body["device_slot"], "room-101");
        assert_eq!(body["intervals"]["fast_interval_secs"], 10);
    }

    #[tokio::test]
    async fn logs_and_policy_endpoints_work() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_state(dir.path());
        state.logs.push("line-1".to_owned());
        state.logs.push("line-2".to_owned());

        let (status, body) = route(&state, "GET", "/v1/logs?limit=1", b"").await;
        assert_eq!(status, 200);
        assert_eq!(body["items"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["items"][0], "line-2");

        let (status, body) = route(&state, "GET", "/v1/policy", b"").await;
        assert_eq!(status, 200);
        assert_eq!(body["policy"]["telemetry_full_interval_secs"], 300);
    }

    #[tokio::test]
    async fn enroll_rejects_bad_json_and_empty_link() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_state(dir.path());

        let (status, body) = route(&state, "POST", "/v1/enroll", b"not-json").await;
        assert_eq!(status, 400);
        assert_eq!(body["error"]["code"], "bad_request");

        let (status, body) = route(&state, "POST", "/v1/enroll", br#"{"link":"  "}"#).await;
        assert_eq!(status, 400);
        assert_eq!(body["error"]["code"], "bad_request");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ipc_serves_status_over_unix_socket() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_state(dir.path());
        spawn(state).expect("spawn ipc");

        let socket = dir.path().join("agent.sock");
        let mut stream = loop {
            match UnixStream::connect(&socket).await {
                Ok(stream) => break stream,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        };
        stream
            .write_all(b"GET /v1/status HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("write request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read response");
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.contains("\"version\""), "{text}");
    }
}
