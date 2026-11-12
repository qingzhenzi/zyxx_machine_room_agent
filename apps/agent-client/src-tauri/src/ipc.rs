//! Local IPC client for the `qe-agent` daemon.
//!
//! The daemon exposes a tiny HTTP/1.1 API over a Unix domain socket (see the
//! backend `docs/agent-ipc.md`). The webview must never connect to the socket
//! itself, so this module performs the request on the Rust side and exposes the
//! result through `#[tauri::command]` handlers.
//!
//! Socket discovery: `QE_AGENT_IPC_SOCKET` (development override) or the
//! production default `/run/qe-agent/agent.sock`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

/// Production socket path used by the systemd unit.
pub const DEFAULT_SOCKET_PATH: &str = "/run/qe-agent/agent.sock";

/// Status/logs/policy requests are cheap; the daemon never blocks for long.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

const DEFAULT_LOG_LINES: u32 = 200;
const MAX_LOG_LINES: u32 = 2000;

/// Registration blocks until the server issues a certificate or the request
/// expires; the GUI passes the same default as the daemon (300s).
const DEFAULT_ENROLL_WAIT_SECS: u64 = 300;
const MAX_ENROLL_WAIT_SECS: u64 = 900;

/// One readable IPC failure. Every message is meant to be shown in the UI.
#[derive(Debug)]
pub struct IpcError {
    message: String,
}

impl IpcError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for IpcError {}

/// Socket path the GUI should talk to.
pub fn socket_path() -> PathBuf {
    std::env::var_os("QE_AGENT_IPC_SOCKET")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET_PATH))
}

/// Displayable socket path (used by the status/settings pages).
pub fn endpoint() -> String {
    socket_path().to_string_lossy().into_owned()
}

/// Perform one IPC request and return the parsed JSON body.
///
/// `timeout` bounds both write and read; the response is read until the daemon
/// closes the connection (`Connection: close`).
#[cfg(unix)]
pub fn request(
    method: &str,
    path: &str,
    body: Option<&str>,
    timeout: Duration,
) -> Result<Value, IpcError> {
    request_at(&socket_path(), method, path, body, timeout)
}

#[cfg(not(unix))]
pub fn request(
    _method: &str,
    _path: &str,
    _body: Option<&str>,
    _timeout: Duration,
) -> Result<Value, IpcError> {
    Err(IpcError::new(
        "本机 IPC 仅支持 Linux / macOS（Unix domain socket）；当前平台暂不支持。",
    ))
}

#[cfg(unix)]
fn request_at(
    socket: &Path,
    method: &str,
    path: &str,
    body: Option<&str>,
    timeout: Duration,
) -> Result<Value, IpcError> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let display = socket.display();
    let mut stream = UnixStream::connect(socket).map_err(|error| connect_error(&error, socket))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| IpcError::new(format!("设置 IPC 读取超时失败：{error}")))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| IpcError::new(format!("设置 IPC 写入超时失败：{error}")))?;

    let request = build_request(method, path, body);
    stream.write_all(request.as_bytes()).map_err(|error| {
        IpcError::new(format!(
            "向 qe-agent 写入 IPC 请求失败（{display}）：{error}"
        ))
    })?;
    stream
        .flush()
        .map_err(|error| IpcError::new(format!("刷新 IPC 请求失败（{display}）：{error}")))?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response).map_err(|error| {
        use std::io::ErrorKind;

        if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) {
            IpcError::new(format!(
                "qe-agent 响应超时（超过 {}s，socket {display}）。daemon 可能正忙或未正常启动。",
                timeout.as_secs()
            ))
        } else {
            IpcError::new(format!("读取 qe-agent IPC 响应失败（{display}）：{error}"))
        }
    })?;

    parse_response(&response)
}

