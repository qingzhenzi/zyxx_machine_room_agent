//! Small window-focusing helpers shared by the tray, the single-instance
//! handler and the window commands.

use tauri::{Manager, WebviewWindow};

/// Bring the application's main window to the foreground.
///
/// Does nothing when the window no longer exists (for example while the app is
/// shutting down).
pub fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        focus_window(&window);
    }
}

/// Show, unminimize and focus a window, ignoring transient platform failures.
pub fn focus_window(window: &WebviewWindow) {
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
}
