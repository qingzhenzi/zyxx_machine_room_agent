//! QEStudio boot display: fullscreen, agent-driven presentation layer.
//!
//! This process is deliberately "dumb": qe-agent spawns and supervises it,
//! and every scene switch comes from the agent over IPC. No business logic
//! lives here.

mod ipc;

#[tauri::command]
fn display_state() -> Result<ipc::DisplayState, String> {
    Ok(ipc::display_state())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![display_state])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
