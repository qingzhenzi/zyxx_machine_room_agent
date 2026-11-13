//! 物理键盘调试快捷键：Ctrl+Alt+Shift+U。
//!
//! Wayland/KWin 下 Tauri 的全局快捷键和 webview keydown 都可能拿不到键，
//! 这里由 root agent 直接读 /dev/input 键盘设备；命中后写一个触发文件，
//! boot-display 轮询到后弹出原生密码框。只用于 kiosk 调试。

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use evdev::{Device, EventType, KeyCode};

const TRIGGER: &str = "/run/qe-agent/debug-unlock";

pub fn spawn() {
    if std::env::var("QE_BOOT_DISPLAY_DEBUG_PASSWORD")
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
    {
        return;
    }
    thread::Builder::new()
        .name("qe-debug-hotkey".to_owned())
        .spawn(scan_loop)
        .ok();
}

/// 周期性扫描 /dev/input：新插入的键盘会被自动纳入，拔掉后 watcher 退出。
fn scan_loop() {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    let known: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
    loop {
        for path in keyboard_devices() {
            let mut guard = match known.lock() {
                Ok(guard) => guard,
                Err(_) => return,
            };
            if !guard.insert(path.clone()) {
                continue;
            }
            eprintln!("qe-debug-hotkey: 监听键盘 {}", path.display());
            drop(guard);
            let known = known.clone();
            thread::Builder::new()
                .name("qe-debug-key".to_owned())
                .spawn(move || watch_device(path, known))
                .ok();
        }
        thread::sleep(Duration::from_secs(2));
    }
}

fn keyboard_devices() -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir("/dev/input") else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("event"))
        })
        .filter(|path| {
            let Ok(device) = Device::open(path) else {
                return false;
            };
            device
                .supported_keys()
                .is_some_and(|keys| keys.contains(KeyCode::KEY_U) && keys.contains(KeyCode::KEY_LEFTCTRL))
        })
        .collect()
}

fn watch_device(path: PathBuf, known: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<PathBuf>>>) {
    let Ok(mut device) = Device::open(&path) else {
        return;
    };
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut u_down = false;
    loop {
        let events = match device.fetch_events() {
            Ok(events) => events,
            Err(error) => {
                eprintln!("qe-debug-hotkey: 读取 {} 失败: {error}", path.display());
                if let Ok(mut guard) = known.lock() {
                    guard.remove(&path);
                }
                return;
            }
        };
        for event in events {
            if event.event_type() != EventType::KEY {
                continue;
            }
            let key = KeyCode::new(event.code());
            let pressed = event.value() == 1;
            let released = event.value() == 0;
            match key {
                KeyCode::KEY_LEFTCTRL | KeyCode::KEY_RIGHTCTRL => ctrl = pressed,
                KeyCode::KEY_LEFTALT | KeyCode::KEY_RIGHTALT => alt = pressed,
                KeyCode::KEY_LEFTSHIFT | KeyCode::KEY_RIGHTSHIFT => shift = pressed,
                KeyCode::KEY_U if pressed => u_down = true,
                KeyCode::KEY_U if released => u_down = false,
                _ => {}
            }
            if ctrl && alt && shift && u_down {
                if let Some(parent) = std::path::Path::new(TRIGGER).parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let _ = fs::write(TRIGGER, b"1");
                eprintln!("qe-debug-hotkey: Ctrl+Alt+Shift+U 触发");
                u_down = false;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
}
