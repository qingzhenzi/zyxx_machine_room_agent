use tauri::menu::{Menu, MenuItem};
#[cfg(target_os = "macos")]
use tauri::menu::Submenu;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

/// Small runtime facts surfaced in the settings "About" panel.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PlatformInfo {
    app_version: String,
    tauri_version: String,
    os: &'static str,
    arch: &'static str,
}

#[tauri::command]
fn platform_info(app: tauri::AppHandle) -> PlatformInfo {
    PlatformInfo {
        app_version: app.package_info().version.to_string(),
        tauri_version: tauri::VERSION.to_string(),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
    }
}

/// Write exported text to the user's Downloads directory.
///
/// Keeping this in Rust avoids granting the webview broad filesystem access.
#[tauri::command]
fn save_text_file(app: tauri::AppHandle, name: String, contents: String) -> Result<String, String> {
    let safe_name: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect();

    let directory = app
        .path()
        .download_dir()
        .map_err(|error| error.to_string())?;
    let path = directory.join(safe_name);
    std::fs::write(&path, contents).map_err(|error| error.to_string())?;

    Ok(path.display().to_string())
}

/// Open a route in a secondary window (used by the tab strip).
#[tauri::command]
fn open_workspace_window(
    app: tauri::AppHandle,
    label: String,
    title: String,
    route: String,
) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
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
    .build()
    .map_err(|error| error.to_string())?;

    Ok(())
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn app_name(app: &tauri::AppHandle) -> String {
    app.config()
        .product_name
        .clone()
        .unwrap_or_else(|| "Desktop Starter".to_string())
}

// The native menubar only makes sense on macOS, where it lives in the system
// menu bar. On Linux/Windows GTK/Win32 renders it as a strip *inside* the
// frameless window, above the custom title bar — the same commands are already
// covered by the command palette (Ctrl+K) and the tray menu.
#[cfg(target_os = "macos")]
fn setup_menu(app: &mut tauri::App) -> tauri::Result<()> {
    let name = app_name(app.handle());
    let new_item = MenuItem::with_id(app, "new-item", "New Item", true, Some("CmdOrCtrl+N"))?;
    let settings = MenuItem::with_id(app, "settings", "Settings", true, Some("CmdOrCtrl+,"))?;
    let palette = MenuItem::with_id(
        app,
        "command-palette",
        "Command Palette",
        true,
        Some("CmdOrCtrl+K"),
    )?;
    let quit = MenuItem::with_id(app, "quit", format!("Quit {name}"), true, Some("CmdOrCtrl+Q"))?;

    let file = Submenu::with_items(app, "File", true, &[&new_item, &settings, &quit])?;
    let view = Submenu::with_items(app, "View", true, &[&palette])?;
    let menu = Menu::with_items(app, &[&file, &view])?;

    app.set_menu(menu)?;
    app.on_menu_event(|app, event| {
        let id = event.id().as_ref();
        if id == "quit" {
            app.exit(0);
            return;
        }
        let _ = app.emit("native-menu", id);
    });

    Ok(())
}

fn setup_tray(app: &mut tauri::App) -> tauri::Result<()> {
    let name = app_name(app.handle());
    let open = MenuItem::with_id(app, "open", format!("Open {name}"), true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", format!("Quit {name}"), true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    TrayIconBuilder::with_id("main-tray")
        .tooltip(&name)
        .icon(
            app.default_window_icon()
                .expect("application icon is configured")
                .clone(),
        )
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            show_main_window(app);
            for argument in argv {
                if argument.starts_with("desktopstarter://") {
                    let _ = app.emit("deep-link", argument);
                }
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(tauri_plugin_log::log::LevelFilter::Info)
                .build(),
        )
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .setup(|app| {
            setup_tray(app)?;
            #[cfg(target_os = "macos")]
            setup_menu(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            platform_info,
            save_text_file,
            open_workspace_window
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
