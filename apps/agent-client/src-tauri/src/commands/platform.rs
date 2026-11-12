//! Commands describing the runtime environment to the "about" panel.

use crate::desktop::{self, DesktopCapabilities};

/// Small runtime facts surfaced in the desktop "关于" panel.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformInfo {
    pub app_version: String,
    pub tauri_version: String,
    pub os: &'static str,
    pub arch: &'static str,
}

#[tauri::command]
pub fn platform_info(app: tauri::AppHandle) -> PlatformInfo {
    PlatformInfo {
        app_version: app.package_info().version.to_string(),
        tauri_version: tauri::VERSION.to_string(),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
    }
}

#[tauri::command]
pub fn desktop_capabilities() -> DesktopCapabilities {
    desktop::capabilities()
}