#[cfg(unix)]
fn connect_error(error: &std::io::Error, socket: &Path) -> IpcError {
    use std::io::ErrorKind;

    let display = socket.display();
    match error.kind() {
        ErrorKind::NotFound => IpcError::new(format!(
            "未连接 qe-agent daemon：IPC socket 不存在（{display}）。启动 daemon：\
             qe-agent serve --role both --ipc-socket {display}"
        )),
        ErrorKind::PermissionDenied => IpcError::new(format!(
            "未连接 qe-agent daemon：没有权限访问 IPC socket（{display}）。请把当前用户加入 \
             qe-agent 组，或检查 socket 权限是否为 0660：{error}"
        )),
        ErrorKind::ConnectionRefused => IpcError::new(format!(
            "未连接 qe-agent daemon：IPC socket 拒绝连接（{display}，daemon 可能已退出）：{error}"
        )),
        _ => IpcError::new(format!("连接 qe-agent IPC 失败（{display}）：{error}")),
    }
}

#[cfg(unix)]
fn build_request(method: &str, path: &str, body: Option<&str>) -> String {
    use std::fmt::Write as _;

    let mut request = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: localhost\r\n\
         Accept: application/json\r\n\
         Connection: close\r\n"
    );

    if let Some(body) = body {
        request.push_str("Content-Type: application/json\r\n");
        let _ = write!(request, "Content-Length: {}\r\n", body.len());
    }

    request.push_str("\r\n");

    if let Some(body) = body {
        request.push_str(body);
    }

    request
}

#[cfg(unix)]
fn parse_response(bytes: &[u8]) -> Result<Value, IpcError> {
    let text = String::from_utf8_lossy(bytes);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return Err(IpcError::new(
            "qe-agent 返回了无法解析的 IPC 响应（缺少 HTTP 头）。",
        ));
    };

    let status_line = head
        .lines()
        .next()
        .ok_or_else(|| IpcError::new("qe-agent 返回了空的 IPC 响应。"))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| IpcError::new(format!("无法解析 IPC 状态码：{status_line}")))?;

    let body = body.trim();
    let value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(body).map_err(|error| {
            IpcError::new(format!(
                "无法解析 qe-agent 响应 JSON（HTTP {status}）：{error}"
            ))
        })?
    };

    if (400..600).contains(&status) {
        let message = value
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("未知错误");
        return Err(IpcError::new(format!(
            "qe-agent 返回 HTTP {status}：{message}"
        )));
    }

    Ok(value)
}

/// Run a blocking socket request off the async/main thread so a slow or
/// unreachable daemon can never freeze the webview (enroll may wait minutes).
async fn run_blocking(
    method: &'static str,
    path: String,
    body: Option<String>,
    timeout: Duration,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        request(method, &path, body.as_deref(), timeout).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("qe-agent IPC 任务执行失败：{error}"))?
}

#[tauri::command]
pub async fn agent_ipc_status() -> Result<Value, String> {
    run_blocking("GET", "/v1/status".to_string(), None, READ_TIMEOUT).await
}

#[tauri::command]
pub async fn agent_ipc_logs(limit: Option<u32>) -> Result<Value, String> {
    let limit = limit.unwrap_or(DEFAULT_LOG_LINES).clamp(1, MAX_LOG_LINES);
    run_blocking("GET", format!("/v1/logs?limit={limit}"), None, READ_TIMEOUT).await
}

#[tauri::command]
pub async fn agent_ipc_policy() -> Result<Value, String> {
    run_blocking("GET", "/v1/policy".to_string(), None, READ_TIMEOUT).await
}

#[tauri::command]
pub async fn agent_ipc_enroll(
    link: String,
    trust: bool,
    wait_secs: Option<u64>,
) -> Result<Value, String> {
    let wait_secs = wait_secs
        .unwrap_or(DEFAULT_ENROLL_WAIT_SECS)
        .clamp(1, MAX_ENROLL_WAIT_SECS);
    let body = serde_json::json!({
        "link": link,
        "trust": trust,
        "wait_secs": wait_secs,
    })
    .to_string();

    let timeout = Duration::from_secs(wait_secs.saturating_add(5));
    run_blocking("POST", "/v1/enroll".to_string(), Some(body), timeout).await
}

