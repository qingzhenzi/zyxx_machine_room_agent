//! GRUB / 引导链与内核启动参数核查。
//!
//! 数据来源全部是「事实」而不是「配置意图」：
//! - 生效参数：`/proc/cmdline`
//! - 配置意图：`/etc/default/grub`、`/etc/kernel/cmdline`
//! - 完整性：`grub.cfg` 权限、Secure Boot efivar、lockdown、模块签名

use crate::check::{Check, Ctx};
use crate::config::GrubConfig;
use crate::model::{Finding, Severity};
use crate::util;
use std::time::Duration;

pub struct GrubCheck;

const GRUB_CFGS: &[&str] = &[
    "/boot/grub/grub.cfg",
    "/boot/grub2/grub.cfg",
    "/boot/efi/EFI/grub/grub.cfg",
    "/boot/efi/EFI/fedora/grub.cfg",
    "/boot/efi/EFI/redhat/grub.cfg",
    "/boot/efi/EFI/debian/grub.cfg",
    "/boot/efi/EFI/ubuntu/grub.cfg",
];

const SECUREBOOT_VAR: &str =
    "/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c";

impl Check for GrubCheck {
    fn id(&self) -> &'static str {
        "grub"
    }

    fn title(&self) -> &'static str {
        "GRUB / 引导与内核启动参数"
    }

    fn run(&self, cx: &Ctx<'_>) -> Vec<Finding> {
        let cfg = &cx.cfg.grub;
        let cmdline = util::read_trim("/proc/cmdline").unwrap_or_default();
        let params: Vec<&str> = cmdline
            .split_whitespace()
            .filter(|p| !p.starts_with("BOOT_IMAGE="))
            .collect();

        vec![
            check_required_params(cfg, &params),
            check_forbidden_params(cfg, &params),
            check_cfg_permissions(),
            check_password(cfg),
            check_secure_boot(cfg),
            check_lockdown(cfg),
            check_module_sig(),
            check_recovery_shell(&params),
            check_config_effective(cfg, &params),
        ]
    }
}

/// 启动参数匹配：带 `=` 的按全等，裸 flag 按 key 匹配（`audit` 可匹配 `audit=1`）。
fn param_matches(params: &[&str], want: &str) -> bool {
    if want.contains('=') {
        params.contains(&want)
    } else {
        params.iter().any(|p| p.split('=').next() == Some(want))
    }
}

fn check_required_params(cfg: &GrubConfig, params: &[&str]) -> Finding {
    let f = Finding::new("GRUB-01", "内核启动参数包含安全必需项", Severity::High)
        .reference("CIS 1.4.1 / 等保 8.1.4.1");

    if cfg.required_params.is_empty() {
        return f.skip("策略未配置 required_params，跳过");
    }

    let missing: Vec<&String> = cfg
        .required_params
        .iter()
        .filter(|r| !param_matches(params, r))
        .collect();

    if missing.is_empty() {
        f.pass(format!("{} 项必需参数均已生效", cfg.required_params.len()))
            .evidence(params.join(" ").as_str())
    } else {
        f.fail(format!("缺少 {} 项必需参数", missing.len()))
            .evidence_all(missing.iter().map(|m| format!("missing: {m}")))
            .evidence(format!("实际 cmdline: {}", params.join(" ")))
            .fix("编辑 /etc/default/grub 的 GRUB_CMDLINE_LINUX_DEFAULT，然后 grub-mkconfig -o /boot/grub/grub.cfg（RHEL: grub2-mkconfig）")
    }
}

fn check_forbidden_params(cfg: &GrubConfig, params: &[&str]) -> Finding {
    let f = Finding::new("GRUB-02", "内核启动参数不含禁用项", Severity::High);

    if cfg.forbidden_params.is_empty() {
        return f.skip("策略未配置 forbidden_params，跳过");
    }

    let hit: Vec<&String> = cfg
        .forbidden_params
        .iter()
        .filter(|b| param_matches(params, b))
        .collect();

    if hit.is_empty() {
        f.pass("未发现禁用参数").evidence(params.join(" "))
    } else {
        f.fail("发现禁用启动参数")
            .evidence_all(hit.iter().map(|h| format!("forbidden: {h}")))
            .fix("移除对应参数并重新生成 grub.cfg")
    }
}

