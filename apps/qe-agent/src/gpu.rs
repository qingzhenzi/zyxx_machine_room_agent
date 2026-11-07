//! `qe-agent cli gpu list`：枚举宿主机 GPU / PCI 显示设备，辅助 GPU 直通配置。
//!
//! 直通前需要确认三件事：设备是显示类（class `0x03xxxx`）、已绑定 `vfio-pci`、
//! 且所在 IOMMU 组已隔离。这里只做只读枚举，不修改任何 sysfs 状态。
//!
//! 扫描函数接受可注入的 sysfs 根目录，单测用临时目录构造假设备树。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// 默认 PCI 设备 sysfs 根目录。
pub const DEFAULT_SYSFS_ROOT: &str = "/sys/bus/pci/devices";

/// 一个显示类 PCI 设备（通常就是 GPU）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuDevice {
    /// PCI 地址（`dddd:bb:dd.f`）。
    pub pci_address: String,
    /// PCI class，例如 `0x030000`（VGA）或 `0x038000`（display controller）。
    pub class: u32,
    /// vendor id（4 位小写 hex，无 `0x` 前缀）。
    pub vendor_id: String,
    /// device id（4 位小写 hex，无 `0x` 前缀）。
    pub device_id: String,
    /// 当前绑定驱动（例如 `vfio-pci` / `amdgpu` / `i915`）。
    pub driver: Option<String>,
    /// IOMMU 组编号（`/sys/bus/pci/devices/<addr>/iommu_group` 的链接名）。
    pub iommu_group: Option<String>,
}

impl GpuDevice {
    /// 是否可直接用于 PCI 直通（已绑定 vfio-pci）。
    pub fn ready_for_passthrough(&self) -> bool {
        self.driver.as_deref() == Some("vfio-pci")
    }
}

/// class 的高字节是否为 `0x03`（显示控制器大类）。
pub fn is_display_class(class: u32) -> bool {
    class >> 16 == 0x03
}

/// 解析 sysfs 里的十六进制值（`0x030000` 或 `030000`）。
pub fn parse_hex_id(raw: &str) -> Option<u32> {
    let trimmed = raw.trim();
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    if hex.is_empty() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

/// 读一个 sysfs 十六进制属性；文件不存在返回 `None`。
fn read_hex(path: &Path, label: &str) -> Result<Option<u32>> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(Some(parse_hex_id(&raw).with_context(|| {
            format!("{label} 不是十六进制: {raw:?}（{}）", path.display())
        })?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("读取 {label} 失败: {}", path.display())),
    }
}

fn read_link_name(path: &Path) -> Option<String> {
    std::fs::read_link(path)
        .ok()
        .and_then(|target| target.file_name().map(|name| name.to_string_lossy().into_owned()))
}

/// 读取单个设备目录；非显示类设备返回 `None`。
pub fn read_device(device_dir: &Path) -> Result<Option<GpuDevice>> {
    let Some(pci_address) = device_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
    else {
        return Ok(None);
    };
    let Some(class) = read_hex(&device_dir.join("class"), "class")? else {
        return Ok(None);
    };
    if !is_display_class(class) {
        return Ok(None);
    }
    let vendor_id = read_hex(&device_dir.join("vendor"), "vendor")?
        .map(|vendor| format!("{vendor:04x}"))
        .unwrap_or_else(|| "????".to_owned());
    let device_id = read_hex(&device_dir.join("device"), "device")?
        .map(|device| format!("{device:04x}"))
        .unwrap_or_else(|| "????".to_owned());
    Ok(Some(GpuDevice {
        pci_address,
        class,
        vendor_id,
        device_id,
        driver: read_link_name(&device_dir.join("driver")),
        iommu_group: read_link_name(&device_dir.join("iommu_group")),
    }))
}

/// 扫描 sysfs 根目录下的显示类设备（按 PCI 地址排序）。
pub fn scan(sysfs_root: &Path) -> Result<Vec<GpuDevice>> {
    let entries = std::fs::read_dir(sysfs_root)
        .with_context(|| format!("读取 sysfs 失败: {}", sysfs_root.display()))?;
    let mut devices = Vec::new();
    for entry in entries {
        let entry = entry?;
        // 注意：`/sys/bus/pci/devices/<addr>` 是指向 `/sys/devices/pci...` 的符号链接，
        // 必须用 path().is_dir()（跟随链接）而不是 file_type().is_dir()（不跟随）。
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // 只认合法 PCI 地址目录，跳过 `drivers` 之类的杂项。
        if virt_core::PciAddress::parse(&name).is_err() {
            continue;
        }
        if let Some(device) = read_device(&entry.path())? {
            devices.push(device);
        }
    }
    devices.sort_by(|left, right| left.pci_address.cmp(&right.pci_address));
    Ok(devices)
}

/// `qe-agent cli gpu list` 的实现：打印宿主机显示设备与直通就绪状态。
pub fn list() -> Result<()> {
    let root = PathBuf::from(DEFAULT_SYSFS_ROOT);
    if !root.exists() {
        anyhow::bail!(
            "{} 不存在：非 Linux 或 sysfs 未挂载，无法枚举 PCI 设备",
            root.display()
        );
    }
    let devices = scan(&root)?;
    print_table(&devices);
    Ok(())
}

