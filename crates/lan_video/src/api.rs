//! 控制面 HTTP API（只监听本机，可选 Bearer token）。
//!
//! - `GET  /v1/health`            健康检查
//! - `GET  /v1/capabilities`      编码器/采集源/IP 探测
//! - `GET  /v1/sessions`          会话列表
//! - `POST /v1/sessions`          创建会话（body 为 PipelineConfig）
//! - `DELETE /v1/sessions/{id}`   强制终止

use crate::{capabilities, input::Injector, pipeline::PipelineConfig, session::SessionManager};
use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    body::Body,
    extract::{
        DefaultBodyLimit, Path, Query, Request, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get},
    serve::ListenerExt,
};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_util::io::ReaderStream;

#[derive(Clone)]
pub struct AppState {
    pub manager: Arc<SessionManager>,
    pub token: Option<String>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/capabilities", get(capabilities_handler))
        .route("/v1/audio-devices", get(audio_devices_handler))
        .route("/v1/windows", get(windows_handler))
        .route("/v1/sessions", get(list_sessions).post(create_session))
        .route("/v1/sessions/{id}", delete(kill_session))
        .route("/v1/sessions/{id}/control", get(control_ws))
        .route("/v1/sessions/{id}/files", get(list_files).post(upload_file))
        .route("/v1/sessions/{id}/files/{name}", get(download_file))
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024 * 1024))
        .layer(middleware::from_fn(cors))
        .with_state(state)
}

pub async fn serve(state: AppState, bind: &str) -> Result<()> {
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("无法绑定 {bind}"))?;
    let addr = listener.local_addr()?;
    // 输入事件是小包，必须关 Nagle，否则最多会白加 ~40ms。
    let listener = listener.tap_io(|stream| {
        let _ = stream.set_nodelay(true);
    });
    println!("lan-video API: http://{addr}");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("HTTP 服务异常退出")?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "gst": crate::gst_version(),
        "sessions": state.manager.list().len(),
    }))
}

async fn capabilities_handler() -> Json<Value> {
    Json(serde_json::to_value(capabilities()).unwrap_or(Value::Null))
}

async fn audio_devices_handler() -> Json<Value> {
    Json(json!({ "devices": crate::devices::audio_devices() }))
}

async fn windows_handler() -> Json<Value> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(windows) = crate::kwin::list_windows() {
            if !windows.is_empty() {
                return Json(json!({ "backend": "kwin", "windows": windows }));
            }
        }
        let windows = crate::x11::list_windows();
        Json(json!({ "backend": "x11", "windows": windows }))
    }
    #[cfg(not(target_os = "linux"))]
    {
        match crate::windows::list_windows() {
            Ok(windows) => Json(json!({ "backend": "win32", "windows": windows })),
            Err(err) => Json(json!({
                "windows": [],
                "error": format!("窗口枚举失败：{err:#}；可用 PowerShell 取 HWND 后传 --window-handle")
            })),
        }
    }
}

async fn list_sessions(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(err) = check_auth(&state, &headers) {
        return err;
    }
    Json(json!({ "sessions": state.manager.list() })).into_response()
}

async fn create_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(cfg): Json<PipelineConfig>,
) -> Response {
    if let Err(err) = check_auth(&state, &headers) {
        return err;
    }
    match state.manager.create(&cfg) {
        Ok(info) => (StatusCode::CREATED, Json(json!({ "session": info }))).into_response(),
        Err(err) => bad_request(err),
    }
}

async fn kill_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(err) = check_auth(&state, &headers) {
        return err;
    }
    match state.manager.kill(&id) {
        Ok(true) => Json(json!({ "killed": true, "id": id })).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "session not found", "id": id })),
        )
            .into_response(),
        Err(err) => bad_request(err),
    }
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

#[derive(Deserialize)]
struct UploadQuery {
    token: Option<String>,
    name: String,
}

fn session_auth(state: &AppState, id: &str, token: Option<&str>, headers: &HeaderMap) -> bool {
    let header_token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    state
        .manager
        .authorize(id, token.or(header_token).unwrap_or_default())
}

async fn control_ws(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !session_auth(&state, &id, query.token.as_deref(), &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid session token"})),
        )
            .into_response();
    }
    let manager = state.manager.clone();
    ws.on_upgrade(move |socket| async move {
        if let Err(err) = control_loop(socket, manager, id).await {
            eprintln!("control: {err:#}");
        }
    })
}

