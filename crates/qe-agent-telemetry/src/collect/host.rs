//! 主机清单：hostname / OS / kernel / arch / uptime / machine_id / 网卡 MAC。

use std::fs;

use super::macs;

/// 内置采集到的主机信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInfo {
    /// 主机名（必填字段，读不到时退化为 `unknown`）。
    pub hostname: String,
    /// 发行版展示名（`/etc/os-release` 的 `PRETTY_NAME`，回退 `NAME`）。
    pub os_name: Option<String>,
    /// 内核版本（`/proc/sys/kernel/osrelease`）。
    pub kernel_version: Option<String>,
    /// CPU 架构（编译期常量，如 `x86_64`）。
    pub arch: String,
    /// 开机时长（秒）。
    pub uptime_secs: Option<u64>,
    /// 网卡 MAC（小写、排序去重；Wake-on-LAN 目标，读不到时为空）。
    pub macs: Vec<String>,
}

/// 从本机 `/proc`、`/etc/os-release`、`/sys/class/net` 采集主机信息。
pub fn collect() -> HostInfo {
    HostInfo {
        hostname: read_trimmed("/proc/sys/kernel/hostname")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "unknown".to_owned()),
        os_name: fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|content| parse_os_release(&content)),
        kernel_version: read_trimmed("/proc/sys/kernel/osrelease")
            .filter(|value| !value.is_empty()),
        arch: std::env::consts::ARCH.to_owned(),
        uptime_secs: fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|content| parse_uptime(&content)),
        macs: macs::collect(),
    }
}

/// 解析 `/etc/os-release`：优先 `PRETTY_NAME`，其次 `NAME`。
pub fn parse_os_release(content: &str) -> Option<String> {
    let mut pretty = None;
    let mut name = None;
    for line in content.lines() {
        let Some((key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let value = unquote(raw_value.trim());
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "PRETTY_NAME" => pretty = Some(value),
            "NAME" => name = Some(value),
            _ => {}
        }
    }
    pretty.or(name)
}

/// 解析 `/proc/uptime` 第一列（秒，带小数）。
pub fn parse_uptime(content: &str) -> Option<u64> {
    let seconds = content.split_whitespace().next()?.parse::<f64>().ok()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Some(seconds as u64)
}

/// 读取并 trim 一个文本文件。
pub(crate) fn read_trimmed(path: &str) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|content| content.trim().to_owned())
}

/// 计算 `machine_id`：优先 `/etc/machine-id`，缺失时用 hostname 的 FNV-1a 哈希。
pub fn machine_id_for(hostname: &str) -> String {
    let from_file = read_trimmed("/etc/machine-id");
    machine_id_from(from_file.as_deref(), hostname)
}

/// `machine_id` 的纯函数版本，便于测试。
pub fn machine_id_from(machine_id: Option<&str>, hostname: &str) -> String {
    if let Some(id) = machine_id.map(str::trim).filter(|id| !id.is_empty()) {
        return id.to_owned();
    }
    let normalized = hostname.trim().to_ascii_lowercase();
    format!("host-{:016x}", fnv1a64(normalized.as_bytes()))
}

/// FNV-1a 64 位哈希（std-only，跨进程稳定）。
fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if let Some(inner) = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        inner.replace("\\\"", "\"").replace("\\\\", "\\")
    } else if let Some(inner) = trimmed
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        inner.to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release_prefers_pretty_name() {
        let content = "NAME=\"CachyOS Linux\"\nPRETTY_NAME=\"CachyOS Linux rolling\"\nID=cachyos\n";
        assert_eq!(
            parse_os_release(content).as_deref(),
            Some("CachyOS Linux rolling")
        );
    }

    #[test]
    fn os_release_falls_back_to_name_and_handles_quotes() {
        assert_eq!(
            parse_os_release("NAME='Debian GNU/Linux'\n").as_deref(),
            Some("Debian GNU/Linux")
        );
        assert_eq!(parse_os_release("ID=debian\n").as_deref(), None);
    }

    #[test]
    fn uptime_is_truncated_to_seconds() {
        assert_eq!(parse_uptime("3421.42 1234.00\n"), Some(3421));
        assert_eq!(parse_uptime("0.9 0.5"), Some(0));
        assert_eq!(parse_uptime("garbage"), None);
        assert_eq!(parse_uptime(""), None);
    }

    #[test]
    fn machine_id_prefers_machine_id_file_value() {
        assert_eq!(
            machine_id_from(Some("  d69484312530414fadc4bc54e86d4705\n"), "host-a"),
            "d69484312530414fadc4bc54e86d4705"
        );
    }

    #[test]
    fn machine_id_fallback_is_stable_and_host_specific() {
        let first = machine_id_from(None, "room-101-node");
        let second = machine_id_from(Some("   "), "room-101-node");
        let other = machine_id_from(None, "room-102-node");
        assert_eq!(first, second);
        assert_ne!(first, other);
        assert!(first.starts_with("host-"));
        assert_eq!(first.len(), "host-".len() + 16);
    }

    #[test]
    fn hostname_hash_is_case_insensitive() {
        assert_eq!(
            machine_id_from(None, "Room-101"),
            machine_id_from(None, "room-101")
        );
    }
}
