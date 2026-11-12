//! 进程保护：自身加固状态、受保护进程的存活与 OOM 优先级、systemd 沙箱评分。
//!
//! 「保护进程」在这里分三层理解，代码也按三层组织：
//! 1. 保护**自己**不被 dump / ptrace / 换页（prctl + rlimit，见 `harden.rs`）；
//! 2. 保护**关键业务进程**不被 OOM 杀掉、被谁看护（oom_score_adj / Restart）；
//! 3. 用**内核沙箱**限制进程能做什么（seccomp / CapabilityBoundingSet / ProtectSystem）。

use crate::check::{Check, Ctx};
use crate::config::ProcessConfig;
use crate::model::{Finding, Severity};
use crate::util;
use std::time::Duration;

pub struct ProcessCheck;

impl Check for ProcessCheck {
    fn id(&self) -> &'static str {
        "process"
    }

    fn title(&self) -> &'static str {
        "进程保护"
    }

    fn run(&self, cx: &Ctx<'_>) -> Vec<Finding> {
        let cfg = &cx.cfg.process;
        let mut out = Vec::new();
        if cfg.check_self {
            out.push(check_self(cfg));
        }
        out.push(check_protected(cfg));
        if cfg.check_systemd_unit {
            out.push(check_unit_hardening(cfg));
            out.push(check_watchdog());
        }
        out
    }
}

fn check_self(cfg: &ProcessConfig) -> Finding {
    let f = Finding::new("PROC-01", "本进程已加固", Severity::High);

    let Some(status) = std::fs::read_to_string("/proc/self/status").ok() else {
        return f.skip("/proc/self/status 不可读");
    };
    let field = |key: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix(':')))
            .map(|v| v.trim().to_string())
    };

    let nnp = field("NoNewPrivs").unwrap_or_else(|| "0".into());
    let seccomp = field("Seccomp").unwrap_or_else(|| "0".into());
    let tracer = field("TracerPid").unwrap_or_else(|| "0".into());
    let cap = field("CapEff").unwrap_or_default();

    let mut issues = Vec::new();
    let mut evidence = vec![
        format!("NoNewPrivs = {nnp}"),
        format!("Seccomp = {seccomp} (0=off 1=strict 2=filter)"),
        format!("TracerPid = {tracer}"),
        format!("CapEff = {cap}"),
    ];

    if cfg.require_no_new_privs && nnp == "0" {
        issues.push("未设置 no_new_privs");
    }
    if cfg.require_seccomp && seccomp == "0" {
        issues.push("未加载 seccomp 过滤器");
    }
    if tracer != "0" {
        issues.push("进程正被 ptrace 跟踪");
    }

    if issues.is_empty() {
        f.pass("进程加固项满足策略").evidence_all(evidence)
    } else {
        f.fail(issues.join("；"))
            .evidence_all(std::mem::take(&mut evidence))
            .fix("以 --harden 启动；systemd 单元加 NoNewPrivileges=yes、SystemCallFilter=@system-service")
    }
}

fn check_protected(cfg: &ProcessConfig) -> Finding {
    let f = Finding::new("PROC-02", "关键进程存活且不易被 OOM", Severity::High);

    if cfg.protected.is_empty() {
        return f.skip("策略未配置受保护进程");
    }

    let procs = util::pids();
    let mut issues = Vec::new();
    let mut evidence = Vec::new();

    for want in &cfg.protected {
        let hits: Vec<(u32, String)> = procs
            .iter()
            .filter(|(_, comm)| comm == &want.name)
            .cloned()
            .collect();

        if hits.is_empty() {
            issues.push(format!("{} 未运行", want.name));
            continue;
        }

        for (pid, comm) in &hits {
            let adj = util::read_trim(format!("/proc/{pid}/oom_score_adj"))
                .and_then(|v| v.parse::<i32>().ok());
            match adj {
                Some(a) if a <= want.max_oom_score_adj => {
                    evidence.push(format!("{comm} (pid {pid}) oom_score_adj = {a}"));
                }
                Some(a) => {
                    evidence.push(format!("{comm} (pid {pid}) oom_score_adj = {a}"));
                    issues.push(format!(
                        "{} oom_score_adj={a} 高于阈值 {}",
                        want.name, want.max_oom_score_adj
                    ));
                }
                None => evidence.push(format!("{comm} (pid {pid}) oom_score_adj 不可读")),
            }
        }
    }

    if issues.is_empty() {
        f.pass("受保护进程均存活且 OOM 优先级正确")
            .evidence_all(evidence)
    } else {
        f.fail(issues.join("；"))
            .evidence_all(evidence)
            .fix("在单元里设置 OOMScoreAdjust=-900（越负越不易被杀），并配置 Restart=always")
    }
}

fn current_unit() -> Option<String> {
    let cgroup = util::read_trim("/proc/self/cgroup")?;
    for line in cgroup.lines() {
        let rest = line.split_once("::").map(|(_, r)| r).unwrap_or(line);
        for seg in rest.split('/') {
            // 交互式会话、scope、user@ 不是被守护的服务，沙箱评分对它没意义
            if seg.ends_with(".scope") || seg.starts_with("user@") || seg.starts_with("session-") {
                return None;
            }
            if seg.ends_with(".service") {
                return Some(seg.to_string());
            }
        }
    }
    None
}

