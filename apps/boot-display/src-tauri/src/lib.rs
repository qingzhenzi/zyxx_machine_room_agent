//! QEStudio boot display: fullscreen, agent-driven presentation layer.
//!
//! This process is deliberately "dumb": qe-agent spawns and supervises it,
//! and every scene switch comes from the agent over IPC. No business logic
//! lives here.

mod ipc;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use tauri::{Manager, WindowEvent};

#[tauri::command]
fn display_state() -> ipc::DisplayState {
    let state = ipc::display_state();
    tracing_on(&state);
    state
}

/// 轮询日志(stderr;systemd 下进 journal)。
fn tracing_on(state: &ipc::DisplayState) {
    eprintln!(
        "[boot-display] v={} template={}",
        state.display.version, state.display.template
    );
}

/// 调试解锁：Ctrl+Alt+Shift+U 输入密码后，通知 agent 停止 supervise 并退出。
#[tauri::command]
fn unlock_display(password: String, app: tauri::AppHandle) -> Result<(), String> {
    let expected = std::env::var("QE_BOOT_DISPLAY_DEBUG_PASSWORD").unwrap_or_default();
    if expected.is_empty() {
        return Err("未配置 QE_BOOT_DISPLAY_DEBUG_PASSWORD，调试解锁已禁用".into());
    }
    if password.as_bytes() != expected.as_bytes() {
        return Err("调试密码错误".into());
    }
    let socket = std::env::var("QE_AGENT_IPC_SOCKET")
        .unwrap_or_else(|_| "/run/qe-agent/agent.sock".to_owned());
    let mut stream = UnixStream::connect(&socket)
        .map_err(|error| format!("连接 qe-agent IPC 失败（{socket}）：{error}"))?;
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(3)));
    let body = "{}";
    let request = format!(
        "POST /v1/display/unlock HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| error.to_string())?;
    let _ = stream.flush();
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| error.to_string())?;
    if !response.starts_with("HTTP/1.1 200") && !response.starts_with("HTTP/1.0 200") {
        return Err(format!(
            "agent 拒绝解锁：{}",
            response.lines().next().unwrap_or("")
        ));
    }
    app.exit(0);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![display_state, unlock_display])
        .on_window_event(|_window, event| {
            // 防止 Alt+F4 / 窗口关闭请求结束 kiosk 展示层；
            // 正常退出只能走调试密码解锁或 agent 主动结束进程。
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
            }
        })
        .setup(|app| {
            // 调试窗口只允许显式开启，正式 boot 画面不能带 devtools。
            if std::env::var("QE_BOOT_DISPLAY_DEVTOOLS").as_deref() == Ok("1")
                && let Some(w) = app.get_webview_window("main")
            {
                w.open_devtools();
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
