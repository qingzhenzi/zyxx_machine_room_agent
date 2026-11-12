//! KDE Wayland always-on-top bridge (KWin scripting over D-Bus).
//!
//! Wayland has no client-side keep-above protocol, but KWin exposes its internal
//! "keep above" state through the scripting interface. KDE users can therefore
//! still pin windows on native Wayland; other compositors cannot.

use std::path::Path;

const DESTINATION: &str = "org.kde.KWin";
const OBJECT_PATH: &str = "/Scripting";
const INTERFACE: &str = "org.kde.kwin.Scripting";

fn connection() -> Result<zbus::blocking::Connection, String> {
    zbus::blocking::Connection::session().map_err(|error| error.to_string())
}

/// Whether a session D-Bus connection can be opened at all.
pub fn available() -> bool {
    connection().is_ok()
}

fn script(caption: &str, pinned: bool) -> String {
    let caption = serde_json::to_string(caption).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        "const CAPTION = {caption};\n\
         const PINNED = {};\n\
         function applyPin(window) {{\n\
           if (String(window.caption) === CAPTION) window.keepAbove = PINNED;\n\
         }}\n\
         const windows = workspace.windowList();\n\
         for (let index = 0; index < windows.length; index++) applyPin(windows[index]);\n\
         if (PINNED && typeof workspace.windowAdded === 'function') {{\n\
           workspace.windowAdded.connect(applyPin);\n\
         }}\n",
        if pinned { "true" } else { "false" }
    )
}

/// Write the pin script and load it through KWin's scripting D-Bus interface.
pub fn apply(script_dir: &Path, plugin: &str, caption: &str, pinned: bool) -> Result<(), String> {
    std::fs::create_dir_all(script_dir).map_err(|error| error.to_string())?;
    let script_path = script_dir.join(format!("{plugin}.js"));
    std::fs::write(&script_path, script(caption, pinned)).map_err(|error| error.to_string())?;

    let connection = connection()?;
    // Replace previous state so a stale caption/handler cannot linger.
    let _ = connection.call_method(
        Some(DESTINATION),
        OBJECT_PATH,
        Some(INTERFACE),
        "unloadScript",
        &plugin,
    );

    let script_path = script_path.to_string_lossy().into_owned();
    connection
        .call_method(
            Some(DESTINATION),
            OBJECT_PATH,
            Some(INTERFACE),
            "loadScript",
            &(script_path.as_str(), plugin),
        )
        .map_err(|error| error.to_string())?;
    connection
        .call_method(
            Some(DESTINATION),
            OBJECT_PATH,
            Some(INTERFACE),
            "start",
            &(),
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Unload a previously loaded pin script (best effort).
pub fn unload(plugin: &str) {
    if let Ok(connection) = connection() {
        let _ = connection.call_method(
            Some(DESTINATION),
            OBJECT_PATH,
            Some(INTERFACE),
            "unloadScript",
            &plugin,
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::script;

    #[test]
    fn caption_is_json_escaped() {
        let js = script("quote\" backslash\\ tab\t", true);
        assert!(js.contains(r#"const CAPTION = "quote\" backslash\\ tab\t";"#));
        assert!(!js.contains("const CAPTION = \"quote\" backslash"));
    }

    #[test]
    fn pinned_script_connects_window_added_handler() {
        let js = script("QEStudio", true);
        assert!(js.contains("const PINNED = true;"));
        assert!(js.contains("workspace.windowAdded.connect(applyPin);"));
    }

    #[test]
    fn unpinned_script_guards_window_added_handler() {
        let js = script("QEStudio", false);
        assert!(js.contains("const PINNED = false;"));
        // The handler is always emitted, but guarded by `PINNED` so it is a
        // no-op for unpinned windows.
        assert!(js.contains("if (PINNED && typeof workspace.windowAdded === 'function')"));
    }

    #[test]
    fn script_matches_caption_as_string() {
        let js = script("QEStudio", false);
        assert!(js.contains("if (String(window.caption) === CAPTION) window.keepAbove = PINNED;"));
    }
}
