//! Native macOS menu bar.
//!
//! The window starts hidden and is reachable through the tray. On macOS a
//! minimal application menu keeps the standard Quit entry available; the
//! remaining actions are forwarded to the webview as `native-menu` events.

#[cfg(target_os = "macos")]
pub fn setup(app: &mut tauri::App) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
    use tauri::Emitter;

    let open = MenuItem::with_id(app, "open", "Open QEStudio Agent", true, None::<&str>)?;
    let logs = MenuItem::with_id(app, "logs", "Open Logs", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, Some("CmdOrCtrl+,"))?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = PredefinedMenuItem::quit(app, Some("Quit QEStudio Agent"))?;

    let app_menu = Submenu::with_items(
        app,
        "QEStudio Agent",
        true,
        &[&open, &logs, &settings, &separator, &quit],
    )?;
    let menu = Menu::with_items(app, &[&app_menu])?;
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| {
        let _ = app.emit("native-menu", event.id().as_ref());
    });
    Ok(())
}
