//! Commands that create or focus OS windows.

use serde::Deserialize;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::desktop::pin::{attach_wayland_pin_cleanup, detached_native_title, sync_wayland_pin};
use crate::error::AppResult;
use crate::windowing::focus_window;

/// Options sent by the frontend when opening a detached content window.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachedWindowOptions {
    /// Unique window label, also used to address the window later.
    pub label: String,
    /// Visible window title.
    pub title: String,
    /// Frontend route rendered inside the window (without the leading `#`).
    pub route: String,
    pub width: f64,
    pub height: f64,
    pub resizable: bool,
    pub always_on_top: bool,
}

/// Open (or focus) a multi-workspace window at `route`.
#[tauri::command]
pub fn open_workspace_window(
    app: tauri::AppHandle,
    label: String,
    title: String,
    route: String,
) -> AppResult<()> {
    if let Some(window) = app.get_webview_window(&label) {
        focus_window(&window);
        return Ok(());
    }

    WebviewWindowBuilder::new(
        &app,
        &label,
        WebviewUrl::App(format!("index.html#{route}").into()),
    )
    .title(title)
    .inner_size(1280.0, 820.0)
    .min_inner_size(980.0, 640.0)
    .build()?;

    Ok(())
}

/// Open a lightweight OS window that only renders detached content.
///
/// The frontend stores the payload in localStorage first; this command only
/// creates (or focuses) the webview and points it at the `/detached/:id` route.
#[tauri::command]
pub fn open_detached_window(app: AppHandle, options: DetachedWindowOptions) -> AppResult<()> {
    let native_title = detached_native_title(&options.title, &options.label);

    if let Some(window) = app.get_webview_window(&options.label) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_title(&native_title);
        // Mode can change between opens (fixed / movable / resizable / free).
        let _ = window.set_resizable(options.resizable);
        let _ = window.set_maximizable(options.resizable);
        let _ = window.set_always_on_top(options.always_on_top);
        if !options.resizable {
            let _ = window.unmaximize();
        }
        let _ = window.set_focus();
        sync_wayland_pin(&app, &options.label, &native_title, options.always_on_top);
        return Ok(());
    }

    let window = WebviewWindowBuilder::new(
        &app,
        &options.label,
        WebviewUrl::App(format!("index.html#{}", options.route).into()),
    )
    .title(native_title.clone())
    .inner_size(options.width, options.height)
    .min_inner_size(380.0, 240.0)
    .decorations(false)
    .resizable(options.resizable)
    .maximizable(options.resizable)
    .minimizable(true)
    .closable(true)
    .always_on_top(options.always_on_top)
    .center()
    .build()?;

    sync_wayland_pin(&app, &options.label, &native_title, options.always_on_top);
    attach_wayland_pin_cleanup(&window, &options.label);

    Ok(())
}
