//! QEStudio Agent — machine agent GUI shell (Tauri application wiring).
//!
//! P3 scope: the window starts hidden and lives in the tray; the webview reads
//! daemon status/logs and triggers enrollment through the local `qe-agent` IPC
//! client in [`ipc`] (HTTP/1.1 over a Unix domain socket). Feature code lives in:
//!
//! - [`commands`] — `#[tauri::command]` handlers exposed to the webview
//! - [`ipc`] — HTTP/1.1-over-Unix-socket client for the local `qe-agent` daemon
//! - [`desktop`] — session detection and the KDE Wayland always-on-top bridge
//! - [`windowing`] — main-window focus helpers shared by tray/menu/single-instance
//! - [`tray`] / `menu` — native shell UI
//! - [`error`] — one error type for every fallible command

use tauri::{Emitter, WindowEvent};
use tauri_plugin_window_state::StateFlags;

mod commands;
mod desktop;
mod error;
mod ipc;
#[cfg(target_os = "macos")]
mod menu;
mod setup;
mod tray;
mod windowing;

/// Focus the already running instance and forward OS deep links to it.
///
/// `tauri-plugin-single-instance` must be the first registered plugin, so this
/// callback runs for any second launch instead of starting another process.
fn on_single_instance(app: &tauri::AppHandle, argv: Vec<String>, _cwd: String) {
    windowing::show_main_window(app);
    for argument in argv {
        if argument.starts_with("qestudio://") {
            let _ = app.emit("deep-link", argument);
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Single-instance has to be registered before every other plugin.
        .plugin(tauri_plugin_single_instance::init(on_single_instance))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(tauri_plugin_log::log::LevelFilter::Info)
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        // VISIBLE is intentionally not restored: the agent starts hidden every
        // launch and only the tray / single-instance handler may show the window.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(StateFlags::all() & !StateFlags::VISIBLE)
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            tray::setup(app)?;
            #[cfg(target_os = "macos")]
            menu::setup(app)?;
            Ok(())
        })
        // The agent lives in the tray: closing the window only hides it, while
        // Quit stays in the tray menu (and the macOS application menu).
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::platform::platform_info,
            commands::platform::desktop_capabilities,
            commands::files::save_text_file,
            commands::files::read_import_file,
            commands::keychain::keychain_get,
            commands::keychain::keychain_set,
            commands::keychain::keychain_delete,
            commands::windows::open_workspace_window,
            commands::windows::open_detached_window,
            ipc::agent_ipc_status,
            ipc::agent_ipc_logs,
            ipc::agent_ipc_policy,
            ipc::agent_ipc_enroll,
            ipc::agent_ipc_power,
            ipc::agent_ipc_endpoint,
            setup::agent_setup_probe,
            setup::agent_show_main_window,
            setup::agent_setup_install,
            setup::agent_setup_enroll,
            setup::agent_setup_service,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
