//! Desktop session detection and Linux/KDE always-on-top capability reporting.
//!
//! Wayland has no client-side keep-above protocol, but KWin exposes its internal
//! "keep above" state through the scripting interface. KDE users can therefore
//! still pin windows on native Wayland; other compositors cannot.

#[cfg(target_os = "linux")]
pub mod kwin;
pub mod pin;

/// Capabilities advertised to the frontend "about" panel.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopCapabilities {
    /// Actual windowing backend the app is running on.
    pub session: &'static str,
    /// Whether a pinned detached window can really stay above other windows.
    pub always_on_top_supported: bool,
    /// Which mechanism keeps the window above others.
    pub always_on_top_backend: &'static str,
}

/// Detect the actual windowing backend the app is running on.
pub fn session() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        let gdk_backend = std::env::var("GDK_BACKEND").unwrap_or_default();
        let primary = gdk_backend.split(',').next().unwrap_or_default().trim();
        if primary == "x11" {
            return "x11";
        }
        if primary == "wayland" {
            return "wayland";
        }
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            return "wayland";
        }
        if std::env::var_os("DISPLAY").is_some() {
            return "x11";
        }
        "unknown"
    }
    #[cfg(not(target_os = "linux"))]
    {
        "native"
    }
}

/// Whether the current desktop environment is KDE.
#[cfg(target_os = "linux")]
pub fn is_kde_desktop() -> bool {
    [
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "DESKTOP_SESSION",
    ]
    .iter()
    .filter_map(|key| std::env::var(key).ok())
    .any(|value| value.to_ascii_lowercase().contains("kde"))
}

/// Whether the KWin scripting bridge can be reached on this machine.
pub fn kwin_pin_supported() -> bool {
    #[cfg(target_os = "linux")]
    {
        kwin::available()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Resolve how always-on-top behaves on the current desktop session.
pub fn capabilities() -> DesktopCapabilities {
    let session = session();

    if session == "wayland" {
        #[cfg(target_os = "linux")]
        if is_kde_desktop() && kwin_pin_supported() {
            return DesktopCapabilities {
                session,
                always_on_top_supported: true,
                always_on_top_backend: "kwin-scripting",
            };
        }
        return DesktopCapabilities {
            session,
            always_on_top_supported: false,
            always_on_top_backend: "unsupported",
        };
    }

    DesktopCapabilities {
        session,
        always_on_top_supported: true,
        always_on_top_backend: "native",
    }
}
