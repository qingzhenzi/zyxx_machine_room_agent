//! 虚拟化态势：我是谁（裸金属 / 哪种 hypervisor）、IOMMU 是否在、嵌套虚拟化是否被放开。
//!
//! 注意这里有两个方向，很多实现会混淆：
//! - 被检测主机**本身是虚拟机**：关注是否与资产台账一致、virtio 是否正常；
//! - 被检测主机是**宿主机**：`/dev/kvm` + 嵌套虚拟化是实实在在的提权面。

use crate::check::{Check, Ctx};
use crate::config::{VirtConfig, VirtExpect};
use crate::model::{Finding, Severity};
use crate::util;
use std::time::Duration;

pub struct VirtCheck;

impl Check for VirtCheck {
    fn id(&self) -> &'static str {
        "virt"
    }

    fn title(&self) -> &'static str {
        "虚拟化环境与 IOMMU"
    }

    fn run(&self, cx: &Ctx<'_>) -> Vec<Finding> {
        let probe = probe();
        vec![
            report_environment(&probe),
            check_expectation(&cx.cfg.virt, &probe),
            check_iommu(&cx.cfg.virt),
            check_nested(&cx.cfg.virt),
        ]
    }
}

struct Probe {
    is_vm: bool,
    kind: String,
    evidence: Vec<String>,
}

/// 多源交叉判定，单一口径都会误判（容器、云厂商定制 DMI 等）。
fn probe() -> Probe {
    let mut evidence = Vec::new();
    let mut kind: Option<String> = None;
    let mut is_vm = false;

    // 1) Xen 有明确的 sysfs 标记
    if let Some(t) = util::read_trim("/sys/hypervisor/type") {
        evidence.push(format!("/sys/hypervisor/type = {t}"));
        if t == "xen" {
            kind = Some("xen".into());
            is_vm = true;
        }
    }

    // 2) CPUID hypervisor present bit（bit 31 of ECX on leaf 1）
    let hypervisor_bit = cpuid_hypervisor_bit();
    evidence.push(format!("CPUID hypervisor bit = {hypervisor_bit}"));
    if hypervisor_bit {
        is_vm = true;
    }

    // 3) DMI 指纹
    let vendor = util::read_trim("/sys/class/dmi/id/sys_vendor").unwrap_or_default();
    let product = util::read_trim("/sys/class/dmi/id/product_name").unwrap_or_default();
    if !vendor.is_empty() || !product.is_empty() {
        evidence.push(format!("DMI vendor={vendor} product={product}"));
    }
    let dmi = format!("{vendor} {product}").to_lowercase();
    if kind.is_none() {
        kind = match () {
            _ if dmi.contains("qemu") || dmi.contains("kvm") => Some("kvm".into()),
            _ if dmi.contains("vmware") => Some("vmware".into()),
            _ if dmi.contains("virtualbox") || dmi.contains("innotek") => Some("virtualbox".into()),
            _ if dmi.contains("hyper-v")
                || dmi.contains("microsoft corporation virtual machine") =>
            {
                Some("hyperv".into())
            }
            _ if dmi.contains("bochs") => Some("bochs".into()),
            _ if dmi.contains("parallels") => Some("parallels".into()),
            _ if dmi.contains("amazon ec2") => Some("amazon".into()),
            _ if dmi.contains("google compute engine") => Some("gce".into()),
            _ => None,
        };
    }

    // 4) systemd-detect-virt 兜底（容器环境也要能区分）
    if let Some(o) = util::run("systemd-detect-virt", &[], Duration::from_secs(3)) {
        let v = o.stdout.trim().to_string();
        if !v.is_empty() {
            evidence.push(format!("systemd-detect-virt = {v}"));
            if v != "none" && kind.is_none() {
                kind = Some(v);
                is_vm = true;
            }
        }
    }

    Probe {
        is_vm,
        kind: kind.unwrap_or_else(|| "none".into()),
        evidence,
    }
}

#[cfg(target_arch = "x86_64")]
fn cpuid_hypervisor_bit() -> bool {
    // leaf 1 的 ECX[31] 是 hypervisor present bit
    let r = core::arch::x86_64::__cpuid(1);
    r.ecx & (1 << 31) != 0
}

#[cfg(not(target_arch = "x86_64"))]
fn cpuid_hypervisor_bit() -> bool {
    false
}

fn report_environment(p: &Probe) -> Finding {
    let f = Finding::new("VIRT-01", "虚拟化环境识别", Severity::Info);
    let detail = if p.is_vm {
        format!("运行于虚拟机，类型 = {}", p.kind)
    } else {
        "裸金属（未检测到 hypervisor）".to_string()
    };
    f.pass(detail).evidence_all(p.evidence.clone())
}

