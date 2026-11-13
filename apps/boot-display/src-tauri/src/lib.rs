//! QEStudio boot display: fullscreen, agent-driven presentation layer.
//!
//! This process is deliberately "dumb": qe-agent spawns and supervises it,
//! and every scene switch comes from the agent over IPC. No business logic
//! lives here.

mod ipc;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

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
fn validate_password(password: &str) -> Result<(), String> {
    let expected = std::env::var("QE_BOOT_DISPLAY_DEBUG_PASSWORD").unwrap_or_default();
    if expected.is_empty() {
        return Err("未配置 QE_BOOT_DISPLAY_DEBUG_PASSWORD，调试解锁已禁用".into());
    }
    if password.as_bytes() != expected.as_bytes() {
        return Err("调试密码错误".into());
    }
    Ok(())
}

fn unlock_agent() -> Result<(), String> {
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
    Ok(())
}

#[tauri::command]
fn unlock_display(password: String, app: tauri::AppHandle) -> Result<(), String> {
    validate_password(&password)?;
    unlock_agent()?;
    app.exit(0);
    Ok(())
}

/// 全局快捷键路径不依赖 webview 事件：弹一个原生密码框，验证后直接退出。
fn unlock_via_native_dialog(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let output = std::process::Command::new("kdialog")
            .args(["--password", "QEStudio 调试解锁：输入调试密码"])
            .output()
            .or_else(|_| {
                std::process::Command::new("zenity")
                    .args(["--password", "--title=QEStudio 调试解锁"])
                    .output()
            });
        let Ok(output) = output else {
            eprintln!("无法启动 kdialog/zenity，调试解锁失败");
            return;
        };
        if !output.status.success() {
            return;
        }
        let password = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if let Err(error) = validate_password(&password) {
            eprintln!("QE debug unlock rejected: {error}");
            return;
        }
        if let Err(error) = unlock_agent() {
            eprintln!("QE debug unlock IPC failed: {error}");
            return;
        }
        app.exit(0);
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![display_state, unlock_display])
        .on_window_event(|_window, event| {
            // 防止 Alt+F4 / 窗口关闭请求结束 kiosk 展示层；
            // 正常退出只能走调试密码解锁或 agent 主动结束进程。
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
            }
        })
        .setup(|app| {
            // 全局调试快捷键：Wayland 下窗口不一定拿到焦点，不能只靠 JS keydown。
            let shortcut = Shortcut::new(
                Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT),
                Code::KeyU,
            );
            match app.global_shortcut().on_shortcut(
                shortcut,
                move |app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        eprintln!("QE debug hotkey pressed");
                        unlock_via_native_dialog(app.clone());
                    }
                },
            ) {
                Ok(()) => eprintln!("QE debug hotkey registered"),
                Err(error) => eprintln!("注册全局调试快捷键失败: {error}"),
            }

            // 兼容兜底：KDE 自定义快捷键/调试脚本可以 touch 此文件触发同一解锁框。
            let handle = app.handle().clone();
            let dialog_handle = app.handle().clone();
            std::thread::spawn(move || loop {
                for path in ["/tmp/qe-debug-request", "/run/qe-agent/debug-unlock"] {
                    if std::path::Path::new(path).exists() {
                        let _ = std::fs::remove_file(path);
                        let _ = dialog_handle.emit("qe-debug-unlock", ());
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(400));
            });
            let _ = handle;

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
