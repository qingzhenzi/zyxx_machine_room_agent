//! qep2p — 内网优先的 P2P 文件传输库。
//!
//! 传输与身份由 [iroh] 提供（Ed25519 公钥即节点 ID，QUIC + TLS），
//! 文件内容用 [iroh_blobs] 做 BLAKE3 校验流与断点续传；
//! 本 crate 只负责节点生命周期、mDNS 发现、自建 relay 配置，
//! 以及一层很薄的「推送」控制协议。

pub mod node;

pub use node::{
    DownloadOutcome, Event, Node, NodeConfig, PingOutcome, RelayChoice, SendOutcome, SharedBlob,
    load_or_create_identity, parse_ticket,
};

/// 控制协议的版本，会随每个请求发送。
pub const PROTOCOL_VERSION: u32 = 1;

/// 控制协议 ALPN，和文件传输的 `iroh_blobs::ALPN` 区分开。
pub const CONTROL_ALPN: &[u8] = b"qep2p/control/1";

/// 取主机名，读不到就退回 `qep2p`。
pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "qep2p".to_string())
}

/// 把不可信文本转成可安全打印的形式：控制字符转义（保留换行和制表符），超长截断。
pub fn printable(text: &str, max: usize) -> String {
    let mut out = String::new();
    for (i, c) in text.chars().enumerate() {
        if i >= max {
            out.push('…');
            break;
        }
        match c {
            '\n' | '\t' => out.push(c),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// 节点 ID 的短形式，便于日志展示。
pub fn short_id(id: &iroh::EndpointId) -> String {
    id.to_string().chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_escapes_control_chars() {
        assert_eq!(printable("ok\nline\tend", 100), "ok\nline\tend");
        assert_eq!(printable("\u{1b}[31mred", 100), "\\u{1b}[31mred");
        assert_eq!(printable("abcdef", 3), "abc…");
    }
}