fn check_expectation(cfg: &VirtConfig, p: &Probe) -> Finding {
    let f = Finding::new("VIRT-02", "虚拟化环境与策略一致", Severity::High);

    match cfg.expect {
        VirtExpect::Any => f
            .skip("策略为 any，仅探测不判定")
            .evidence_all(p.evidence.clone()),
        VirtExpect::BareMetal if p.is_vm => f
            .fail(format!("期望裸金属，实际为虚拟机（{}）", p.kind))
            .evidence_all(p.evidence.clone())
            .fix("核对资产台账；若为误部署，迁移到物理机或更新策略 expect = \"any\"")
            .reference("资产基线"),
        VirtExpect::VirtualMachine if !p.is_vm => f
            .fail("期望虚拟机，实际为裸金属")
            .evidence_all(p.evidence.clone())
            .fix("确认是否漏装 agent 或策略配置错误"),
        _ => {
            let want = if cfg.expected_hypervisor.is_empty() {
                None
            } else {
                Some(cfg.expected_hypervisor.as_str())
            };
            match want {
                Some(w) if w != p.kind => f
                    .fail(format!("期望 hypervisor={w}，实际={}", p.kind))
                    .evidence_all(p.evidence.clone()),
                _ => f.pass("环境类型符合策略").evidence_all(p.evidence.clone()),
            }
        }
    }
}

fn check_iommu(cfg: &VirtConfig) -> Finding {
    let f = Finding::new("VIRT-03", "IOMMU 已启用", Severity::Medium).reference("CIS 1.4.7");

    let groups = util::count_dir("/sys/kernel/iommu_groups");
    let classes = util::count_dir("/sys/class/iommu");
    let cmdline = util::read_trim("/proc/cmdline").unwrap_or_default();
    let has_flag = ["intel_iommu=on", "amd_iommu=on", "iommu=pt"]
        .iter()
        .any(|f| cmdline.split_whitespace().any(|p| p == *f));

    let enabled = groups > 0 || classes > 0;

    if enabled {
        f.pass(format!("IOMMU 分组 {groups} 个")).evidence(format!(
            "iommu_groups={groups} class/iommu={classes} cmdline_flag={has_flag}"
        ))
    } else if cfg.require_iommu {
        f.fail("IOMMU 未启用，外设可通过 DMA 直接读写内存")
            .evidence("iommu_groups=0, class/iommu=0")
            .fix("BIOS 开启 VT-d/AMD-Vi，并加启动参数 intel_iommu=on iommu=pt")
    } else {
        f.warn("IOMMU 未启用（策略未强制）")
            .evidence("iommu_groups=0")
    }
}

fn check_nested(cfg: &VirtConfig) -> Finding {
    let f = Finding::new("VIRT-04", "嵌套虚拟化受控", Severity::High);

    let mut evidence = Vec::new();
    let mut nested_on = false;
    let mut present = false;

    for (module, path) in [
        ("kvm_intel", "/sys/module/kvm_intel/parameters/nested"),
        ("kvm_amd", "/sys/module/kvm_amd/parameters/nested"),
    ] {
        if util::is_dir(format!("/sys/module/{module}")) {
            present = true;
            let v = util::read_trim(path).unwrap_or_else(|| "unknown".into());
            evidence.push(format!("{module}.nested = {v}"));
            if matches!(v.as_str(), "Y" | "1" | "y") {
                nested_on = true;
            }
        }
    }

    if !present {
        return f.skip("未加载 kvm_intel/kvm_amd，本机不是 KVM 宿主机");
    }

    // /dev/kvm 的访问面同样是判据
    if let Some(mode) = util::mode("/dev/kvm") {
        evidence.push(format!("/dev/kvm 权限 {mode:04o}"));
    } else {
        evidence.push("/dev/kvm 不存在".into());
    }

    if nested_on && cfg.forbid_nested {
        f.fail("宿主已开启嵌套虚拟化，guest 可再开 KVM 放大攻击面")
            .evidence_all(evidence)
            .fix("modprobe -r kvm_intel && modprobe kvm_intel nested=0（写入 /etc/modprobe.d/kvm.conf）")
    } else if nested_on {
        f.warn("嵌套虚拟化已开启（策略允许）")
            .evidence_all(evidence)
    } else {
        f.pass("嵌套虚拟化关闭").evidence_all(evidence)
    }
}
