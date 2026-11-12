//! 采集设备枚举（音频）。
//!
//! Linux 走 `pactl -f json list sources`（能看到 monitor=系统声音 与麦克风）；
//! Windows 走 `gst-device-monitor-1.0 Audio/Source`，并额外提供 loopback 虚拟项。

use serde::Serialize;
use std::process::Command;

#[derive(Serialize, Clone, Debug)]
pub struct AudioDevice {
    /// 传给 `audio_device` 的值。
    pub id: String,
    /// 人读名称。
    pub name: String,
    /// `system`（系统声音）或 `mic`（麦克风）。
    pub kind: &'static str,
    pub default: bool,
}

pub fn audio_devices() -> Vec<AudioDevice> {
    #[cfg(target_os = "linux")]
    {
        linux_audio_devices()
    }
    #[cfg(not(target_os = "linux"))]
    {
        windows_audio_devices()
    }
}

#[cfg(target_os = "linux")]
fn linux_audio_devices() -> Vec<AudioDevice> {
    let default_sink = command_stdout("pactl", &["get-default-sink"]);
    let default_source = command_stdout("pactl", &["get-default-source"]);
    let default_monitor = default_sink
        .as_deref()
        .map(|sink| format!("{}.monitor", sink.trim()));

    let output = Command::new("pactl")
        .args(["-f", "json", "list", "sources"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        return Vec::new();
    };

    let mut devices = Vec::new();
    for item in items {
        let Some(id) = item.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let props = item.get("properties");
        let description = props
            .and_then(|p| p.get("device.description"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                props
                    .and_then(|p| p.get("node.nick"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or(id);
        let is_monitor = id.ends_with(".monitor");
        let default = if is_monitor {
            default_monitor.as_deref() == Some(id)
        } else {
            default_source.as_deref().map(str::trim) == Some(id)
        };
        devices.push(AudioDevice {
            id: id.to_string(),
            name: description.to_string(),
            kind: if is_monitor { "system" } else { "mic" },
            default,
        });
    }
    devices
}

#[cfg(not(target_os = "linux"))]
fn windows_audio_devices() -> Vec<AudioDevice> {
    let mut devices = vec![AudioDevice {
        id: "@loopback".to_string(),
        name: "系统声音（WASAPI loopback）".to_string(),
        kind: "system",
        default: true,
    }];
    if let Ok(output) = Command::new("gst-device-monitor-1.0")
        .args(["Audio/Source"])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let line = line.trim();
            if let Some(name) = line.strip_prefix("name  :") {
                let name = name.trim();
                if name.is_empty() {
                    continue;
                }
                devices.push(AudioDevice {
                    id: name.to_string(),
                    name: name.to_string(),
                    kind: "mic",
                    default: false,
                });
            }
        }
    }
    devices
}

#[cfg(target_os = "linux")]
fn command_stdout(bin: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(bin).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}
