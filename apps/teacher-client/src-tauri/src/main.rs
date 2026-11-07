//! Room Node 教师端桌面客户端（Tauri v2 MVP）。
//!
//! 设计原则：**所有跨域 HTTP 都在 Rust 侧完成**。
//! 控制节点（`http://127.0.0.1:5174` 或生产 Web 入口）默认只允许 console 源做 CORS，
//! Tauri WebView 的源是 `tauri://localhost` / `http://tauri.localhost`，直接 fetch 会被
//! CORS 拦截，因此前端统一通过 `api_request` 命令走 reqwest。这样：
//!
//! - 不依赖 `tauri-plugin-http` 的 JS 侧 ACL 作用域，capabilities 只需 `core:default`；
//! - 登录 token 只在 WebView（localStorage）与 Rust 进程内存之间传递，不落到磁盘；
//! - 「在浏览器中打开教师工作台」由 `open_external` 调系统默认浏览器。
//!
//! 当前阶段（MVP）只做只读展示，不引入托盘/自动更新/通知等插件。

use std::time::Duration;

use serde::Serialize;
use tauri::command;

/// 控制节点 API 单次请求超时。登录/列表接口都很轻，20s 足够。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Rust → WebView 的统一响应：`status` 是 HTTP 状态码，`body` 已解析为 JSON
/// （非 JSON 响应退化为字符串，方便展示服务端错误页）。
#[derive(Debug, Serialize)]
pub struct ApiResponse {
    status: u16,
    body: serde_json::Value,
}

/// 只允许 http/https，避免命令被当成任意 scheme（file://、jar: 等）的代理。
fn validate_http_url(raw: &str) -> Result<reqwest::Url, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("URL 不能为空".to_owned());
    }
    let url = reqwest::Url::parse(trimmed).map_err(|error| format!("非法 URL：{error}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        scheme => Err(format!("只允许 http/https URL，收到 {scheme}")),
    }
}

/// `POST /api/v1/auth/login` 等控制节点 API 的通用 HTTP 代理。
///
/// WebView 通过 `window.__TAURI__.core.invoke('api_request', {...})` 调用。
#[command]
async fn api_request(
    method: String,
    url: String,
    token: Option<String>,
    body: Option<serde_json::Value>,
) -> Result<ApiResponse, String> {
    let url = validate_http_url(&url)?;
    let method = match method.to_ascii_uppercase().as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "PATCH" => reqwest::Method::PATCH,
        "DELETE" => reqwest::Method::DELETE,
        other => return Err(format!("不支持的 HTTP 方法：{other}")),
    };

    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent("room-node-teacher-client/0.1.0")
        .build()
        .map_err(|error| format!("初始化 HTTP 客户端失败：{error}"))?;

    let mut request = client.request(method, url);
    if let Some(token) = token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }

    let response = request
        .send()
        .await
        .map_err(|error| format!("请求控制节点失败：{error}"))?;
    let status = response.status().as_u16();
    let text = response
        .text()
        .await
        .map_err(|error| format!("读取控制节点响应失败：{error}"))?;

    // 控制节点错误体是 `{ error: { code, message } }`；正常响应是 JSON。
    // 极端情况下（反向代理 502 的 HTML 页）退化成字符串，前端会把 status 展示出来。
    let parsed = if text.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::Value::String(text))
    };

    Ok(ApiResponse {
        status,
        body: parsed,
    })
}

/// 用系统默认浏览器打开教师工作台（`{base}/teacher`）。
#[command]
fn open_external(url: String) -> Result<(), String> {
    let url = validate_http_url(&url)?.to_string();

    #[cfg(target_os = "linux")]
    let spawn = std::process::Command::new("xdg-open").arg(&url).spawn();
    #[cfg(target_os = "macos")]
    let spawn = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(target_os = "windows")]
    let spawn = std::process::Command::new("cmd")
        .args(["/C", "start", "", &url])
        .spawn();

    spawn
        .map(|_child| ())
        .map_err(|error| format!("无法打开系统浏览器（请确认已安装 xdg-open/open）：{error}"))
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![api_request, open_external])
        .run(tauri::generate_context!())
        .expect("启动 Room Node 教师端失败");
}
