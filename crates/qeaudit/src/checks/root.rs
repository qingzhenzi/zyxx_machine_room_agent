//! root 用户限制：账户、远程登录、sudo、强制访问控制、Capability 收敛。

use crate::check::{Check, Ctx};
use crate::model::{Finding, Severity};
use crate::util;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub struct RootCheck;

impl Check for RootCheck {
    fn id(&self) -> &'static str {
        "root"
    }

    fn title(&self) -> &'static str {
        "root 权限收敛"
    }

    fn run(&self, cx: &Ctx<'_>) -> Vec<Finding> {
        let cfg = &cx.cfg.root;
        let mut out = vec![
            check_uid0(),
            check_shadow(),
            check_sshd(),
            check_sudo(),
            check_sysctl(cfg),
            check_lsm(cfg),
            check_capabilities(cfg),
        ];
        if !cfg.check_sshd {
            out.retain(|f| f.id != "ROOT-03");
        }
        out
    }
}

/// UID 0 只允许存在一个 `root`。多出来的等价账户是最常见的后门形态。
fn check_uid0() -> Finding {
    let f = Finding::new("ROOT-01", "UID 0 账户唯一", Severity::Critical).reference("CIS 6.2.5");

    let mut uid0 = Vec::new();
    for line in util::read_lines("/etc/passwd") {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 7 {
            continue;
        }
        if fields[2] == "0" {
            uid0.push(fields[0].to_string());
        }
    }

    if uid0.is_empty() {
        return f.skip("无法读取 /etc/passwd");
    }
    if uid0.len() == 1 && uid0[0] == "root" {
        f.pass("仅 root 拥有 UID 0")
    } else {
        f.fail(format!("发现 {} 个 UID 0 账户", uid0.len()))
            .evidence_all(uid0.iter().map(|u| format!("uid0: {u}")))
            .fix("usermod -u <新uid> <账户> 或 userdel 无关账户；并审计登录记录")
    }
}

fn check_shadow() -> Finding {
    let f = Finding::new("ROOT-02", "口令文件权限与空口令", Severity::High)
        .reference("CIS 6.1.2-6.1.3");

    let mut evidence = Vec::new();
    let mut issues = Vec::new();

    if let Some(mode) = util::mode("/etc/shadow") {
        evidence.push(format!("/etc/shadow 权限 {mode:04o}"));
        // 允许 0000/0400/0600/0640，禁止其他用户可读
        if mode & 0o007 != 0 {
            issues.push(format!("/etc/shadow 其他用户可访问（{mode:04o}）"));
        }
    } else {
        return f.skip("/etc/shadow 不可读（需 root）");
    }

    match std::fs::read_to_string("/etc/shadow") {
        Ok(text) => {
            let mut empty = Vec::new();
            let mut unlocked_root = false;
            for line in text.lines() {
                let fields: Vec<&str> = line.split(':').collect();
                if fields.len() < 2 {
                    continue;
                }
                if fields[1].is_empty() {
                    empty.push(fields[0].to_string());
                }
                if fields[0] == "root"
                    && !fields[1].is_empty()
                    && !fields[1].starts_with(['!', '*'])
                {
                    unlocked_root = true;
                }
            }
            if !empty.is_empty() {
                issues.push(format!("{} 个账户口令为空", empty.len()));
                evidence.extend(empty.iter().map(|u| format!("empty password: {u}")));
            }
            evidence.push(format!(
                "root 口令字段: {}",
                if unlocked_root {
                    "已设置哈希"
                } else {
                    "锁定/无哈希"
                }
            ));
        }
        Err(e) => {
            // 非 root 读不到明文时不能断言"没有空口令"，必须降级为 Skip
            return f
                .skip(format!("/etc/shadow 不可读（{e}），需 root 才能判定"))
                .evidence_all(evidence);
        }
    }

    if issues.is_empty() {
        f.pass("口令文件权限合规且无空口令").evidence_all(evidence)
    } else {
        f.fail(issues.join("；"))
            .evidence_all(evidence)
            .fix("passwd -l <user> 锁定空口令账户；chmod 0640 /etc/shadow")
    }
}

