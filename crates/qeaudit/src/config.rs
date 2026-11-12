//! 基线策略。所有阈值、白名单、必需项都在这里，检查代码里不写死业务值。
//!
//! 设计原则：
//! - 每个字段都有 `Default`，缺省即可跑；
//! - `deny_unknown_fields`，配置文件写错字段名直接报错，而不是静默忽略。

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub profile: Profile,
    pub grub: GrubConfig,
    pub kernel: KernelConfig,
    pub virt: VirtConfig,
    pub space: SpaceConfig,
    pub device: DeviceConfig,
    pub network: NetworkConfig,
    pub root: RootConfig,
    pub process: ProcessConfig,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("读取配置 {} 失败: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("解析配置 {} 失败: {e}", path.display()))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    /// 报告里附带的主机标识，留空则用 hostname。
    pub asset_id: String,
    /// 该基线对应的合规框架，仅用于报告展示。
    pub framework: String,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: "baseline".into(),
            asset_id: String::new(),
            framework: String::new(),
        }
    }
}

// ---------------------------------------------------------------- GRUB

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GrubConfig {
    /// 内核命令行必须包含的参数，`audit=1` 这种带值的按全等匹配，裸 flag 按 key 匹配。
    pub required_params: Vec<String>,
    /// 内核命令行禁止出现的参数。
    pub forbidden_params: Vec<String>,
    /// 是否要求 GRUB 设置了超级用户口令（防止单用户模式直接拿 root）。
    pub require_password: bool,
    /// 是否要求 Secure Boot 已开启（UEFI 机器）。
    pub require_secure_boot: bool,
    /// 是否要求内核 lockdown 处于 integrity 或 confidentiality。
    pub require_lockdown: bool,
    /// 是否比对 /etc/default/grub 与 /proc/cmdline，发现「改了没生效」。
    pub check_config_effective: bool,
}

impl Default for GrubConfig {
    fn default() -> Self {
        Self {
            required_params: Vec::new(),
            forbidden_params: vec!["selinux=0".into(), "init=/bin/sh".into()],
            require_password: true,
            require_secure_boot: false,
            require_lockdown: false,
            check_config_effective: true,
        }
    }
}

// ---------------------------------------------------------------- 虚拟化

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtExpect {
    /// 只做探测，不做合规判断。
    Any,
    /// 期望裸金属。
    BareMetal,
    /// 期望虚拟机。
    VirtualMachine,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct VirtConfig {
    pub expect: VirtExpect,
    /// 要求 IOMMU 已启用（DMA 重映射，防外设 DMA 攻击）。
    pub require_iommu: bool,
    /// 禁止嵌套虚拟化（宿主机上没管住 /dev/kvm 就是提权面）。
    pub forbid_nested: bool,
    /// 期望的 hypervisor 类型，空表示不限定。
    pub expected_hypervisor: String,
}

impl Default for VirtConfig {
    fn default() -> Self {
        Self {
            expect: VirtExpect::Any,
            require_iommu: false,
            forbid_nested: true,
            expected_hypervisor: String::new(),
        }
    }
}

// ---------------------------------------------------------------- 空间

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MountRule {
    pub path: String,
    /// 可用空间低于该百分比即告警。
    pub min_free_percent: u8,
    /// 可用空间低于该字节数即告警，0 表示不启用。
    pub min_free_bytes: u64,
    /// inode 使用率高于该百分比即告警。
    pub max_inode_used_percent: u8,
}

impl Default for MountRule {
    fn default() -> Self {
        Self {
            path: "/".into(),
            min_free_percent: 10,
            min_free_bytes: 1 << 30,
            max_inode_used_percent: 90,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpaceConfig {
    pub mounts: Vec<MountRule>,
    /// 是否检查这些挂载点的 nosuid/nodev/noexec。
    pub check_mount_options: bool,
    pub restricted_mounts: Vec<String>,
}

impl Default for SpaceConfig {
    fn default() -> Self {
        Self {
            mounts: vec![MountRule::default()],
            check_mount_options: true,
            restricted_mounts: vec!["/tmp".into(), "/dev/shm".into(), "/var/tmp".into()],
        }
    }
}

// ---------------------------------------------------------------- 设备

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeviceConfig {
    /// 必须已加载的内核模块（出现在 /sys/module 或 /proc/modules）。
    pub required_modules: Vec<String>,
    /// 必须存在的设备类别（/sys/class/<name> 非空）。
    pub required_classes: Vec<String>,
    /// 关键 PCI 类别码（十六进制前缀）必须绑定驱动，例如 "0200" 以太网卡。
    pub critical_pci_classes: Vec<String>,
    /// systemd 存在失败单元是否算缺陷。
    pub fail_on_failed_units: bool,
    /// 温度上限（摄氏度），0 表示不检查。
    pub max_thermal_c: i64,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            required_modules: Vec::new(),
            required_classes: vec!["net".into(), "block".into(), "tty".into()],
            critical_pci_classes: vec!["0200".into(), "0280".into(), "0108".into(), "0300".into()],
            fail_on_failed_units: true,
            max_thermal_c: 100,
        }
    }
}

// ---------------------------------------------------------------- 网络

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkConfig {
    /// 出网连通性探测目标，`host:port`。
    pub connectivity_targets: Vec<String>,
    pub connectivity_timeout_ms: u64,
    /// 必须处于 up 状态的非 loopback 网卡数量下限。
    pub min_up_interfaces: usize,
    /// 禁止监听的端口。
    pub forbidden_listen_ports: Vec<u16>,
    /// 是否要求主机防火墙存在规则。
    pub require_firewall: bool,
    /// 是否校验内核网络参数加固基线。
    pub enforce_sysctl: bool,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            connectivity_targets: vec!["1.1.1.1:443".into()],
            connectivity_timeout_ms: 2000,
            min_up_interfaces: 1,
            forbidden_listen_ports: Vec::new(),
            require_firewall: true,
            enforce_sysctl: true,
        }
    }
}

