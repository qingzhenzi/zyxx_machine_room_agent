//! Window-caption pinning helpers for detached windows.
//!
//! Wayland has no keep-above protocol, so pinned detached windows are applied
//! through KWin's scripting API on KDE Plasma (GTK/Tauri cannot do it directly).

#[cfg(target_os = "linux")]
use tauri::Manager;

#[cfg(target_os = "linux")]
use crate::desktop::{is_kde_desktop, kwin, session};

/// Hidden per-window marker appended to the native title on KDE Wayland.
///
/// KWin scripting can only match windows by caption, and several detached
/// windows may share the same visible title. Encoding a stable hash of the
/// window label as zero-width characters lets the pin script target exactly one
/// window (the browser/taskbar still renders the plain title).
#[cfg(target_os = "linux")]
pub fn pin_marker(label: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in label.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }

    let mut marker = String::from('\u{2063}');
    for bit in 0..64 {
        marker.push(if (hash >> bit) & 1 == 1 {
            '\u{200c}'
        } else {
            '\u{200b}'
        });
    }
    marker.push('\u{2063}');
    marker
}

/// Native title for a detached window, with a KWin pin marker when needed.
#[cfg(target_os = "linux")]
pub fn detached_native_title(title: &str, label: &str) -> String {
    if session() == "wayland" && is_kde_desktop() {
        format!("{title}{}", pin_marker(label))
    } else {
        title.to_string()
    }
}

#[cfg(not(target_os = "linux"))]
pub fn detached_native_title(title: &str, _label: &str) -> String {
    title.to_string()
}

/// Apply the keep-above state through KWin scripting (KDE Wayland only).
#[cfg(target_os = "linux")]
pub fn sync_wayland_pin(app: &tauri::AppHandle, label: &str, native_title: &str, pinned: bool) {
    if session() != "wayland" || !is_kde_desktop() {
        return;
    }

    let plugin = format!("qestudio-pin-{label}");
    let script_dir = app
        .path()
        .app_cache_dir()
        .map(|dir| dir.join("kwin"))
        .unwrap_or_else(|_| std::env::temp_dir().join("qestudio-kwin"));

    if let Err(error) = kwin::apply(&script_dir, &plugin, native_title, pinned) {
        eprintln!("failed to sync KWin always-on-top: {error}");
    }
}

#[cfg(not(target_os = "linux"))]
pub fn sync_wayland_pin(_app: &tauri::AppHandle, _label: &str, _title: &str, _pinned: bool) {}

/// Unload the KWin pin script once its window is destroyed.
#[cfg(target_os = "linux")]
pub fn attach_wayland_pin_cleanup(window: &tauri::WebviewWindow, label: &str) {
    if session() != "wayland" || !is_kde_desktop() {
        return;
    }

    let plugin = format!("qestudio-pin-{label}");
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            kwin::unload(&plugin);
        }
    });
}

#[cfg(not(target_os = "linux"))]
pub fn attach_wayland_pin_cleanup(_window: &tauri::WebviewWindow, _label: &str) {}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::pin_marker;

    #[test]
    fn marker_is_stable_for_the_same_label() {
        assert_eq!(pin_marker("popout-42"), pin_marker("popout-42"));
    }

    #[test]
    fn marker_differs_between_labels() {
        assert_ne!(pin_marker("popout-42"), pin_marker("popout-43"));
    }

    #[test]
    fn marker_is_wrapped_in_zero_width_characters() {
        let marker = pin_marker("popout-42");
        assert_eq!(marker.chars().count(), 66);
        assert!(marker.starts_with('\u{2063}'));
        assert!(marker.ends_with('\u{2063}'));
        assert!(marker
            .chars()
            .all(|character| matches!(character, '\u{2063}' | '\u{200b}' | '\u{200c}')));
    }
}