/// 读取当前单元的沙箱属性并打分。systemd 的 `ProtectSystem=strict` 等
/// 是「零代码成本」的进程保护，优先于任何自研方案。
fn check_unit_hardening(cfg: &ProcessConfig) -> Finding {
    let f = Finding::new("PROC-03", "systemd 单元沙箱已加固", Severity::Medium);

    let Some(unit) = current_unit() else {
        return f.skip("未运行在 systemd service 单元中（交互式会话不适用）");
    };

    let props = [
        "NoNewPrivileges",
        "ProtectSystem",
        "ProtectHome",
        "PrivateTmp",
        "ProtectKernelTunables",
        "CapabilityBoundingSet",
        "MemoryDenyWriteExecute",
        "RestrictAddressFamilies",
    ];
    let mut args = vec!["show", unit.as_str(), "--no-pager"];
    for p in props {
        args.push("-p");
        args.push(p);
    }

    let Some(out) = util::run("systemctl", &args, Duration::from_secs(10)) else {
        return f.skip("systemctl 不可用");
    };
    if !out.ok() {
        return f.skip(format!("systemctl show 失败: {}", out.stderr.trim()));
    }

    let map: std::collections::BTreeMap<String, String> = out
        .stdout
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            Some((k.to_string(), v.trim().to_string()))
        })
        .collect();

    let mut score = 0usize;
    let mut evidence = Vec::new();

    let check = |key: &str, ok: bool, evidence: &mut Vec<String>, score: &mut usize, note: &str| {
        let val = map.get(key).cloned().unwrap_or_default();
        if ok {
            *score += 1;
            evidence.push(format!("✓ {key}={val}"));
        } else {
            evidence.push(format!("✗ {key}={val} ({note})"));
        }
    };

    check(
        "NoNewPrivileges",
        map.get("NoNewPrivileges")
            .map(|v| v == "yes")
            .unwrap_or(false),
        &mut evidence,
        &mut score,
        "应为 yes",
    );
    check(
        "ProtectSystem",
        matches!(
            map.get("ProtectSystem").map(String::as_str),
            Some("strict") | Some("full")
        ),
        &mut evidence,
        &mut score,
        "应为 strict/full",
    );
    check(
        "ProtectHome",
        matches!(
            map.get("ProtectHome").map(String::as_str),
            Some("yes") | Some("tmpfs")
        ),
        &mut evidence,
        &mut score,
        "应为 yes/tmpfs",
    );
    check(
        "PrivateTmp",
        map.get("PrivateTmp").map(|v| v == "yes").unwrap_or(false),
        &mut evidence,
        &mut score,
        "应为 yes",
    );
    check(
        "ProtectKernelTunables",
        map.get("ProtectKernelTunables")
            .map(|v| v == "yes")
            .unwrap_or(false),
        &mut evidence,
        &mut score,
        "应为 yes",
    );
    // 注意语义：`CapabilityBoundingSet=` 为空表示「一个能力都不给」，是最严格的；
    // 真正的问题是非空但含 cap_sys_admin 或几乎全套能力。
    let caps = map
        .get("CapabilityBoundingSet")
        .cloned()
        .unwrap_or_default();
    let cap_set: Vec<&str> = caps.split_whitespace().collect();
    let cap_ok = cap_set.len() < 30 && !cap_set.contains(&"cap_sys_admin");
    check(
        "CapabilityBoundingSet",
        cap_ok,
        &mut evidence,
        &mut score,
        "应剔除 cap_sys_admin 且只保留必要能力",
    );

    if score >= cfg.min_hardening_score {
        f.pass(format!("{unit} 加固得分 {score}/6"))
            .evidence_all(evidence)
    } else {
        f.warn(format!(
            "{unit} 加固得分 {score}/6，低于策略要求的 {}",
            cfg.min_hardening_score
        ))
        .evidence_all(evidence)
        .fix("参考 deploy/qeaudit.service 的 [Service] 段补全沙箱指令后 daemon-reload")
    }
}

fn check_watchdog() -> Finding {
    let f = Finding::new("PROC-04", "进程有重启与看门狗保障", Severity::Medium);

    let Some(unit) = current_unit() else {
        return f.skip("未运行在 systemd service 单元中（交互式会话不适用）");
    };
    let Some(out) = util::run(
        "systemctl",
        &[
            "show",
            unit.as_str(),
            "--no-pager",
            "-p",
            "Restart",
            "-p",
            "WatchdogUSec",
        ],
        Duration::from_secs(10),
    ) else {
        return f.skip("systemctl 不可用");
    };

    let map: std::collections::BTreeMap<String, String> = out
        .stdout
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            Some((k.to_string(), v.trim().to_string()))
        })
        .collect();

    let restart = map.get("Restart").cloned().unwrap_or_default();
    let watchdog = map
        .get("WatchdogUSec")
        .cloned()
        .unwrap_or_else(|| "0".into());
    let restart_ok = matches!(restart.as_str(), "always" | "on-failure" | "on-abnormal");
    let watchdog_ok = watchdog != "0" && !watchdog.is_empty();

    let evidence = vec![
        format!("Restart = {restart}"),
        format!("WatchdogUSec = {watchdog}"),
    ];

    if restart_ok && watchdog_ok {
        f.pass("重启策略与看门狗均已配置").evidence_all(evidence)
    } else if restart_ok {
        f.warn("有自动重启，但未配置 WatchdogSec")
            .evidence_all(evidence)
            .fix("单元加 WatchdogSec=30s，进程内定期 sd_notify(WATCHDOG=1)")
    } else {
        f.fail("进程挂掉后不会自动拉起")
            .evidence_all(evidence)
            .fix("单元加 Restart=always、RestartSec=5s")
    }
}