async fn control_loop(
    mut socket: WebSocket,
    manager: Arc<SessionManager>,
    id: String,
) -> Result<()> {
    if manager.get(&id).is_none() {
        bail!("session 不存在");
    }
    let mut injector = Injector::spawn()?;
    while let Some(message) = socket.recv().await {
        match message? {
            Message::Binary(data) => {
                // type=6 是控制延迟探测：原样回显，不注入。
                if data.first() == Some(&6) {
                    socket.send(Message::Binary(data)).await?;
                    continue;
                }
                handle_control_binary(&mut injector, &data)?;
            }
            Message::Text(text) => handle_control_text(&mut injector, text.as_str())?,
            Message::Close(_) => break,
            _ => {}
        }
    }
    Ok(())
}

// 二进制协议（控制端 -> agent）：
//   [1, dx i16, dy i16]                 鼠标相对移动
//   [2, code u16, down u8]              鼠标按键（code 用 evdev BTN_*）
//   [3, code u16, down u8]              键盘按键（code 用 evdev KEY_*）
//   [4, delta i16]                      滚轮
//   [5, utf8 text...]                   剪贴板（控制端 -> 被控端）
fn handle_control_binary(injector: &mut Injector, data: &[u8]) -> Result<()> {
    let Some(kind) = data.first().copied() else {
        return Ok(());
    };
    match kind {
        1 if data.len() >= 5 => {
            let dx = i16::from_le_bytes([data[1], data[2]]);
            let dy = i16::from_le_bytes([data[3], data[4]]);
            injector.mouse_move(dx, dy)?;
        }
        2 if data.len() >= 4 => {
            let code = u16::from_le_bytes([data[1], data[2]]);
            injector.mouse_button(code, data[3] != 0)?;
        }
        3 if data.len() >= 4 => {
            let code = u16::from_le_bytes([data[1], data[2]]);
            injector.key(code, data[3] != 0)?;
        }
        4 if data.len() >= 3 => {
            let delta = i16::from_le_bytes([data[1], data[2]]);
            injector.wheel(delta)?;
        }
        5 if data.len() > 1 => {
            let text = String::from_utf8_lossy(&data[1..]);
            crate::input::set_clipboard(&text)?;
        }
        _ => {}
    }
    Ok(())
}

fn handle_control_text(injector: &mut Injector, text: &str) -> Result<()> {
    let value: Value = serde_json::from_str(text)?;
    match value.get("t").and_then(Value::as_str).unwrap_or_default() {
        "move" => {
            let dx = value.get("dx").and_then(Value::as_i64).unwrap_or(0) as i16;
            let dy = value.get("dy").and_then(Value::as_i64).unwrap_or(0) as i16;
            injector.mouse_move(dx, dy)?;
        }
        "button" => {
            let code = value.get("code").and_then(Value::as_u64).unwrap_or(0) as u16;
            let down = value.get("down").and_then(Value::as_bool).unwrap_or(false);
            injector.mouse_button(code, down)?;
        }
        "key" => {
            let code = value.get("code").and_then(Value::as_u64).unwrap_or(0) as u16;
            let down = value.get("down").and_then(Value::as_bool).unwrap_or(false);
            injector.key(code, down)?;
        }
        "wheel" => {
            let delta = value.get("delta").and_then(Value::as_i64).unwrap_or(0) as i16;
            injector.wheel(delta)?;
        }
        "clipboard" => {
            if let Some(text) = value.get("text").and_then(Value::as_str) {
                crate::input::set_clipboard(text)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn safe_component(value: &str) -> Option<String> {
    let name = std::path::Path::new(value).file_name()?.to_str()?;
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some(name.to_string())
}

/// 单文件上传上限（默认 512 MB；超过报 413）。
const MAX_UPLOAD_BYTES: u64 = 512 * 1024 * 1024;

/// 禁止的可执行/脚本后缀（防止把 agent 变成远程执行入口）。
const FORBIDDEN_EXTS: &[&str] = &[
    ".exe", ".scr", ".bat", ".cmd", ".com", ".pif", ".sh", ".bash", ".zsh", ".ps1", ".psm1",
    ".vbs", ".vbe", ".js", ".jse", ".wsf", ".wsh", ".msi", ".msp", ".dll", ".so", ".dylib",
    ".app", ".deb", ".rpm", ".apk", ".jar", ".py", ".rb", ".pl",
];

fn is_forbidden_upload(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    FORBIDDEN_EXTS
        .iter()
        .any(|ext| lower.ends_with(ext))
}

fn session_files_dir(id: &str) -> Option<PathBuf> {
    let id = safe_component(id)?;
    Some(crate::home_dir().join("Downloads/lan_video").join(id))
}

async fn upload_file(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if !session_auth(&state, &id, query.token.as_deref(), &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }
    let Some(name) = safe_component(&query.name) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid file name"})),
        )
            .into_response();
    };
    if is_forbidden_upload(&name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "file type not allowed"})),
        )
            .into_response();
    }
    let Some(dir) = session_files_dir(&id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid session"})),
        )
            .into_response();
    };
    if let Err(err) = tokio::fs::create_dir_all(&dir).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response();
    }
    let path = dir.join(&name);
    let mut file = match tokio::fs::File::create(&path).await {
        Ok(file) => file,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response();
        }
    };
    let mut stream = body.into_data_stream();
    let mut size = 0u64;
    while let Some(chunk) = stream.next().await {
        let bytes = match chunk {
            Ok(bytes) => bytes,
            Err(err) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": err.to_string()})),
                )
                    .into_response();
            }
        };
        if let Err(err) = file.write_all(&bytes).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response();
        }
        size += bytes.len() as u64;
        if size > MAX_UPLOAD_BYTES {
            drop(file);
            let _ = tokio::fs::remove_file(&path).await;
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({"error": format!("file exceeds {} MB limit", MAX_UPLOAD_BYTES / (1024 * 1024))})),
            )
                .into_response();
        }
    }
    if let Err(err) = file.flush().await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response();
    }
    Json(json!({"ok": true, "name": name, "size": size, "path": path})).into_response()
}

