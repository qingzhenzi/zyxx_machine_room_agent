//! 报告渲染：人读表格 + 稳定 JSON。两者都从同一份 `Finding` 派生，不会漂移。

use crate::config::Config;
use crate::model::{Finding, Severity, Status};
use crate::util;
use serde::Serialize;

pub const TOOL: &str = "qeaudit";

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub total: usize,
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
    pub skip: usize,
    /// 所有非 pass 项里的最高风险等级。
    pub worst_severity: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub tool: &'static str,
    pub version: &'static str,
    pub generated_at: String,
    pub generated_at_epoch: i64,
    pub hostname: String,
    pub kernel: String,
    pub profile: String,
    pub asset_id: String,
    pub summary: Summary,
    pub findings: Vec<Finding>,
}

impl Report {
    pub fn build(cfg: &Config, findings: Vec<Finding>) -> Self {
        let count = |s: Status| findings.iter().filter(|f| f.status == s).count();
        let worst = findings
            .iter()
            .filter(|f| f.status.is_bad())
            .map(|f| f.severity)
            .max()
            .unwrap_or(Severity::Info);
        let hostname = util::hostname();
        let asset_id = if cfg.profile.asset_id.is_empty() {
            hostname.clone()
        } else {
            cfg.profile.asset_id.clone()
        };

        Self {
            tool: TOOL,
            version: env!("CARGO_PKG_VERSION"),
            generated_at: util::format_utc(util::now_secs()),
            generated_at_epoch: util::now_secs(),
            hostname,
            kernel: util::uname_release(),
            profile: cfg.profile.name.clone(),
            asset_id,
            summary: Summary {
                total: findings.len(),
                pass: count(Status::Pass),
                warn: count(Status::Warn),
                fail: count(Status::Fail),
                skip: count(Status::Skip),
                worst_severity: worst.as_str().to_string(),
            },
            findings,
        }
    }

    /// 人读输出：先一张总表，再展开有问题的项。
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "{} {}  profile={}  host={}  kernel={}\n",
            self.tool, self.version, self.profile, self.hostname, self.kernel
        ));
        out.push_str(&format!(
            "生成时间 {}   结果: {} 通过 / {} 告警 / {} 失败 / {} 跳过\n\n",
            self.generated_at,
            self.summary.pass,
            self.summary.warn,
            self.summary.fail,
            self.summary.skip
        ));

        let w_id = self
            .findings
            .iter()
            .map(|f| f.id.len())
            .max()
            .unwrap_or(8)
            .max(8);
        let w_title = self
            .findings
            .iter()
            .map(|f| f.title.chars().count())
            .max()
            .unwrap_or(20);

        out.push_str(&format!(
            "{:<w_id$}  {:<6}  {:<8}  {:<w_title$}\n",
            "ID",
            "STATUS",
            "SEVERITY",
            "TITLE",
            w_id = w_id,
            w_title = w_title
        ));
        out.push_str(&format!(
            "{:-<w_id$}  {:-<6}  {:-<8}  {:-<w_title$}\n",
            "",
            "",
            "",
            "",
            w_id = w_id,
            w_title = w_title
        ));

        for f in &self.findings {
            out.push_str(&format!(
                "{:<w_id$}  {:<6}  {:<8}  {:<w_title$}\n",
                f.id,
                f.status.as_str().to_uppercase(),
                f.severity.as_str(),
                f.title,
                w_id = w_id,
                w_title = w_title
            ));
        }

        let problems: Vec<&Finding> = self.findings.iter().filter(|f| f.status.is_bad()).collect();
        if !problems.is_empty() {
            out.push_str("\n问题详情\n");
            for f in problems {
                out.push_str(&format!(
                    "\n[{}] {} {}\n    {}",
                    f.status.as_str().to_uppercase(),
                    f.id,
                    f.title,
                    f.detail
                ));
                for e in &f.evidence {
                    out.push_str(&format!("\n      - {e}"));
                }
                if let Some(fix) = &f.remediation {
                    out.push_str(&format!("\n      修复: {fix}"));
                }
                out.push('\n');
            }
        }

        let skipped = self.summary.skip;
        if skipped > 0 {
            out.push_str(&format!("\n{skipped} 项因环境/权限未判定，属正常降级。\n"));
        }
        out
    }
}

/// 进程退出码：0 = 干净，1 = 仅告警，2 = 存在失败项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum FailOn {
    /// 有 fail 即返回 2，有 warn 返回 1。
    Fail,
    /// warn 或 fail 都返回 2。
    Warn,
    /// 恒返回 0（只出报告，不卡流水线）。
    Never,
}

impl Report {
    pub fn exit_code(&self, fail_on: FailOn) -> u8 {
        match fail_on {
            FailOn::Never => 0,
            FailOn::Warn => {
                if self.summary.fail + self.summary.warn > 0 {
                    2
                } else {
                    0
                }
            }
            FailOn::Fail => {
                if self.summary.fail > 0 {
                    2
                } else if self.summary.warn > 0 {
                    1
                } else {
                    0
                }
            }
        }
    }
}
