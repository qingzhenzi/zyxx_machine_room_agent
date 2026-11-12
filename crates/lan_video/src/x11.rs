//! X11 窗口枚举（用于 `--x11-window`）。
//!
//! 依赖 `xdotool`（X11 会话或 XWayland 下可用）。

use serde::Serialize;
use std::process::Command;

#[derive(Serialize, Clone, Debug)]
pub struct WindowInfo {
    /// XID（十进制），传给 `--x11-window`。
    pub id: u64,
    pub title: String,
    pub app: String,
    pub backend: &'static str,
}

pub fn list_windows() -> Vec<WindowInfo> {
    let ids = command_stdout("xdotool", &["search", "--onlyvisible", "--name", ".*"]);
    let Some(ids) = ids else {
        return Vec::new();
    };
    let mut windows = Vec::new();
    for line in ids.lines() {
        let Ok(id) = line.trim().parse::<u64>() else {
            continue;
        };
        let title = command_stdout("xdotool", &["getwindowname", &id.to_string()])
            .unwrap_or_default()
            .trim()
            .to_string();
        if title.is_empty() {
            continue;
        }
        let app = command_stdout("xdotool", &["getwindowclassname", &id.to_string()])
            .unwrap_or_default()
            .trim()
            .to_string();
        windows.push(WindowInfo {
            id,
            title,
            app,
            backend: "x11",
        });
    }
    windows
}

fn command_stdout(bin: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(bin).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}
