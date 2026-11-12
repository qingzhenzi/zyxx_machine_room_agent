//! System tray icon and its menu.
//!
//! The agent client starts hidden: the tray is the only always-visible entry
//! point on first launch, so this module is what makes the app reachable.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Emitter;

use crate::windowing::show_main_window;

/// Install the tray icon shown while the app keeps running in the background.
pub fn setup(app: &mut tauri::App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open QEStudio Agent", true, None::<&str>)?;
    let logs = MenuItem::with_id(app, "logs", "Open Logs", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit QEStudio Agent", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &logs, &quit])?;

    let mut builder = TrayIconBuilder::with_id("qe-agent-tray")
        .tooltip("QEStudio Agent")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            // Show the window and let the webview route to /logs.
            "logs" => {
                show_main_window(app);
                let _ = app.emit("native-menu", "logs");
            }
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
        });

    // The icon is optional: a missing application icon must not abort startup.
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder.build(app)?;

    Ok(())
}