fn check_sshd() -> Finding {
    let f = Finding::new(
        "ROOT-03",
        "SSH 禁止 root 直登与口令爆破",
        Severity::Critical,
    )
    .reference("CIS 5.2.7-5.2.10");

    let main = Path::new("/etc/ssh/sshd_config");
    if !main.exists() {
        return f.skip("未安装 OpenSSH server");
    }
    let cfg = sshd_effective(main, 0);
    if cfg.is_empty() {
        return f.skip("sshd_config 不可读");
    }

    let mut issues: Vec<String> = Vec::new();
    let mut evidence = Vec::new();

    let get = |k: &str| cfg.get(k).map(|s| s.to_lowercase());

    // PermitRootLogin 默认 prohibit-password（OpenSSH >= 7.0）
    let prl = get("permitrootlogin").unwrap_or_else(|| "prohibit-password".into());
    evidence.push(format!("PermitRootLogin = {prl}"));
    if prl == "yes" || prl == "without-password" {
        issues.push("PermitRootLogin 允许 root 直接登录".to_string());
    }

    // PasswordAuthentication 默认 yes
    let pwa = get("passwordauthentication").unwrap_or_else(|| "yes".into());
    evidence.push(format!("PasswordAuthentication = {pwa}"));
    if pwa == "yes" {
        issues.push("PasswordAuthentication 仍为 yes，可被在线爆破".to_string());
    }

    let empty = get("permitemptypasswords").unwrap_or_else(|| "no".into());
    evidence.push(format!("PermitEmptyPasswords = {empty}"));
    if empty == "yes" {
        issues.push("允许空口令登录".to_string());
    }

    if let Some(n) = get("maxauthtries").and_then(|v| v.parse::<u32>().ok()) {
        evidence.push(format!("MaxAuthTries = {n}"));
        if n > 3 {
            issues.push(format!("MaxAuthTries={n} 偏大（建议 <= 3）"));
        }
    }

    if get("x11forwarding").as_deref() == Some("yes") {
        evidence.push("X11Forwarding = yes".into());
    }

    if issues.is_empty() {
        f.pass("SSH 关键项符合基线").evidence_all(evidence)
    } else {
        f.fail(issues.join("；"))
            .evidence_all(evidence)
            .fix("在 /etc/ssh/sshd_config.d/99-hardening.conf 中设置 PermitRootLogin no、PasswordAuthentication no，然后 sshd -t && systemctl reload sshd")
    }
}

/// 展开 `Include` 并按出现顺序合并，后出现的覆盖前面的（与 sshd 语义一致）。
fn sshd_effective(path: &Path, depth: usize) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if depth > 4 {
        return map;
    }
    for raw in util::read_lines(path) {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or("").to_lowercase();
        let val = parts.next().unwrap_or("").trim().to_string();
        if key.is_empty() {
            continue;
        }
        if key == "include" {
            for inc in expand_glob(&val) {
                map.extend(sshd_effective(&inc, depth + 1));
            }
        } else {
            map.insert(key, val);
        }
    }
    map
}

/// 只支持 `dir/*.conf` 这一种 sshd 会用到的通配形式，不引入 glob crate。
fn expand_glob(pattern: &str) -> Vec<PathBuf> {
    let p = Path::new(pattern);
    let Some(dir) = p.parent() else {
        return Vec::new();
    };
    let Some(suffix) = p.file_name().and_then(|f| f.to_str()) else {
        return Vec::new();
    };
    let suffix = suffix.trim_start_matches('*');
    let mut out: Vec<PathBuf> = util::list_dir(dir)
        .into_iter()
        .filter(|name| name.ends_with(suffix))
        .map(|name| dir.join(name))
        .collect();
    out.sort();
    out
}