fn check_cfg_permissions() -> Finding {
    let f = Finding::new("GRUB-03", "grub.cfg 权限最小化", Severity::Medium).reference("CIS 1.4.2");

    let mut found = Vec::new();
    let mut bad = Vec::new();

    for path in GRUB_CFGS {
        if let Some(mode) = util::mode(path) {
            found.push((*path, mode));
            // grub.cfg 可能包含 password_pbkdf2 哈希，禁止组/其他用户可读
            if mode & 0o077 != 0 {
                bad.push(format!("{path} 权限 {mode:04o}"));
            }
        }
    }

    if found.is_empty() {
        return f.skip("未找到 grub.cfg（可能使用 systemd-boot / EFI stub）");
    }
    if bad.is_empty() {
        f.pass("grub.cfg 不可被非属主读取")
            .evidence_all(found.iter().map(|(p, m)| format!("{p}: {m:04o}")))
    } else {
        f.fail("grub.cfg 权限过宽，口令哈希可被本地读取")
            .evidence_all(bad)
            .fix("chmod 0600 /boot/grub/grub.cfg")
    }
}

fn check_password(cfg: &GrubConfig) -> Finding {
    let f =
        Finding::new("GRUB-04", "GRUB 已设置超级用户口令", Severity::High).reference("CIS 1.4.3");

    if !cfg.require_password {
        return f.skip("策略未要求 GRUB 口令");
    }

    let mut evidence = Vec::new();
    let mut configured = false;
    let mut readable = 0usize;

    // Debian/Ubuntu: grub.cfg 里生成 password_pbkdf2 + set superuser
    for path in GRUB_CFGS {
        if let Ok(text) = std::fs::read_to_string(path) {
            readable += 1;
            if text.contains("password_pbkdf2") || text.contains("set superuser") {
                configured = true;
                evidence.push(format!("{path} 含 password_pbkdf2/superuser"));
            }
        }
    }
    // 读不到任何 grub.cfg 时不能断言「没设口令」，否则非 root 运行会误报
    if readable == 0 && !configured {
        return f.skip("grub.cfg 不可读（需 root），无法判定口令是否已设置");
    }
    // Debian 的 user.cfg / RHEL 的 user.cfg
    for path in ["/boot/grub/user.cfg", "/boot/grub2/user.cfg"] {
        if let Some(text) = util::read_trim(path) {
            configured = true;
            evidence.push(format!("{path}: {} 字节", text.len()));
        }
    }

    if configured {
        f.pass("GRUB 口令已配置（仍建议人工确认可用）")
            .evidence_all(evidence)
            .fix("用 grub2-setpassword 轮换口令，并确保 user.cfg 权限 0600")
    } else {
        f.fail("未检测到 GRUB 口令，物理接触者可进单用户模式获得 root shell")
            .fix("grub2-setpassword（Debian: grub-mkpasswd-pbkdf2 + 40_custom），并 chmod 0600 user.cfg")
    }
}

fn check_secure_boot(cfg: &GrubConfig) -> Finding {
    let f =
        Finding::new("GRUB-05", "UEFI Secure Boot 已启用", Severity::Medium).reference("CIS 1.4.4");

    if !util::is_dir("/sys/firmware/efi") {
        return f.skip("非 UEFI 启动（Legacy BIOS），不适用");
    }

    let enabled = std::fs::read(SECUREBOOT_VAR)
        .ok()
        .and_then(|d| d.get(4).copied())
        .map(|v| v == 1);

    match enabled {
        Some(true) => f.pass("SecureBoot=1"),
        Some(false) => {
            if cfg.require_secure_boot {
                f.fail("SecureBoot=0，引导链未做签名校验")
                    .evidence("efivar SecureBoot = 0")
                    .fix("在固件设置中开启 Secure Boot，并注册发行版密钥（mokutil --import）")
            } else {
                f.warn("SecureBoot=0（策略未强制，仅提示）")
                    .evidence("efivar SecureBoot = 0")
            }
        }
        None => {
            // efivarfs 未挂载或读取被拒，退回 mokutil
            match util::run("mokutil", &["--sb-state"], Duration::from_secs(5)) {
                Some(o) if o.ok() => {
                    if o.stdout.to_lowercase().contains("enabled") {
                        f.pass("mokutil: SecureBoot enabled")
                            .evidence(o.stdout.trim())
                    } else {
                        f.warn("mokutil 报告 Secure Boot 未启用")
                            .evidence(o.stdout.trim())
                    }
                }
                _ => f.skip("无法读取 SecureBoot efivar（需 root 或 efivarfs 未挂载）"),
            }
        }
    }
}

fn check_lockdown(cfg: &GrubConfig) -> Finding {
    let f =
        Finding::new("GRUB-06", "内核 lockdown 已启用", Severity::Medium).reference("CIS 1.4.5");

    let Some(raw) = util::read_trim("/sys/kernel/security/lockdown") else {
        return f.skip("lockdown 未编译进内核或 securityfs 未挂载");
    };

    // 形如 "none [integrity] confidentiality" 或 "none integrity [confidentiality]"
    let active = raw
        .split_whitespace()
        .find(|t| t.starts_with('['))
        .map(|t| t.trim_matches(|c| c == '[' || c == ']').to_string())
        .unwrap_or_else(|| "none".into());

    if active == "none" {
        if cfg.require_lockdown {
            f.fail("lockdown=none，内核未阻止 kexec/直接改内核内存等操作")
                .evidence(format!("securityfs: {raw}"))
                .fix("启动参数加 lockdown=integrity（需 CONFIG_SECURITY_LOCKDOWN_LSM）")
        } else {
            f.warn("lockdown 未启用（策略未强制）")
                .evidence(format!("securityfs: {raw}"))
        }
    } else {
        f.pass(format!("lockdown={active}"))
            .evidence(format!("securityfs: {raw}"))
    }
}

