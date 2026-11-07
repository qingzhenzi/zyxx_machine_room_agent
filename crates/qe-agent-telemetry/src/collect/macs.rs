//! 网卡 MAC 采集（Wake-on-LAN 用）：`/sys/class/net/<iface>/address`。
//!
//! 契约：`sections.agent.host.macs`（`docs/machine-telemetry.md` v1.3）。
//!
//! 规则：
//! - **只保留物理网卡**：`/sys/class/net/<iface>/device`（PCI/USB 设备，含 virtio）
//!   或 `phy80211`（无线）存在才算；bridge/veth/docker0/tun 等虚拟接口发不了 WoL，
//!   混进来只会把 magic packet 发到一堆无用 MAC（实测宿主机曾上报 16 个虚拟网卡）；
//! - 跳过回环接口（`lo`/`lo0`）；
//! - 跳过全零 MAC（无地址/未初始化网卡）；
//! - 统一转小写冒号格式（`aa:bb:cc:dd:ee:ff`）；
//! - 排序 + 去重（同一物理网卡可能出现多条别名）。
//!
//! 采集是**纯读取**：不写 sysfs、不执行外部命令，容器里读不到时返回空列表
//! （容器内的 veth 没有 `device`，因此容器里通常得到空数组，这是有意的保守行为）。

use std::path::Path;

/// 系统网卡 MAC 列表（小写、排序去重）。
pub fn collect() -> Vec<String> {
    collect_from(Path::new("/sys/class/net"))
}

/// 从指定 sysfs 根目录读取 MAC，便于用临时目录做单测。
pub fn collect_from(root: &Path) -> Vec<String> {
    let mut macs = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return macs;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if is_loopback(&name) {
            continue;
        }
        let dir = entry.path();
        if !is_physical(&dir) {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(dir.join("address")) else {
            continue;
        };
        if let Some(mac) = normalize(&raw) {
            macs.push(mac);
        }
    }
    macs.sort();
    macs.dedup();
    macs
}

/// 规范化 MAC 文本：6 组 2 位十六进制，转小写冒号格式；全零/非法返回 `None`。
pub fn normalize(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let parts: Vec<&str> = trimmed.split(':').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut bytes = [0u8; 6];
    for (index, part) in parts.iter().enumerate() {
        if part.len() != 2 {
            return None;
        }
        bytes[index] = u8::from_str_radix(part, 16).ok()?;
    }
    if bytes == [0u8; 6] {
        return None;
    }
    Some(format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]
    ))
}

/// 是否回环接口（Linux 固定叫 `lo`；容器/BSD 兼容 `lo0`）。
fn is_loopback(name: &str) -> bool {
    name == "lo" || name == "lo0"
}

/// 是否物理网卡：有 `device` 符号链接（PCI/USB/virtio 设备）或 `phy80211`（无线）。
///
/// bridge/veth/docker0/tun/tailscale0 等虚拟接口两者都没有；容器里的 veth 同样没有，
/// 这样可以避免把 WOL 包发给虚拟网卡的随机 MAC。
fn is_physical(iface_dir: &Path) -> bool {
    iface_dir.join("device").exists() || iface_dir.join("phy80211").exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_net_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "qe-macs-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join("net")).unwrap();
        dir
    }

    fn write_iface(root: &Path, name: &str, address: &str) {
        let dir = root.join("net").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("address"), address).unwrap();
    }

    /// 标记为物理网卡（建 `device` 目录，模拟 /sys 下的设备符号链接）。
    fn make_physical(root: &Path, name: &str) {
        fs::create_dir_all(root.join("net").join(name).join("device")).unwrap();
    }

    /// 标记为无线物理网卡（`phy80211`）。
    fn make_wireless(root: &Path, name: &str) {
        fs::create_dir_all(root.join("net").join(name).join("phy80211")).unwrap();
    }

    #[test]
    fn normalizes_uppercase_and_pads() {
        assert_eq!(
            normalize("AA:BB:CC:DD:EE:FF\n").as_deref(),
            Some("aa:bb:cc:dd:ee:ff")
        );
        assert_eq!(
            normalize(" 00:1a:2b:3c:4d:5e ").as_deref(),
            Some("00:1a:2b:3c:4d:5e")
        );
    }

    #[test]
    fn rejects_all_zero_and_malformed() {
        assert_eq!(normalize("00:00:00:00:00:00"), None);
        assert_eq!(normalize(""), None);
        assert_eq!(normalize("aa:bb:cc:dd:ee"), None);
        assert_eq!(normalize("aa:bb:cc:dd:ee:ff:00"), None);
        assert_eq!(normalize("zz:bb:cc:dd:ee:ff"), None);
        assert_eq!(normalize("a:bb:cc:dd:ee:ff"), None);
        assert_eq!(normalize("-1:bb:cc:dd:ee:ff"), None);
    }

    #[test]
    fn skips_loopback_and_all_zero_and_sorts_dedups() {
        let root = temp_net_root("collect");
        write_iface(&root, "lo", "00:00:00:00:00:00\n");
        write_iface(&root, "enp3s0", "AA:BB:CC:DD:EE:02\n");
        make_physical(&root, "enp3s0");
        write_iface(&root, "wlan0", "aa:bb:cc:dd:ee:01\n");
        make_wireless(&root, "wlan0");
        // 同一物理网卡的别名共享 MAC：去重。
        write_iface(&root, "enp3s0:1", "aa:bb:cc:dd:ee:02\n");
        make_physical(&root, "enp3s0:1");
        // 未初始化网卡：全零地址，跳过。
        write_iface(&root, "tun0", "00:00:00:00:00:00\n");
        // 坏地址：跳过。
        write_iface(&root, "veth1", "not-a-mac\n");
        // 没有 address 文件的接口：跳过。
        fs::create_dir_all(root.join("net").join("dummy0")).unwrap();

        assert_eq!(
            collect_from(&root.join("net")),
            vec![
                "aa:bb:cc:dd:ee:01".to_owned(),
                "aa:bb:cc:dd:ee:02".to_owned()
            ]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn skips_virtual_interfaces_with_valid_macs() {
        let root = temp_net_root("virtual");
        // 虚拟接口即使 MAC 合法也不能作为 WoL 目标。
        for name in ["docker0", "br-04cdd03bc656", "incusbr0", "veth024b956", "tailscale0"] {
            write_iface(&root, name, "aa:bb:cc:dd:ee:0a\n");
        }
        // 唯一物理网卡。
        write_iface(&root, "wlan0", "BC:2B:02:10:91:E7\n");
        make_physical(&root, "wlan0");

        assert_eq!(
            collect_from(&root.join("net")),
            vec!["bc:2b:02:10:91:e7".to_owned()]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_sysfs_root_returns_empty() {
        assert!(collect_from(Path::new("/definitely/not/a/sysfs/root")).is_empty());
    }

    #[test]
    fn loopback_detection_covers_linux_and_bsd_names() {
        assert!(is_loopback("lo"));
        assert!(is_loopback("lo0"));
        assert!(!is_loopback("eth0"));
        assert!(!is_loopback("enp3s0"));
    }
}