fn check_sudo() -> Finding {
    let f =
        Finding::new("ROOT-04", "sudo 配置无免密提权", Severity::High).reference("CIS 5.3.1-5.3.4");

    if !util::exists("/etc/sudoers") {
        return f.skip("未安装 sudo");
    }

    let mut evidence = Vec::new();
    let mut issues = Vec::new();

    if let Some(mode) = util::mode("/etc/sudoers") {
        evidence.push(format!("/etc/sudoers 权限 {mode:04o}"));
        if mode & 0o022 != 0 {
            issues.push("/etc/sudoers 可被组/其他用户写".to_string());
        }
    }
    if let Some(mode) = util::mode("/etc/sudoers.d") {
        evidence.push(format!("/etc/sudoers.d 权限 {mode:04o}"));
        if mode & 0o022 != 0 {
            issues.push("/etc/sudoers.d 可被组/其他用户写".to_string());
        }
    }

    let mut files = vec![PathBuf::from("/etc/sudoers")];
    files.extend(
        util::list_dir("/etc/sudoers.d")
            .into_iter()
            .map(|n| Path::new("/etc/sudoers.d").join(n)),
    );

    let mut nopasswd = Vec::new();
    for file in files {
        for (idx, line) in util::read_lines(&file).into_iter().enumerate() {
            let l = line.trim();
            if l.starts_with('#') || l.is_empty() {
                continue;
            }
            if l.contains("NOPASSWD:") {
                nopasswd.push(format!("{}:{} {}", file.display(), idx + 1, l));
            }
        }
    }

    if !nopasswd.is_empty() {
        issues.push(format!("{} 处 NOPASSWD 免密提权", nopasswd.len()));
        evidence.extend(nopasswd.iter().take(10).cloned());
    }

    if issues.is_empty() {
        f.pass("sudo 配置无免密规则").evidence_all(evidence)
    } else {
        f.fail(issues.join("；"))
            .evidence_all(evidence)
            .fix("visudo 移除 NOPASSWD；/etc/sudoers 权限 0440，/etc/sudoers.d 0750")
    }
}

/// 内核提权加固参数：`(key, 允许值, 说明)`。
const ROOT_SYSCTLS: &[(&str, &[&str], &str)] = &[
    ("kernel.kptr_restrict", &["1", "2"], "隐藏内核指针"),
    ("kernel.dmesg_restrict", &["1"], "限制 dmesg 读取"),
    (
        "kernel.perf_event_paranoid",
        &["2", "3"],
        "限制 perf 侧信道",
    ),
    (
        "kernel.yama.ptrace_scope",
        &["1", "2", "3"],
        "限制 ptrace 注入",
    ),
    (
        "kernel.unprivileged_bpf_disabled",
        &["1", "2"],
        "禁非特权 eBPF",
    ),
    ("kernel.randomize_va_space", &["2"], "完整 ASLR"),
    ("fs.protected_hardlinks", &["1"], "防硬链接攻击"),
    ("fs.protected_symlinks", &["1"], "防符号链接攻击"),
    ("fs.suid_dumpable", &["0"], "禁止 suid 进程 core dump"),
];

fn check_sysctl(cfg: &crate::config::RootConfig) -> Finding {
    let f = Finding::new("ROOT-05", "内核提权面已收敛", Severity::Medium).reference("CIS 1.5.x");

    if !cfg.enforce_sysctl {
        return f.skip("策略关闭该项");
    }

    let mut bad = Vec::new();
    let mut good = Vec::new();

    for (key, allowed, desc) in ROOT_SYSCTLS {
        let Some(actual) = util::sysctl(key) else {
            continue;
        };
        if allowed.contains(&actual.as_str()) {
            good.push(format!("{key} = {actual}"));
        } else {
            bad.push(format!(
                "{key} = {actual}（期望 {}，{desc}）",
                allowed.join("/")
            ));
        }
    }

    if bad.is_empty() {
        f.pass(format!("{} 项内核参数符合基线", good.len()))
            .evidence_all(good)
    } else {
        f.fail(format!("{} 项内核参数不符合基线", bad.len()))
            .evidence_all(bad)
            .fix("写入 /etc/sysctl.d/99-root-hardening.conf 后 sysctl --system")
    }
}

