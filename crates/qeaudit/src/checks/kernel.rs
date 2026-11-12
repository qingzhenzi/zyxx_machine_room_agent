//! 内核模块加载面：能不能在启动后把 root 加载模块的能力收掉。
//!
//! 三个开关，强度递增、都是"开弓没有回头箭"（重启才复位）：
//!
//! | 机制 | 作用 | 代价 |
//! | --- | --- | --- |
//! | `module.sig_enforce=1` | 拒绝未签名/签名不可验证的模块 | 树外模块（nvidia/zfs/vbox）需先签名 |
//! | `lockdown=integrity` | 顺带拒绝未签名模块，并禁 kexec//dev/mem/MSR | 需启动参数，部分工具受限 |
//! | `kernel.modules_disabled=1` | 任何模块都不许再加载 | 热插拔、USB、后续驱动全部失效 |
//!
//! 注意：这些开关都**不能**在运行时被关闭（写 0 返回错误），但 root 可以
//! `reboot` 换个启动参数绕过——所以真正的边界是 Secure Boot + GRUB 口令，
//! 本文件的检查只能证明"当前这层是开着的"。

use crate::check::{Check, Ctx};
use crate::config::KernelConfig;
use crate::model::{Finding, Severity};
use crate::util;

pub struct KernelCheck;

const SIG_ENFORCE: &str = "/sys/module/module/parameters/sig_enforce";

impl Check for KernelCheck {
    fn id(&self) -> &'static str {
        "kernel"
    }

    fn title(&self) -> &'static str {
        "内核模块加载面"
    }

    fn run(&self, cx: &Ctx<'_>) -> Vec<Finding> {
        let cfg = &cx.cfg.kernel;
        vec![
            check_signature(cfg),
            check_modules_disabled(cfg),
            check_kexec(cfg),
        ]
    }
}

/// `sig_enforce` 是 bool 型模块参数，内核用 Y/N 表示；兼容 1/0。
fn bool_param(value: &str) -> bool {
    matches!(value.trim(), "Y" | "y" | "1")
}

fn check_signature(cfg: &KernelConfig) -> Finding {
    let f = Finding::new("KERN-01", "未签名内核模块被拒绝加载", Severity::Critical)
        .reference("CIS 1.5.1");

    let runtime = util::read_trim(SIG_ENFORCE);
    let disabled = util::sysctl_i64("kernel.modules_disabled").unwrap_or(0) == 1;
    let force_builtin = util::kernel_config()
        .map(|c| c.lines().any(|l| l.trim() == "CONFIG_MODULE_SIG_FORCE=y"))
        .unwrap_or(false);

    // 模块加载被完全冻结时，未签名模块自然也进不来
    if disabled {
        return f
            .pass("kernel.modules_disabled=1，模块加载已完全冻结")
            .evidence("任何模块（含已签名）都无法再加载，重启才恢复");
    }

    let enforced = runtime.as_deref().map(bool_param).unwrap_or(false);
    if enforced || force_builtin {
        return f
            .pass("未签名模块会被内核拒绝")
            .evidence(format!(
                "sig_enforce = {}",
                runtime.clone().unwrap_or_else(|| "-".into())
            ))
            .evidence(format!("CONFIG_MODULE_SIG_FORCE = {force_builtin}"));
    }

    let Some(runtime) = runtime else {
        // 连参数节点都没有：内核没编 CONFIG_MODULE_SIG
        let detail = "内核未编译模块签名支持（无 sig_enforce 参数）";
        return if cfg.require_signed_modules {
            f.fail(detail).fix(
                "只能启用 kernel.modules_disabled=1 冻结加载，或换用开启 CONFIG_MODULE_SIG 的内核",
            )
        } else {
            f.warn(detail)
        };
    };

    let detail = format!("sig_enforce = {runtime}，内核当前接受未签名模块");
    if cfg.require_signed_modules {
        f.fail(detail)
            .evidence(format!("CONFIG_MODULE_SIG_FORCE = {force_builtin}"))
            .fix(
                "临时: echo 1 > /sys/module/module/parameters/sig_enforce（只能开不能关）；\
                 永久: 启动参数加 module.sig_enforce=1 或内核编 CONFIG_MODULE_SIG_FORCE=y；\
                 树外模块需先用 MOK 密钥签名",
            )
    } else {
        f.warn(detail)
    }
}

fn check_modules_disabled(cfg: &KernelConfig) -> Finding {
    let f = Finding::new("KERN-02", "启动完成后冻结模块加载", Severity::High);

    let Some(v) = util::sysctl_i64("kernel.modules_disabled") else {
        return f.skip("无法读取 kernel.modules_disabled");
    };
    if v == 1 {
        return f
            .pass("modules_disabled=1")
            .evidence("此后无法加载任何模块，重启才恢复");
    }

    let detail = "modules_disabled=0，root 仍可加载任意模块";
    if cfg.require_modules_disabled {
        f.fail(detail).fix(
            "所有必需模块加载完成后: sysctl -w kernel.modules_disabled=1（不可逆；\
             会禁用热插拔/USB/后续驱动，务必先确认 rescue 路径）",
        )
    } else {
        f.warn(detail)
            .fix("如需彻底冻结：在启动末期 oneshot 单元里执行 sysctl -w kernel.modules_disabled=1")
    }
}

fn check_kexec(cfg: &KernelConfig) -> Finding {
    let f = Finding::new("KERN-03", "kexec 已禁用", Severity::High);

    let Some(v) = util::sysctl_i64("kernel.kexec_load_disabled") else {
        return f.skip("无法读取 kernel.kexec_load_disabled");
    };
    if v == 1 {
        return f.pass("kexec_load_disabled=1");
    }

    let detail = "kexec 可用，root 可 kexec 一个未加固内核，绕过上面所有运行时限制";
    if cfg.require_kexec_disabled {
        f.fail(detail)
            .fix("sysctl -w kernel.kexec_load_disabled=1（不可逆，重启复位）")
    } else {
        f.warn(detail)
    }
}

#[cfg(test)]
mod tests {
    use super::bool_param;

    #[test]
    fn bool_param_accepts_kernel_spellings() {
        assert!(bool_param("Y"));
        assert!(bool_param("1"));
        assert!(!bool_param("N"));
        assert!(!bool_param("0"));
        assert!(!bool_param(""));
    }
}
