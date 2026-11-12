//! 设备功能与设备状态。
//!
//! 「设备功能」= 驱动绑定、必需模块、设备类别齐备；
//! 「设备状态」= 失败单元、内核污染（taint）、温度、块设备在线状态。

use crate::check::{Check, Ctx};
use crate::config::DeviceConfig;
use crate::model::{Finding, Severity, Status};
use crate::util;
use std::time::Duration;

pub struct DeviceCheck;

impl Check for DeviceCheck {
    fn id(&self) -> &'static str {
        "device"
    }

    fn title(&self) -> &'static str {
        "设备功能与运行状态"
    }

    fn run(&self, cx: &Ctx<'_>) -> Vec<Finding> {
        let cfg = &cx.cfg.device;
        vec![
            check_modules(cfg),
            check_classes(cfg),
            check_pci_drivers(cfg),
            check_failed_units(cfg),
            check_taint(),
            check_thermal(cfg),
            check_block_state(),
        ]
    }
}

fn check_modules(cfg: &DeviceConfig) -> Finding {
    let f = Finding::new("DEV-01", "必需内核模块已加载", Severity::High);

    if cfg.required_modules.is_empty() {
        return f.skip("策略未配置 required_modules");
    }

    let mut missing = Vec::new();
    for m in &cfg.required_modules {
        // /sys/module 对内建（builtin）模块同样存在
        if !util::is_dir(format!("/sys/module/{m}")) {
            missing.push(m);
        }
    }

    if missing.is_empty() {
        f.pass(format!("{} 个模块均在位", cfg.required_modules.len()))
    } else {
        f.fail("缺少必需模块，相关设备功能不可用")
            .evidence_all(missing.iter().map(|m| format!("missing module: {m}")))
            .fix("modprobe <module>，并写入 /etc/modules-load.d/<name>.conf 持久化")
    }
}

fn check_classes(cfg: &DeviceConfig) -> Finding {
    let f = Finding::new("DEV-02", "设备类别齐备", Severity::Medium);

    if cfg.required_classes.is_empty() {
        return f.skip("策略未配置 required_classes");
    }

    let mut evidence = Vec::new();
    let mut missing = Vec::new();

    for class in &cfg.required_classes {
        let dir = format!("/sys/class/{class}");
        if !util::is_dir(&dir) {
            missing.push(class);
            evidence.push(format!("{class}: /sys/class/{class} 不存在"));
            continue;
        }
        let n = util::count_dir(&dir);
        evidence.push(format!("{class}: {n} 个设备"));
        if n == 0 {
            missing.push(class);
        }
    }

    if missing.is_empty() {
        f.pass("必需设备类别均有实例").evidence_all(evidence)
    } else {
        f.fail("设备类别缺失或为空，硬件可能未识别")
            .evidence_all(evidence)
            .fix("dmesg | grep -i -E 'error|fail'，检查固件与驱动")
    }
}

/// 关键 PCI 设备（网卡/无线/NVMe/显卡）必须绑定驱动，否则「设备功能」实为降级。
fn check_pci_drivers(cfg: &DeviceConfig) -> Finding {
    let f = Finding::new("DEV-03", "关键 PCI 设备已绑定驱动", Severity::High);

    if cfg.critical_pci_classes.is_empty() {
        return f.skip("策略未配置 critical_pci_classes");
    }

    let mut checked = 0usize;
    let mut unbound = Vec::new();

    for dev in util::list_dir("/sys/bus/pci/devices") {
        let base = format!("/sys/bus/pci/devices/{dev}");
        let Some(class) = util::read_trim(format!("{base}/class")) else {
            continue;
        };
        // 形如 0x020000，取前 4 位做类别匹配
        let code = class.trim_start_matches("0x").to_lowercase();
        let code4 = &code[..code.len().min(4)];
        if !cfg
            .critical_pci_classes
            .iter()
            .any(|c| c.to_lowercase() == code4)
        {
            continue;
        }
        checked += 1;
        let has_driver = util::exists(format!("{base}/driver"));
        if !has_driver {
            let slot = util::read_trim(format!("{base}/uevent"))
                .unwrap_or_default()
                .lines()
                .find_map(|l| l.strip_prefix("PCI_SLOT_NAME=").map(str::to_string))
                .unwrap_or_else(|| dev.clone());
            unbound.push(format!("{slot} (class {class}) 无驱动绑定"));
        }
    }

    if checked == 0 {
        return f.skip("未找到关键类别 PCI 设备");
    }
    if unbound.is_empty() {
        f.pass(format!("{checked} 个关键 PCI 设备均已绑定驱动"))
    } else {
        f.fail(format!(
            "{checked} 个关键设备中 {} 个未绑定驱动",
            unbound.len()
        ))
        .evidence_all(unbound)
        .fix("lspci -k -s <slot> 确认驱动；检查 initramfs 是否缺少对应模块")
    }
}

fn check_failed_units(cfg: &DeviceConfig) -> Finding {
    let f = Finding::new("DEV-04", "无失败的 systemd 单元", Severity::High);

    let Some(out) = util::run(
        "systemctl",
        &["--failed", "--no-legend", "--plain", "--no-pager"],
        Duration::from_secs(10),
    ) else {
        return f.skip("systemctl 不可用（容器或非 systemd 系统）");
    };
    if !out.ok() {
        return f.skip(format!("systemctl 返回非零: {}", out.stderr.trim()));
    }

    let failed: Vec<&str> = out
        .stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();

    if failed.is_empty() {
        f.pass("没有失败单元")
    } else if cfg.fail_on_failed_units {
        f.fail(format!("{} 个单元处于 failed", failed.len()))
            .evidence_all(failed.iter().take(20).map(|l| l.to_string()))
            .fix("systemctl status <unit>；systemctl reset-failed <unit> 后修复根因")
    } else {
        f.warn(format!("{} 个失败单元（策略不强制）", failed.len()))
            .evidence_all(failed.iter().take(20).map(|l| l.to_string()))
    }
}