fn check_lsm(cfg: &crate::config::RootConfig) -> Finding {
    let f = Finding::new("ROOT-06", "强制访问控制已启用", Severity::High).reference("CIS 1.6.1");

    let lsms = util::read_trim("/sys/kernel/security/lsm").unwrap_or_default();
    let selinux_enforce = util::read_trim("/sys/fs/selinux/enforce").as_deref() == Some("1");
    let apparmor = util::read_trim("/sys/module/apparmor/parameters/enabled")
        .map(|v| v.eq_ignore_ascii_case("y"))
        .unwrap_or(false);

    let mut evidence = Vec::new();
    if !lsms.is_empty() {
        evidence.push(format!("active LSM: {lsms}"));
    }
    evidence.push(format!("selinux enforcing = {selinux_enforce}"));
    evidence.push(format!("apparmor enabled = {apparmor}"));

    let value = if selinux_enforce {
        Some("SELinux enforcing")
    } else if apparmor {
        Some("AppArmor enabled")
    } else {
        None
    };

    match value {
        Some(v) => f.pass(v).evidence_all(evidence),
        None if cfg.require_lsm => f
            .fail("SELinux/AppArmor 均未启用，root 被攻陷后无横向约束")
            .evidence_all(evidence)
            .fix("Debian/Ubuntu: apt install apparmor && systemctl enable apparmor；RHEL: selinux=1 enforcing=1"),
        None => f
            .warn("未启用 MAC（策略不强制）")
            .evidence_all(evidence),
    }
}

/// root 进程若持有全部 capability，等于「未收敛的 root」。统计这类进程数量。
fn check_capabilities(cfg: &crate::config::RootConfig) -> Finding {
    let f = Finding::new("ROOT-07", "root 进程 capability 已收敛", Severity::Medium);

    let last_cap = util::sysctl_i64("kernel.cap_last_cap")
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(63);
    let full: u64 = if last_cap >= 63 {
        u64::MAX
    } else {
        (1u64 << (last_cap + 1)) - 1
    };

    let mut offenders = Vec::new();
    let mut root_procs = 0usize;

    for (pid, comm) in util::pids() {
        if pid <= 2 {
            continue; // init / kthreadd 天然持有全部能力
        }
        // 内核线程的 PPid 都是 kthreadd(2)，它们没有用户态能力集，不应计入
        let ppid = util::proc_status(pid, "PPid")
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(0);
        if ppid <= 2 {
            continue;
        }
        let Some(uid_line) = util::proc_status(pid, "Uid") else {
            continue;
        };
        let Some(uid) = uid_line.split_whitespace().next() else {
            continue;
        };
        if uid != "0" {
            continue;
        }
        root_procs += 1;
        let Some(capeff) = util::proc_status(pid, "CapEff") else {
            continue;
        };
        let Ok(bits) = u64::from_str_radix(capeff.trim(), 16) else {
            continue;
        };
        if bits == full {
            offenders.push(format!("pid {pid} {comm} CapEff={capeff}"));
        }
    }

    if root_procs == 0 {
        return f.skip("当前无 root 进程（或 /proc 不可读）");
    }
    if offenders.len() <= cfg.max_full_cap_root_procs {
        f.pass(format!(
            "{root_procs} 个 root 进程中 {} 个持全量 capability（阈值 {}）",
            offenders.len(),
            cfg.max_full_cap_root_procs
        ))
    } else {
        f.warn(format!(
            "{} 个 root 进程持有全量 capability",
            offenders.len()
        ))
        .evidence_all(offenders.into_iter().take(20))
        .fix("在 systemd 单元中用 CapabilityBoundingSet= 与 AmbientCapabilities= 收敛")
    }
}
