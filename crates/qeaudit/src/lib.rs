//! qeaudit —— Linux 主机安全基线核查器（库 + CLI）。
//!
//! 设计要点：
//! - 纯用户态，不依赖内核模块；所有判据来自 /proc、/sys、/etc 与少量只读命令；
//! - 每个检查项是独立 `Check`，单个检查 panic 不会拖垮整轮扫描；
//! - 策略与代码分离，阈值走 TOML；
//! - 输出同时面向人和流水线（表格 / JSON / 退出码）。
//!
//! # 库用法（守护进程进程内调用）
//!
//! `qe-agent` / `qe-agent-telemetry` 直接链接本 crate 采集安全遥测，不再 spawn 外部进程：
//!
//! ```no_run
//! use qeaudit::{Config, ScanOptions, scan_json};
//!
//! let cfg = Config::default();
//! let json = scan_json(&cfg, &ScanOptions::default()).expect("序列化");
//! ```
// 注意：util.rs 的 libc 时间格式化需要少量 unsafe，这里不能 `deny(unsafe_code)`。

pub mod check;
pub mod config;
pub mod harden;
pub mod model;
pub mod report;
pub mod util;

mod checks;

pub use config::Config;
pub use model::{Finding, Severity, Status};
pub use report::{FailOn, Report};

/// 库版本（与 CLI 同源）。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 一轮扫描的执行选项。
#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    /// 只跑指定检查器（`grub,kernel,virt,space,device,network,root,process`）；空 = 全部。
    pub only: Vec<String>,
}

impl ScanOptions {
    /// 跑全部检查器。
    pub fn all() -> Self {
        Self::default()
    }

    /// 只跑指定 id 的检查器。
    pub fn only(ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            only: ids.into_iter().map(Into::into).collect(),
        }
    }
}

/// 执行一轮核查，返回全部 [`Finding`]。
///
/// 与 CLI 行为一致：单个检查器 panic 会被隔离为一条 `Skip`，不影响其余检查。
pub fn scan(cfg: &Config, opts: &ScanOptions) -> Vec<Finding> {
    let cx = check::Ctx { cfg };
    let mut findings = Vec::new();

    for checker in check::registry::all() {
        if !opts.only.is_empty() && !opts.only.iter().any(|o| o == checker.id()) {
            continue;
        }
        // 单个检查器出 bug 时降级为 Skip，而不是整轮失败。
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| checker.run(&cx)));
        match result {
            Ok(mut fs) => findings.append(&mut fs),
            Err(_) => {
                eprintln!("检查器 {} 执行异常，已跳过", checker.id());
                findings.push(
                    Finding::new(
                        format!("{}-ERR", checker.id().to_uppercase()),
                        format!("{} 执行异常", checker.title()),
                        Severity::Low,
                    )
                    .skip("检查器 panic，已隔离"),
                );
            }
        }
    }
    findings
}

/// 执行一轮核查并组装完整 [`Report`]（含 summary 与主机元信息）。
pub fn scan_report(cfg: &Config, opts: &ScanOptions) -> Report {
    Report::build(cfg, scan(cfg, opts))
}

/// 执行一轮核查并序列化为 pretty JSON（与 CLI `scan --format json` 同构）。
pub fn scan_json(cfg: &Config, opts: &ScanOptions) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&scan_report(cfg, opts)).map(|s| s + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_runs_all_checks_isolated() {
        let cfg = Config::default();
        let findings = scan(&cfg, &ScanOptions::all());
        assert!(!findings.is_empty(), "应至少产出内置检查项");
    }

    #[test]
    fn scan_only_filters_checkers() {
        let cfg = Config::default();
        let findings = scan(&cfg, &ScanOptions::only(["kernel"]));
        assert!(
            findings.iter().all(|f| f.id.starts_with("KERN")),
            "只应包含 kernel 检查项: {:?}",
            findings.iter().map(|f| &f.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn scan_json_is_valid() {
        let cfg = Config::default();
        let json = scan_json(&cfg, &ScanOptions::all()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["tool"], "qeaudit");
        assert!(value.get("summary").is_some());
    }
}