fn check_module_sig() -> Finding {
    let f = Finding::new("GRUB-07", "内核具备模块签名能力", Severity::High).reference("CIS 1.5.1");

    // 这个参数节点存在，就说明内核编译了 CONFIG_MODULE_SIG
    let runtime = util::read_trim("/sys/module/module/parameters/sig_enforce");
    let config = util::kernel_config();
    let sig_built = runtime.is_some()
        || config
            .as_deref()
            .map(|c| c.lines().any(|l| l.trim() == "CONFIG_MODULE_SIG=y"))
            .unwrap_or(false);

    if sig_built {
        let mut out = f.pass("CONFIG_MODULE_SIG=y，内核可校验模块签名");
        if let Some(v) = &runtime {
            out = out.evidence(format!("sig_enforce = {v}"));
        }
        return out;
    }

    match config {
        Some(text)
            if text
                .lines()
                .any(|l| l.trim() == "# CONFIG_MODULE_SIG is not set") =>
        {
            f.fail("内核未编译模块签名支持（CONFIG_MODULE_SIG=n）").fix(
                "换用开启 CONFIG_MODULE_SIG 的内核；否则只能靠 kernel.modules_disabled 冻结加载",
            )
        }
        Some(_) => f.skip("内核配置中未找到 CONFIG_MODULE_SIG 项"),
        None => f.skip("无法读取内核配置，且无 sig_enforce 参数"),
    }
}

fn check_recovery_shell(params: &[&str]) -> Finding {
    let f = Finding::new("GRUB-08", "启动参数未打开维护者 shell", Severity::High)
        .reference("CIS 1.4.6");

    let dangerous = [
        "rd.break",
        "init=/bin/sh",
        "init=/bin/bash",
        "single",
        "emergency",
    ];
    let hit: Vec<&str> = dangerous
        .iter()
        .copied()
        .filter(|d| {
            params
                .iter()
                .any(|p| p == d || p.starts_with(&format!("{d} ")))
        })
        .collect();

    if hit.is_empty() {
        f.pass("未发现单用户/救援启动参数")
    } else {
        f.fail("启动参数绕过了认证直接进 shell")
            .evidence_all(hit.iter().map(|h| format!("匹配: {h}")))
            .fix("移除该参数；如确需救援，用 GRUB 口令保护编辑权限")
    }
}

fn check_config_effective(cfg: &GrubConfig, params: &[&str]) -> Finding {
    let f = Finding::new("GRUB-09", "GRUB 配置与运行参数一致", Severity::Low);

    if !cfg.check_config_effective {
        return f.skip("策略关闭该项");
    }

    let kv = util::parse_kv_file("/etc/default/grub");
    if kv.is_empty() {
        return f.skip("未找到 /etc/default/grub");
    }

    let configured: Vec<String> = ["GRUB_CMDLINE_LINUX", "GRUB_CMDLINE_LINUX_DEFAULT"]
        .iter()
        .filter_map(|k| kv.get(*k))
        .flat_map(|v| v.split_whitespace().map(str::to_string))
        .collect();

    if configured.is_empty() {
        return f.skip("/etc/default/grub 未配置任何命令行参数");
    }

    let pending: Vec<&String> = configured
        .iter()
        .filter(|c| !param_matches(params, c))
        .collect();

    if pending.is_empty() {
        f.pass("配置项均已生效").evidence(configured.join(" "))
    } else {
        f.warn("配置了但未生效，可能忘记重新生成 grub.cfg")
            .evidence_all(pending.iter().map(|p| format!("pending: {p}")))
            .fix("grub-mkconfig -o /boot/grub/grub.cfg 后重启")
    }
}

#[cfg(test)]
mod tests {
    use super::param_matches;

    #[test]
    fn valued_param_requires_exact_match() {
        assert!(param_matches(&["audit=1"], "audit=1"));
        // audit=0 不能算作满足 audit=1
        assert!(!param_matches(&["audit=0"], "audit=1"));
    }

    #[test]
    fn bare_flag_matches_by_key() {
        assert!(param_matches(&["selinux=1"], "selinux"));
        assert!(param_matches(&["quiet"], "quiet"));
        assert!(!param_matches(&["nosmap"], "nosmt"));
    }
}
