//! 核查结果的数据模型：`Severity` / `Status` / `Finding`。
//!
//! 这里刻意不引入业务逻辑，只保证「一个检查项 = 一条 Finding」，
//! 便于后面同时输出人读表格和机器读 JSON。

use serde::{Deserialize, Serialize};
use std::fmt;

/// 风险等级。顺序即比较顺序，`Critical` 最大。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Low,
    #[default]
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 单项结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// 符合基线。
    Pass,
    /// 不符合但不致命，建议修复。
    Warn,
    /// 不符合，需要处置。
    Fail,
    /// 前置条件不满足（无权限、平台不支持、组件未安装），本项未判定。
    Skip,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Warn => "warn",
            Status::Fail => "fail",
            Status::Skip => "skip",
        }
    }

    pub fn is_bad(self) -> bool {
        matches!(self, Status::Fail | Status::Warn)
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一条核查结论。
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// 稳定 ID，例如 `GRUB-02`。报告、工单、白名单都靠它对齐。
    pub id: String,
    pub title: String,
    pub status: Status,
    pub severity: Severity,
    /// 一句话结论，含实测值。
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// 支撑结论的原始证据（逐条）。排障时直接看这里。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// 修复建议，尽量给出可直接执行的命令。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    /// 对应的基线条款，例如 `CIS 1.4.2`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

impl Finding {
    pub fn new(id: impl Into<String>, title: impl Into<String>, severity: Severity) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            status: Status::Skip,
            severity,
            detail: String::new(),
            evidence: Vec::new(),
            remediation: None,
            reference: None,
        }
    }

    pub fn status(mut self, status: Status, detail: impl Into<String>) -> Self {
        self.status = status;
        self.detail = detail.into();
        self
    }

    pub fn pass(self, detail: impl Into<String>) -> Self {
        self.status(Status::Pass, detail)
    }

    pub fn warn(self, detail: impl Into<String>) -> Self {
        self.status(Status::Warn, detail)
    }

    pub fn fail(self, detail: impl Into<String>) -> Self {
        self.status(Status::Fail, detail)
    }

    pub fn skip(self, detail: impl Into<String>) -> Self {
        self.status(Status::Skip, detail)
    }

    pub fn evidence(mut self, line: impl Into<String>) -> Self {
        self.evidence.push(line.into());
        self
    }

    pub fn evidence_all<I, S>(mut self, lines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.evidence.extend(lines.into_iter().map(Into::into));
        self
    }

    pub fn fix(mut self, cmd: impl Into<String>) -> Self {
        self.remediation = Some(cmd.into());
        self
    }

    pub fn reference(mut self, r: impl Into<String>) -> Self {
        self.reference = Some(r.into());
        self
    }
}