#[tauri::command]
pub fn agent_ipc_endpoint() -> String {
    endpoint()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
    use std::thread;
    use std::time::Instant;

    static SOCKET_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_socket_path() -> PathBuf {
        let id = SOCKET_COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "qe-agent-ipc-test-{}-{id}.sock",
            std::process::id()
        ))
    }

    /// Removes the socket file when the test finishes.
    struct SocketGuard(PathBuf);

    impl Drop for SocketGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn read_http_request(stream: &mut UnixStream) -> String {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];

        loop {
            let read = match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            buffer.extend_from_slice(&chunk[..read]);

            if let Some(head_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buffer[..head_end]);
                let content_length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);

                if buffer.len() >= head_end + 4 + content_length {
                    break;
                }
            }
        }

        String::from_utf8_lossy(&buffer).into_owned()
    }

    fn spawn_mock_server(status: u16, body: &'static str) -> (PathBuf, Receiver<String>) {
        let socket = temp_socket_path();
        let listener = UnixListener::bind(&socket).expect("bind mock IPC socket");
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let request = read_http_request(&mut stream);
                let _ = sender.send(request);

                let response = format!(
                    "HTTP/1.1 {status} Mock\r\n\
                     content-type: application/json\r\n\
                     content-length: {}\r\n\
                     connection: close\r\n\
                     \r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        (socket, receiver)
    }

    /// Accepts one connection but never answers, to exercise read timeouts.
    fn spawn_hanging_server() -> PathBuf {
        let socket = temp_socket_path();
        let listener = UnixListener::bind(&socket).expect("bind hanging IPC socket");

        thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                thread::sleep(Duration::from_secs(2));
                drop(stream);
            }
        });

        socket
    }

    fn captured_request(receiver: &Receiver<String>) -> String {
        match receiver.recv_timeout(Duration::from_secs(3)) {
            Ok(request) => request,
            Err(RecvTimeoutError::Timeout) => panic!("mock IPC server did not receive a request"),
            Err(RecvTimeoutError::Disconnected) => {
                panic!("mock IPC server channel disconnected before the request")
            }
        }
    }

    #[test]
    fn status_round_trip_returns_parsed_json() {
        let (socket, receiver) = spawn_mock_server(
            200,
            r#"{"version":"9.9.9","roles":["node","machine"],"enrolled":true,"device_slot":"room-7"}"#,
        );
        let _guard = SocketGuard(socket.clone());

        let value = request_at(&socket, "GET", "/v1/status", None, Duration::from_secs(2))
            .expect("status IPC call should succeed");

        let request = captured_request(&receiver);
        assert!(request.starts_with("GET /v1/status HTTP/1.1"));
        assert!(request.contains("Connection: close"));
        assert_eq!(value["version"], "9.9.9");
        assert_eq!(value["roles"][1], "machine");
        assert_eq!(value["enrolled"], true);
        assert_eq!(value["device_slot"], "room-7");
    }

    #[test]
    fn logs_request_carries_limit_and_parses_items() {
        let (socket, receiver) =
            spawn_mock_server(200, r#"{"items":["first line","second line"]}"#);
        let _guard = SocketGuard(socket.clone());

        let value = request_at(
            &socket,
            "GET",
            "/v1/logs?limit=7",
            None,
            Duration::from_secs(2),
        )
        .expect("logs IPC call should succeed");

        let request = captured_request(&receiver);
        assert!(request.starts_with("GET /v1/logs?limit=7 HTTP/1.1"));
        assert_eq!(value["items"][0], "first line");
        assert_eq!(value["items"][1], "second line");
    }

    #[test]
    fn enroll_posts_json_and_returns_pending_reply() {
        let (socket, receiver) =
            spawn_mock_server(202, r#"{"status":"pending","enroll_id":"e-1"}"#);
        let _guard = SocketGuard(socket.clone());

        let body = r#"{"link":"https://qe.example/enroll?token=t","trust":true,"wait_secs":30}"#;
        let value = request_at(
            &socket,
            "POST",
            "/v1/enroll",
            Some(body),
            Duration::from_secs(2),
        )
        .expect("enroll IPC call should succeed");

        let request = captured_request(&receiver);
        assert!(request.starts_with("POST /v1/enroll HTTP/1.1"));
        assert!(request.contains("Content-Type: application/json"));
        assert!(request.contains("Content-Length:"));
        assert!(request.ends_with(body));
        assert_eq!(value["status"], "pending");
        assert_eq!(value["enroll_id"], "e-1");
    }

    #[test]
    fn http_error_body_is_surfaced_in_the_message() {
        let (socket, _receiver) = spawn_mock_server(
            400,
            r#"{"error":{"code":"enroll_failed","message":"注册链接无效: 缺少 scheme"}}"#,
        );
        let _guard = SocketGuard(socket.clone());

        let error = request_at(
            &socket,
            "POST",
            "/v1/enroll",
            Some(r#"{"link":"bad"}"#),
            Duration::from_secs(2),
        )
        .expect_err("HTTP 400 must be an error");

        let message = error.to_string();
        assert!(message.contains("HTTP 400"), "message: {message}");
        assert!(message.contains("注册链接无效"), "message: {message}");
    }

    #[test]
    fn missing_socket_reports_daemon_guidance() {
        let socket = temp_socket_path();
        let error = request_at(&socket, "GET", "/v1/status", None, Duration::from_secs(1))
            .expect_err("missing socket must fail");

        let message = error.to_string();
        assert!(
            message.contains("未连接 qe-agent daemon"),
            "message: {message}"
        );
        assert!(message.contains("socket"), "message: {message}");
        assert!(
            message.contains("qe-agent serve --role both"),
            "message: {message}"
        );
    }

    #[test]
    fn hanging_server_times_out_with_a_readable_error() {
        let socket = spawn_hanging_server();
        let _guard = SocketGuard(socket.clone());

        let started = Instant::now();
        let error = request_at(
            &socket,
            "GET",
            "/v1/status",
            None,
            Duration::from_millis(200),
        )
        .expect_err("hanging server must hit the read timeout");

        assert!(error.to_string().contains("超时"), "message: {error}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout should be respected"
        );
    }

    /// Manual end-to-end check against a real daemon:
    /// `QE_AGENT_IPC_SOCKET=/tmp/qe-agent-dev.sock \
    ///  cargo test --manifest-path src-tauri/Cargo.toml --lib ipc::tests::real_daemon -- --ignored`
    #[test]
    #[ignore = "requires a running qe-agent daemon (QE_AGENT_IPC_SOCKET)"]
    fn real_daemon_endpoints_are_reachable() {
        let status = request("GET", "/v1/status", None, Duration::from_secs(2))
            .expect("real daemon status IPC call should succeed");
        assert!(status["version"].as_str().is_some());
        assert!(status["socket"].as_str().is_some());
        assert!(status["roles"].is_array());

        let logs = request("GET", "/v1/logs?limit=5", None, Duration::from_secs(2))
            .expect("real daemon logs IPC call should succeed");
        assert!(logs["items"].is_array());

        let policy = request("GET", "/v1/policy", None, Duration::from_secs(2))
            .expect("real daemon policy IPC call should succeed");
        assert!(policy["source"].is_string());

        let error = request(
            "POST",
            "/v1/enroll",
            Some(r#"{"link":"not-a-link","trust":true,"wait_secs":5}"#),
            Duration::from_secs(10),
        )
        .expect_err("an invalid enrollment link must fail");
        assert!(error.to_string().contains("HTTP 400"), "message: {error}");
    }

    #[test]
    fn malformed_response_is_rejected() {
        let (socket, _receiver) = spawn_mock_server(200, "not-json");
        let _guard = SocketGuard(socket.clone());

        let error = request_at(&socket, "GET", "/v1/status", None, Duration::from_secs(2))
            .expect_err("non-JSON body must fail");

        assert!(
            error.to_string().contains("无法解析 qe-agent 响应 JSON"),
            "message: {error}"
        );
    }
}