// ---------------------------------------------------------------- root 限制

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RootConfig {
    /// 是否检查 sshd 配置（机器上没装 sshd 时自动跳过）。
    pub check_sshd: bool,
    /// 是否要求存在某个强制访问控制（SELinux enforcing / AppArmor enabled）。
    pub require_lsm: bool,
    /// 是否校验内核提权加固参数。
    pub enforce_sysctl: bool,
    /// 允许存在的「root + 全量 capability」进程数（排掉 PID 1 后）。
    pub max_full_cap_root_procs: usize,
}

impl Default for RootConfig {
    fn default() -> Self {
        Self {
            check_sshd: true,
            require_lsm: true,
            enforce_sysctl: true,
            max_full_cap_root_procs: 2,
        }
    }
}

// ---------------------------------------------------------------- 进程保护

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProtectedProc {
    /// 与 /proc/<pid>/comm 精确匹配。
    pub name: String,
    /// oom_score_adj 上限，越负越不容易被 OOM killer 干掉。
    pub max_oom_score_adj: i32,
}

impl Default for ProtectedProc {
    fn default() -> Self {
        Self {
            name: String::new(),
            max_oom_score_adj: -500,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProcessConfig {
    pub protected: Vec<ProtectedProc>,
    /// 要求本进程已设置 no_new_privs。
    pub require_no_new_privs: bool,
    /// 要求本进程有 seccomp 过滤器。
    pub require_seccomp: bool,
    /// 若以 systemd 单元运行，校验单元加固项。
    pub check_systemd_unit: bool,
    /// 单元加固项最低满足数量（满分 6）。
    pub min_hardening_score: usize,
    pub check_self: bool,
}

impl Default for ProcessConfig {
    fn default() -> Self {
        Self {
            protected: Vec::new(),
            require_no_new_privs: false,
            require_seccomp: false,
            check_systemd_unit: true,
            min_hardening_score: 4,
            check_self: true,
        }
    }
}

// ---------------------------------------------------------------- 内核模块加载面

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct KernelConfig {
    /// 要求「未签名模块被拒绝加载」：sig_enforce / SIG_FORCE / modules_disabled 任一成立即可。
    pub require_signed_modules: bool,
    /// 要求启动完成后冻结模块加载（kernel.modules_disabled=1）。会禁用热插拔，默认不要求。
    pub require_modules_disabled: bool,
    /// 要求禁用 kexec（kernel.kexec_load_disabled=1）。否则 root 可 kexec 另一个内核绕过一切运行时限制。
    pub require_kexec_disabled: bool,
}

impl Default for KernelConfig {
    fn default() -> Self {
        Self {
            require_signed_modules: true,
            require_modules_disabled: false,
            require_kexec_disabled: false,
        }
    }
}
