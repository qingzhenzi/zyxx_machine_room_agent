//! lan_video：局域网低延迟视频传输（GStreamer + RTP/UDP）。
//!
//! 设计：
//! - 媒体面：GStreamer 管线负责采集 / 硬件编码 / RTP 打包；
//! - 控制面：每路会话一个独立 `gst-launch-1.0` 进程，可由管理 API 强杀；
//! - 传输：RTP over UDP（LAN 最低延迟），后续再加 WebRTC 适配层。

pub mod api;
pub mod devices;
pub mod input;
#[cfg(target_os = "linux")]
pub mod kwin;
pub mod pipeline;
pub mod probe;
pub mod session;
#[cfg(all(target_os = "linux", feature = "viewer"))]
pub mod viewer;
#[cfg(target_os = "linux")]
pub mod x11;
pub mod windows;

use serde::Serialize;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// 在 PATH 中查找可执行文件。
pub fn which(bin: &str) -> Option<String> {
    if bin.contains('/') {
        return std::path::Path::new(bin).is_file().then(|| bin.to_string());
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

/// 当前环境是否有某个 GStreamer 元素。
pub fn gst_available(element: &str) -> bool {
    Command::new("gst-inspect-1.0")
        .arg(element)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn gst_version() -> Option<String> {
    let out = Command::new("gst-inspect-1.0")
        .arg("--version")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next().map(|l| l.trim().to_string())
}

/// 所有全局 IPv4（优先 `ip` 命令，回退到 UDP connect 技巧）。
pub fn local_ipv4s() -> Vec<String> {
    if let Ok(out) = Command::new("ip")
        .args(["-o", "-4", "addr", "show", "scope", "global"])
        .output()
    {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            let ips: Vec<String> = text
                .lines()
                .filter_map(|line| {
                    let mut it = line.split_whitespace();
                    while let Some(tok) = it.next() {
                        if tok == "inet" {
                            return it
                                .next()
                                .map(|cidr| cidr.split('/').next().unwrap_or(cidr).to_string());
                        }
                    }
                    None
                })
                .collect();
            if !ips.is_empty() {
                return ips;
            }
        }
    }
    primary_ipv4().into_iter().collect()
}

/// 默认出口 IP（不会真的发包）。
pub fn primary_ipv4() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    sock.local_addr()
        .ok()
        .map(|addr| addr.ip().to_string())
        .filter(|ip| ip != "0.0.0.0")
}

pub fn home_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        // USERPROFILE 比 HOME 可靠；再退到 APPDATA
        if let Some(dir) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(dir);
        }
        if let Some(dir) = std::env::var_os("APPDATA") {
            return PathBuf::from(dir);
        }
        PathBuf::from("C:\\\\Users\\\\Public")
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
    }
}

pub fn state_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        home_dir().join("AppData/Local/lan_video")
    }
    #[cfg(not(target_os = "windows"))]
    {
        home_dir().join(".local/state/lan_video")
    }
}

/// 会话控制 token（/dev/urandom；非 Unix 回退到时间+pid）。
pub fn random_token() -> String {
    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
            let mut buf = [0u8; 16];
            if file.read_exact(&mut buf).is_ok() {
                return buf.iter().map(|b| format!("{b:02x}")).collect();
            }
        }
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}{:x}", nanos, std::process::id())
}

/// 把 `a ! b caps="x,y"` 拆成 gst-launch 能直接吃的 argv。
///
/// 注意：gst-launch 不接受“整条管线作为一个参数”（会报语法错误），
/// 必须拆成独立 token；这里同时处理 caps 里的双引号。
pub fn tokenize_pipeline(pipeline: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escaped = false;
    for ch in pipeline.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_pipeline_and_strips_caps_quotes() {
        let tokens =
            tokenize_pipeline("udpsrc port=5000 caps=\"application/x-rtp,media=video\" ! fakesink");
        assert_eq!(
            tokens,
            vec![
                "udpsrc",
                "port=5000",
                "caps=application/x-rtp,media=video",
                "!",
                "fakesink"
            ]
        );
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct Capabilities {
    pub platform: &'static str,
    pub gst_version: Option<String>,
    pub encoders: Vec<String>,
    pub capture_sources: Vec<String>,
    pub vaapi: bool,
    pub nvenc: bool,
    pub ips: Vec<String>,
    pub recommended: &'static str,
}

pub fn capabilities() -> Capabilities {
    let encoder_candidates = [
        "vah264enc",
        "vah265enc",
        "nvh264enc",
        "nvh265enc",
        "mfh264enc",
        "qsvh264enc",
        "x264enc",
    ];
    let encoders: Vec<String> = encoder_candidates
        .iter()
        .filter(|e| gst_available(e))
        .map(|e| (*e).to_string())
        .collect();

    let capture_candidates = [
        "pipewiresrc",
        "ximagesrc",
        // VNC 采集：连 QEMU/libvirt 的图形控制台，无头虚拟机也能采。
        "rfbsrc",
        "d3d11screencapturesrc",
        "dxgiscreencapsrc",
        "gdiscreencapsrc",
    ];
    let capture_sources: Vec<String> = capture_candidates
        .iter()
        .filter(|e| gst_available(e))
        .map(|e| (*e).to_string())
        .collect();

    let vaapi = gst_available("vah264enc");
    let nvenc = gst_available("nvh264enc");
    let recommended = if vaapi {
        "vah264enc (VAAPI, 零拷贝优先)"
    } else if nvenc {
        "nvh264enc (NVENC)"
    } else {
        "x264enc (软编，仅兜底)"
    };

    Capabilities {
        platform: std::env::consts::OS,
        gst_version: gst_version(),
        encoders,
        capture_sources,
        vaapi,
        nvenc,
        ips: local_ipv4s(),
        recommended,
    }
}