async fn list_files(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
) -> Response {
    if !session_auth(&state, &id, query.token.as_deref(), &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }
    let Some(dir) = session_files_dir(&id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid session"})),
        )
            .into_response();
    };
    let mut files = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
            files.push(json!({"name": name, "size": size}));
        }
    }
    Json(json!({"files": files})).into_response()
}

async fn download_file(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
) -> Response {
    if !session_auth(&state, &id, query.token.as_deref(), &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }
    let (Some(name), Some(dir)) = (safe_component(&name), session_files_dir(&id)) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid path"})),
        )
            .into_response();
    };
    let file = match tokio::fs::File::open(dir.join(&name)).await {
        Ok(file) => file,
        Err(err) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": err.to_string()})),
            )
                .into_response();
        }
    };
    let stream = ReaderStream::new(file);
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")).unwrap(),
    );
    response
}

fn check_auth(state: &AppState, headers: &HeaderMap) -> Result<(), Response> {
    let Some(expected) = &state.token else {
        return Ok(());
    };
    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if provided == format!("Bearer {expected}") {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        )
            .into_response())
    }
}

fn bad_request(err: impl std::fmt::Display) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": err.to_string() })),
    )
        .into_response()
}

async fn cors(req: Request, next: Next) -> Response {
    let preflight = req.method() == Method::OPTIONS;
    let mut response = if preflight {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(req).await
    };
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET,POST,DELETE,OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("content-type,authorization"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 文件类型限制：可执行/脚本后缀被拒，普通文件放行。
    #[test]
    fn upload_rejects_dangerous_extensions() {
        assert!(is_forbidden_upload("virus.exe"));
        assert!(is_forbidden_upload("run.sh"));
        assert!(is_forbidden_upload("payload.py"));
        assert!(is_forbidden_upload("evil.JS"));
        assert!(is_forbidden_upload("lib.so"));
        assert!(!is_forbidden_upload("document.pdf"));
        assert!(!is_forbidden_upload("photo.jpg"));
        assert!(!is_forbidden_upload("data.txt"));
    }

    /// safe_component 防目录穿越。
    #[test]
    fn safe_component_blocks_traversal() {
        assert_eq!(safe_component("ok.txt").as_deref(), Some("ok.txt"));
        assert_eq!(safe_component("../etc/passwd").as_deref(), Some("passwd"));
        assert!(safe_component("").is_none());
        assert!(safe_component(".").is_none());
        assert!(safe_component("..").is_none());
    }
}