fn print_table(devices: &[GpuDevice]) {
    if devices.is_empty() {
        println!("未发现显示类 PCI 设备（class 0x03xxxx）。");
        return;
    }
    println!(
        "{:<16} {:<9} {:<12} {:<12} {:<10} 直通",
        "PCI 地址", "class", "厂商:设备", "驱动", "IOMMU 组"
    );
    for device in devices {
        println!(
            "{:<16} 0x{:06x} {:<12} {:<12} {:<10} {}",
            device.pci_address,
            device.class,
            format!("{}:{}", device.vendor_id, device.device_id),
            device.driver.as_deref().unwrap_or("-"),
            device.iommu_group.as_deref().unwrap_or("-"),
            if device.ready_for_passthrough() {
                "可用"
            } else {
                "需绑定 vfio-pci"
            }
        );
    }
    println!();
    println!("提示：直通前需开启 IOMMU、把设备绑定 vfio-pci 并确认 IOMMU 组隔离；");
    println!("     详见 room_agent/docs/gpu-passthrough.md，创建 VM 时用 `qe-agent vm create --gpu <PCI>`。");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn write(path: &Path, value: &str) {
        std::fs::write(path, format!("{value}\n")).unwrap();
    }

    fn fake_device(root: &Path, address: &str, class: &str, vendor: &str, device: &str) -> PathBuf {
        let dir = root.join(address);
        std::fs::create_dir_all(&dir).unwrap();
        write(&dir.join("class"), class);
        write(&dir.join("vendor"), vendor);
        write(&dir.join("device"), device);
        dir
    }

    #[test]
    fn class_and_hex_parsing() {
        assert!(is_display_class(0x0300_00));
        assert!(is_display_class(0x0380_00));
        assert!(!is_display_class(0x0200_00)); // 网卡
        assert!(!is_display_class(0x0400_00)); // 多媒体

        assert_eq!(parse_hex_id("0x030000\n"), Some(0x030000));
        assert_eq!(parse_hex_id("030000"), Some(0x030000));
        assert_eq!(parse_hex_id("0X10DE"), Some(0x10de));
        assert_eq!(parse_hex_id(""), None);
        assert_eq!(parse_hex_id("0xzz"), None);
    }

    #[test]
    fn scan_filters_display_devices_and_reads_links() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        let gpu = fake_device(
            root,
            "0000:01:00.0",
            "0x030000",
            "0x10de",
            "0x2684",
        );
        // 已绑定 vfio-pci 的外置 GPU
        let drivers = root.join("drivers");
        std::fs::create_dir_all(&drivers).unwrap();
        let vfio = drivers.join("vfio-pci");
        std::fs::create_dir_all(&vfio).unwrap();
        symlink(&vfio, gpu.join("driver")).unwrap();
        // 模拟 iommu_group 链接名
        let groups = root.join("iommu_groups").join("12");
        std::fs::create_dir_all(&groups).unwrap();
        symlink(&groups, gpu.join("iommu_group")).unwrap();

        // 核显（未绑定 vfio）
        fake_device(root, "0000:00:02.0", "0x030000", "0x8086", "0x46a6");
        // 非显示设备（网卡）应被过滤
        fake_device(root, "0000:00:1f.6", "0x020000", "0x8086", "0x15f9");

        let devices = scan(root).unwrap();
        assert_eq!(devices.len(), 2, "{devices:?}");
        assert_eq!(devices[0].pci_address, "0000:00:02.0");
        assert_eq!(devices[0].vendor_id, "8086");
        assert_eq!(devices[0].driver, None);
        assert!(!devices[0].ready_for_passthrough());

        assert_eq!(devices[1].pci_address, "0000:01:00.0");
        assert_eq!(devices[1].vendor_id, "10de");
        assert_eq!(devices[1].device_id, "2684");
        assert_eq!(devices[1].driver.as_deref(), Some("vfio-pci"));
        assert_eq!(devices[1].iommu_group.as_deref(), Some("12"));
        assert!(devices[1].ready_for_passthrough());
    }

    #[test]
    fn scan_follows_symlinked_device_entries() {
        // 真实 sysfs 的 /sys/bus/pci/devices/<addr> 是符号链接，不能因为不跟随而漏掉。
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bus");
        std::fs::create_dir_all(&root).unwrap();
        let real = temp.path().join("devices").join("0000:03:00.0");
        std::fs::create_dir_all(&real).unwrap();
        write(&real.join("class"), "0x030000");
        write(&real.join("vendor"), "0x1002");
        write(&real.join("device"), "0x1681");
        symlink(&real, root.join("0000:03:00.0")).unwrap();

        let devices = scan(&root).unwrap();
        assert_eq!(devices.len(), 1, "{devices:?}");
        assert_eq!(devices[0].pci_address, "0000:03:00.0");
        assert_eq!(devices[0].vendor_id, "1002");
        assert_eq!(devices[0].device_id, "1681");
    }

    #[test]
    fn scan_skips_entries_without_class() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let dir = root.join("0000:05:00.0");
        std::fs::create_dir_all(&dir).unwrap();
        // 没有 class 文件 → 不是完整设备目录
        std::fs::write(dir.join("vendor"), "0x1234\n").unwrap();
        assert!(scan(root).unwrap().is_empty());
    }

    #[test]
    fn missing_sysfs_root_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("no-such-sysfs");
        assert!(scan(&missing).is_err());
    }
}