fn check_taint() -> Finding {
    let f =
        Finding::new("DEV-05", "内核未被污染（taint）", Severity::Medium).reference("CIS 1.5.2");

    let Some(v) = util::sysctl_i64("kernel.tainted") else {
        return f.skip("无法读取 /proc/sys/kernel/tainted");
    };
    if v == 0 {
        return f.pass("tainted = 0");
    }

    let flags = taint_flags(v as u64);
    // 6=D(die/OOPS), 4=B(bad page) 属于硬故障
    let hard = v & (1 << 4) != 0 || v & (1 << 6) != 0;
    let detail = format!("tainted = {v} ({})", flags.join(", "));

    if hard {
        f.fail("内核出现 OOPS/坏页，稳定性与完整性存疑")
            .evidence(detail)
            .fix("journalctl -k -p err -b -1 定位崩溃点；必要时重启并收集 vmcore")
    } else {
        f.warn("内核带 taint 标记").evidence(detail)
    }
}

fn taint_flags(v: u64) -> Vec<&'static str> {
    const TABLE: &[(u32, &str)] = &[
        (0, "P: 专有模块"),
        (1, "F: 强制加载/卸载模块"),
        (2, "S: 非 SMP 安全"),
        (3, "M: 机器检查异常"),
        (4, "B: 坏页"),
        (5, "U: 用户请求"),
        (6, "D: OOPS/内核崩溃"),
        (7, "A: ACPI 被覆盖"),
        (8, "W: 警告"),
        (9, "C: staging 驱动"),
        (10, "I: 固件规避"),
        (11, "O: 树外模块"),
        (12, "E: 未签名模块"),
        (13, "L: 软锁死"),
        (14, "K: livepatch"),
        (15, "X: aux taint"),
        (16, "T: 结构随机化?"),
    ];
    TABLE
        .iter()
        .filter(|(bit, _)| v & (1u64 << bit) != 0)
        .map(|(_, name)| *name)
        .collect()
}

fn check_thermal(cfg: &DeviceConfig) -> Finding {
    let f = Finding::new("DEV-06", "设备温度在安全范围", Severity::Medium);

    if cfg.max_thermal_c <= 0 {
        return f.skip("策略未启用温度检查");
    }

    let mut readings = Vec::new();
    let mut hottest = i64::MIN;

    for zone in util::list_dir("/sys/class/thermal") {
        if !zone.starts_with("thermal_zone") {
            continue;
        }
        let base = format!("/sys/class/thermal/{zone}");
        let Some(raw) = util::read_trim(format!("{base}/temp")) else {
            continue;
        };
        let Ok(milli) = raw.parse::<i64>() else {
            continue;
        };
        let c = milli / 1000;
        let kind = util::read_trim(format!("{base}/type")).unwrap_or_else(|| zone.clone());
        hottest = hottest.max(c);
        readings.push(format!("{kind} = {c}°C"));
    }

    if readings.is_empty() {
        return f.skip("未暴露 thermal zone（虚拟机常见）");
    }
    if hottest > cfg.max_thermal_c {
        f.fail(format!(
            "最高温度 {hottest}°C 超过阈值 {}°C",
            cfg.max_thermal_c
        ))
        .evidence_all(readings)
        .fix("检查散热/风道与 CPU 调频策略")
    } else {
        f.pass(format!("最高温度 {hottest}°C"))
            .evidence_all(readings)
    }
}

fn check_block_state() -> Finding {
    let f = Finding::new("DEV-07", "块设备处于 running 状态", Severity::High);

    let mut checked = 0usize;
    let mut bad = Vec::new();

    for dev in util::list_dir("/sys/class/block") {
        let path = format!("/sys/class/block/{dev}/device/state");
        let Some(state) = util::read_trim(&path) else {
            continue;
        };
        checked += 1;
        // SCSI 用 running，NVMe 用 live，两者都表示在线
        if !matches!(state.as_str(), "running" | "live") {
            bad.push(format!("{dev}: {state}"));
        }
    }

    if checked == 0 {
        return f.skip("未找到块设备状态接口（虚拟盘/只读介质）");
    }
    if bad.is_empty() {
        f.pass(format!("{checked} 个块设备在线（running/live）"))
    } else {
        f.fail("块设备离线或隔离，数据可用性受影响")
            .evidence_all(bad)
            .fix("dmesg | grep -i -E 'nvme|ata|I/O error'；检查 SMART 与线缆")
    }
}

#[allow(dead_code)]
fn worst(a: Status, b: Status) -> Status {
    match (a, b) {
        (Status::Fail, _) | (_, Status::Fail) => Status::Fail,
        (Status::Warn, _) | (_, Status::Warn) => Status::Warn,
        _ => Status::Pass,
    }
}

#[cfg(test)]
mod tests {
    use super::taint_flags;

    #[test]
    fn taint_bits_decode_to_names() {
        let flags = taint_flags((1 << 12) | (1 << 11));
        assert_eq!(flags.len(), 2);
        assert!(flags.iter().any(|f| f.contains("未签名")));
        assert!(flags.iter().any(|f| f.contains("树外")));
    }

    #[test]
    fn clean_kernel_has_no_flags() {
        assert!(taint_flags(0).is_empty());
    }
}
