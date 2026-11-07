//! 阶段 4：系统环境与虚拟化检查。
//!
//! 聚合三个既有 crate 的结果，产出可经 IPC 查询的检查摘要：
//! * `platform-health` — 设备事实快照 + 健康评分
//! * `qeaudit`         — 安全基线扫描
//! * `platform-diagnose` — 短窗采样诊断（Sweep，默认 3s 窗 / 500ms 间隔）

use serde_json::{Value, json};

/// 一次全量检查的结果。
#[derive(Debug, Clone)]
pub struct CheckOutcome {
    pub passed: bool,
    pub summary: String,
    pub detail: Value,
}

/// 执行全量检查（同步、一次性；周期性调度由调用方负责）。
pub fn run_full_checks() -> CheckOutcome {
    let mut issues: Vec<String> = Vec::new();

    // 1) platform-health：收集失败也不 panic，按设计降级。
    let snap = platform_health::snapshot();
    let health = &snap.health;
    if !health.is_healthy() {
        issues.push(format!("健康状态: {:?}", health.status));
    }

    // 2) qeaudit：安全基线。
    let audit_cfg = qeaudit::Config::default();
    let findings = qeaudit::scan(&audit_cfg, &qeaudit::ScanOptions::all());
    let high = findings
        .iter()
        .filter(|finding| finding.severity >= qeaudit::Severity::High)
        .count();
    if high > 0 {
        issues.push(format!("{high} 项高危基线问题"));
    }

    // 3) platform-diagnose：短窗采样 + 诊断（Lag 综合症状覆盖 CPU/内存/IO/温控规则）。
    let mut diagnoser = platform_diagnose::Diagnoser::new(platform_diagnose::Options::default());
    let diagnosis = diagnoser.sweep(
        &platform_diagnose::Symptom::Lag,
        std::time::Duration::from_secs(3),
        std::time::Duration::from_millis(500),
    );
    if !diagnosis.findings.is_empty() {
        issues.push(format!("诊断 {} 项发现", diagnosis.findings.len()));
    }

    // 4) 虚拟化能力探测（KVM 设备可读写）。
    let kvm = kvm_available();

    let passed = issues.is_empty();
    CheckOutcome {
        passed,
        summary: if passed {
            "全部检查通过".into()
        } else {
            issues.join("；")
        },
        detail: json!({
            "health": {
                "healthy": health.is_healthy(),
                "score": health.score,
            },
            "audit": {
                "findings": findings.len(),
                "high": high,
            },
            "diagnose": {
                "verdict": diagnosis.summary,
                "samples": diagnosis.samples,
                "findings": diagnosis.findings.len(),
            },
            "virtualization": kvm,
        }),
    }
}

/// KVM 可用性：/dev/kvm 存在且可读写。
pub fn kvm_available() -> bool {
    #[cfg(unix)]
    {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/kvm")
            .is_ok()
    }
    #[cfg(not(unix))]
    {
        false
    }
}
