//! 设备空间：容量、inode、以及挂载选项带来的「空间类」风险
//! （世界可写目录、tmp 上可执行落盘）。
//!
//! 用 `statvfs` 而不是解析 `df`：`df` 输出受 locale 和列宽影响，且要多起一个进程。

use crate::check::{Check, Ctx};
use crate::model::{Finding, Severity, Status};
use crate::util;

pub struct SpaceCheck;

impl Check for SpaceCheck {
    fn id(&self) -> &'static str {
        "space"
    }

    fn title(&self) -> &'static str {
        "磁盘容量与挂载加固"
    }

    fn run(&self, cx: &Ctx<'_>) -> Vec<Finding> {
        let cfg = &cx.cfg.space;
        vec![
            check_capacity(cfg),
            check_inodes(cfg),
            check_mount_options(cfg),
            check_world_writable(cfg),
        ]
    }
}

fn check_capacity(cfg: &crate::config::SpaceConfig) -> Finding {
    let f =
        Finding::new("SPACE-01", "文件系统可用空间充足", Severity::High).reference("等保 8.1.4.2");

    if cfg.mounts.is_empty() {
        return f.skip("策略未配置挂载点");
    }

    let mut worst = Status::Pass;
    let mut evidence = Vec::new();
    let mut issues = Vec::new();

    for rule in &cfg.mounts {
        let Some(u) = util::disk_usage(&rule.path) else {
            worst = Status::Warn;
            issues.push(format!("{} 无法 statvfs（路径不存在？）", rule.path));
            continue;
        };
        let free_pct = util::pct(u.avail, u.total);
        evidence.push(format!(
            "{}: 可用 {} / 共 {} ({free_pct:.1}%，含保留块 {} 空闲)",
            rule.path,
            util::human_bytes(u.avail),
            util::human_bytes(u.total),
            util::human_bytes(u.free)
        ));

        if free_pct < rule.min_free_percent as f64 {
            worst = Status::Fail;
            issues.push(format!(
                "{} 可用空间 {free_pct:.1}% < 阈值 {}%",
                rule.path, rule.min_free_percent
            ));
        }
        if rule.min_free_bytes > 0 && u.avail < rule.min_free_bytes {
            worst = Status::Fail;
            issues.push(format!(
                "{} 可用 {} < 阈值 {}",
                rule.path,
                util::human_bytes(u.avail),
                util::human_bytes(rule.min_free_bytes)
            ));
        }
    }

    if issues.is_empty() {
        f.pass("全部挂载点空间充足").evidence_all(evidence)
    } else {
        f.status(worst, issues.join("；"))
            .evidence_all(evidence)
            .fix("清理 /var/log、/var/cache 与容器镜像；必要时扩 LVM/云盘")
    }
}

fn check_inodes(cfg: &crate::config::SpaceConfig) -> Finding {
    let f = Finding::new("SPACE-02", "inode 使用率正常", Severity::Medium);

    let mut worst = Status::Pass;
    let mut evidence = Vec::new();
    let mut issues = Vec::new();

    for rule in &cfg.mounts {
        let Some(u) = util::disk_usage(&rule.path) else {
            continue;
        };
        if u.inodes_total == 0 {
            continue; // btrfs 等动态 inode 文件系统
        }
        let used_pct = util::pct(u.inodes_total - u.inodes_free, u.inodes_total);
        evidence.push(format!(
            "{}: inode 已用 {used_pct:.1}% ({} / {})",
            rule.path,
            u.inodes_total - u.inodes_free,
            u.inodes_total
        ));
        if used_pct > rule.max_inode_used_percent as f64 {
            worst = Status::Fail;
            issues.push(format!(
                "{} inode 已用 {used_pct:.1}% > {}%",
                rule.path, rule.max_inode_used_percent
            ));
        }
    }

    if evidence.is_empty() {
        return f.skip("无可用 inode 统计");
    }
    if issues.is_empty() {
        f.pass("inode 使用率正常").evidence_all(evidence)
    } else {
        f.status(worst, issues.join("；")).evidence_all(evidence).fix(
            "定位小文件密集目录：find / -xdev -type f | sed 's|/[^/]*$||' | sort | uniq -c | sort -rn | head",
        )
    }
}

fn check_mount_options(cfg: &crate::config::SpaceConfig) -> Finding {
    let f =
        Finding::new("SPACE-03", "临时目录挂载已加固", Severity::High).reference("CIS 1.1.2-1.1.9");

    if !cfg.check_mount_options {
        return f.skip("策略关闭该项");
    }

    let mounts = util::mounts();
    let mut evidence = Vec::new();
    let mut issues = Vec::new();

    for target in &cfg.restricted_mounts {
        let Some(m) = mounts.iter().find(|m| &m.target == target) else {
            evidence.push(format!("{target}: 未单独挂载（tmpfs 外的默认目录）"));
            continue;
        };
        let missing: Vec<&str> = ["nosuid", "nodev", "noexec"]
            .into_iter()
            .filter(|opt| !m.has_opt(opt))
            .collect();
        evidence.push(format!(
            "{target} ({}, {}): {}",
            m.source,
            m.fstype,
            m.options.join(",")
        ));
        if !missing.is_empty() {
            issues.push(format!("{target} 缺少 {}", missing.join(",")));
        }
    }

    if issues.is_empty() {
        f.pass("临时目录 nosuid/nodev/noexec 齐备")
            .evidence_all(evidence)
    } else {
        f.fail("临时目录可被用于提权或落盘执行")
            .evidence_all(issues)
            .evidence_all(evidence)
            .fix("在 /etc/fstab 对应行加 nosuid,nodev,noexec 后 mount -o remount")
    }
}

/// 世界可写且无 sticky 位的目录：任何用户可删改他人文件，是典型的提权跳板。
fn check_world_writable(cfg: &crate::config::SpaceConfig) -> Finding {
    let f = Finding::new("SPACE-04", "无世界可写且无 sticky 的目录", Severity::High)
        .reference("CIS 1.1.21");

    let mut bad = Vec::new();
    let mut scanned = 0usize;

    for dir in &cfg.restricted_mounts {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        scanned += 1;
        for entry in rd.filter_map(|e| e.ok()).take(512) {
            let Ok(md) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if !md.is_dir() {
                continue;
            }
            use std::os::unix::fs::PermissionsExt;
            let mode = md.permissions().mode();
            let world_writable = mode & 0o002 != 0;
            let sticky = mode & 0o1000 != 0;
            if world_writable && !sticky {
                bad.push(format!("{} 权限 {mode:04o}", entry.path().display()));
            }
        }
    }

    if scanned == 0 {
        return f.skip("受限目录均不可读");
    }
    if bad.is_empty() {
        f.pass("未发现危险的世界可写目录")
    } else {
        f.fail(format!("发现 {} 个世界可写且无 sticky 的目录", bad.len()))
            .evidence_all(bad.into_iter().take(20))
            .fix("chmod +t <dir>（确需共享）或 chmod o-w <dir>")
    }
}
