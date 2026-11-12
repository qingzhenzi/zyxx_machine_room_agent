//! QEStudio boot display: fullscreen, agent-driven presentation layer.
//!
//! This process is deliberately "dumb": qe-agent spawns and supervises it,
//! and every scene switch comes from the agent over IPC. No business logic
//! lives here.

mod ipc;

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![display_state])
        .setup(|app| {
            use tauri::Manager;
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
