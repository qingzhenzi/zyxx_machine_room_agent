//! KDE KWin 免授权采集：`zkde_screencast_unstable_v1` 的 Rust 封装。
//!
//! C 侧（`src/kwin_capture.c`）通过 Wayland 直接向 KWin 申请 PipeWire 流，
//! 把 node id 打到 stdout（`NODE=<id>`），然后保持连接；调用方用
//! `pipewiresrc path=<node>` 消费这条流。
//!
//! 权限是 KWin 的静态机制：客户端的可执行文件必须有一个 desktop 文件声明
//! `X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1`。`ensure_permission_file`
//! 会在用户目录生成该文件（不需要 root、不弹窗）。

use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::ffi::CString;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

unsafe extern "C" {
    fn lan_video_kwin_capture(window_uuid: *const std::ffi::c_char, pointer_mode: i32) -> i32;
}

/// 阻塞运行采集循环，返回退出码。
/// `window` 为窗口 UUID（如 `{uuid}`）；None 表示抓整个输出。
pub fn run(pointer: u32, window: Option<&str>) -> i32 {
    let c_string = window.and_then(|w| CString::new(w).ok());
    let ptr = c_string
        .as_ref()
        .map(|s| s.as_ptr())
        .unwrap_or(std::ptr::null());
    unsafe { lan_video_kwin_capture(ptr, pointer as i32) }
}

#[derive(Serialize, Clone, Debug)]
pub struct WindowInfo {
    /// KRunner 返回的完整 id（`0_{uuid}`）。
    pub id: String,
    /// 传给 `capture.window` 的 UUID（`{uuid}`）。
    pub uuid: String,
    pub title: String,
    /// 图标名/应用标识。
    pub app: String,
    pub backend: &'static str,
}

/// 通过 KWin 的 KRunner Window plugin 枚举窗口。
pub fn list_windows() -> Result<Vec<WindowInfo>> {
    let output = Command::new("busctl")
        .args([
            "--user",
            "call",
            "org.kde.KWin",
            "/WindowsRunner",
            "org.kde.krunner1",
            "Match",
            "s",
            "",
        ])
        .output()
        .context("运行 busctl 失败（需要 KDE/KWin）")?;
    if !output.status.success() {
        bail!("KWin WindowsRunner 不可用（需要 KDE Plasma 6）");
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let quoted = collect_quoted(&text);
    let mut windows = Vec::new();
    let mut i = 0;
    while i + 2 < quoted.len() {
        let id = &quoted[i];
        if id.starts_with("0_{") && id.ends_with('}') {
            windows.push(WindowInfo {
                id: id.clone(),
                uuid: id.trim_start_matches("0_").to_string(),
                title: quoted[i + 1].clone(),
                app: quoted[i + 2].clone(),
                backend: "kwin",
            });
            i += 3;
        } else {
            i += 1;
        }
    }
    Ok(windows)
}

/// 提取 busctl 文本输出中的所有双引号字符串。
///
/// busctl 会像 C 字符串一样转义：`\"`、`\\`、`\n` 以及 UTF-8 的八进制字节
/// （如 `\342\200\224`），这里按字节解码后再转 UTF-8。
fn collect_quoted(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    let mut in_quotes = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_quotes {
            if b == b'\\' {
                if i + 1 < bytes.len() {
                    let next = bytes[i + 1];
                    if (b'0'..=b'7').contains(&next) {
                        let mut value = 0u32;
                        let mut j = i + 1;
                        let mut count = 0;
                        while j < bytes.len() && count < 3 && (b'0'..=b'7').contains(&bytes[j]) {
                            value = value * 8 + (bytes[j] - b'0') as u32;
                            j += 1;
                            count += 1;
                        }
                        current.push(value as u8);
                        i = j;
                        continue;
                    }
                    match next {
                        b'n' => current.push(b'\n'),
                        b't' => current.push(b'\t'),
                        b'r' => current.push(b'\r'),
                        other => current.push(other),
                    }
                    i += 2;
                    continue;
                }
                i += 1;
                continue;
            }
            if b == b'"' {
                out.push(String::from_utf8_lossy(&current).into_owned());
                current.clear();
                in_quotes = false;
            } else {
                current.push(b);
            }
        } else if b == b'"' {
            in_quotes = true;
        }
        i += 1;
    }
    out
}

pub fn permission_file_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".local/share/applications/lan-video-kwin.desktop")
}

/// 为当前可执行文件生成/更新 KWin 权限 desktop 文件，并刷新 KDE service 缓存。
pub fn ensure_permission_file() -> Result<()> {
    let exe = std::env::current_exe()
        .context("获取当前可执行文件路径")?
        .canonicalize()
        .context("规范化可执行文件路径")?;
    let path = permission_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("创建目录 {}", parent.display()))?;
    }
    let content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=lan-video KWin screencast permission\n\
         Comment=Allow lan-video to capture this KDE Wayland session\n\
         Exec={}\n\
         X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1\n\
         NoDisplay=true\n",
        exe.display()
    );
    let changed = fs::read_to_string(&path)
        .map(|current| current != content)
        .unwrap_or(true);
    if changed {
        fs::write(&path, &content).with_context(|| format!("写入 {}", path.display()))?;
        // 让 KApplicationTrader 重新扫描 desktop 文件（KWin 按可执行文件路径匹配）。
        let _ = Command::new("kbuildsycoca6")
            .arg("--noincremental")
            .status();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_krunner_windows() {
        let sample = r#"a(sssida{sv}) 2 "0_{11111111-2222-3333-4444-555555555555}" "Terminal" "utilities-terminal" 100 0.8 1 "subtext" s "窗口" "0_{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}" "Dolphin" "org.kde.dolphin" 100 0.8 1 "subtext" s "窗口""#;
        let quoted = collect_quoted(sample);
        assert!(quoted.iter().any(|s| s.starts_with("0_{")));
        // 简化校验：uuid 去掉 `0_` 前缀
        let id = "0_{11111111-2222-3333-4444-555555555555}";
        assert_eq!(
            id.trim_start_matches("0_"),
            "{11111111-2222-3333-4444-555555555555}"
        );
    }
}
