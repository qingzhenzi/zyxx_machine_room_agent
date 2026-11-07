//! 健康结论：优先采纳 platform-health，缺失时用内置阈值兜底。
//!
//! 契约（`docs/machine-telemetry.md` §2）要求 `health.status` 必填、
//! `score`（0–100）与 `issues[]` 可选。本模块提供两条路径：
//!
//! 1. [`adopt_platform_health`]：platform-health JSON 里已有 `health.status` 时
//!    原样采纳其 status/score/issues；
//! 2. [`evaluate_builtin`]：没有 platform-health 时按内置阈值（CPU/内存/磁盘/
//!    温度/每核 load）计算，阈值见下方常量。
//!
//! 内置评分规则：初始 100 分，warning 扣 10、critical 扣 30、info 不扣，
//! 下限 0；任一 critical → `critical`，否则任一 warning → `warning`，否则 `ok`。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// 健康状态（序列化为契约要求的 `ok` / `warning` / `critical`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    /// 正常。
    Ok,
    /// 需要关注。
    Warning,
    /// 需要处理。
    Critical,
}

impl HealthStatus {
    /// 字符串形式（与序列化一致）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    /// 解析平台返回的状态字符串（大小写不敏感）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "ok" | "healthy" | "pass" => Some(Self::Ok),
            "warning" | "warn" | "degraded" => Some(Self::Warning),
            "critical" | "fail" | "failed" => Some(Self::Critical),
            _ => None,
        }
    }
}

/// 单条问题的严重级别（契约 `health.issues[].severity`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// 提示。
    Info,
    /// 警告。
    Warning,
    /// 严重。
    Critical,
}

impl Severity {
    /// 解析严重级别字符串；无法识别时返回 None。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "info" | "low" => Some(Self::Info),
            "warning" | "warn" | "medium" | "high" => Some(Self::Warning),
            "critical" | "crit" | "fatal" => Some(Self::Critical),
            _ => None,
        }
    }

    fn penalty(self) -> u32 {
        match self {
            Self::Info => 0,
            Self::Warning => 10,
            Self::Critical => 30,
        }
    }
}

/// 一条健康问题。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    /// 严重级别。
    pub severity: Severity,
    /// 机器可读的问题码，如 `disk.usage`。
    pub code: String,
    /// 人类可读摘要。
    pub message: String,
    /// 观测值/阈值等附加信息。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// 健康结论。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    /// 状态（必填）。
    pub status: HealthStatus,
    /// 0–100 分。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<u32>,
    /// 问题列表；无问题时为空数组。
    pub issues: Vec<Issue>,
}

impl Health {
    fn from_issues(status: HealthStatus, issues: Vec<Issue>) -> Self {
        Self {
            status,
            score: Some(score_from_issues(&issues)),
            issues,
        }
    }
}

/// 内置阈值：CPU 使用率警告线（%）。
pub const CPU_WARNING_PERCENT: f64 = 90.0;
/// 内置阈值：CPU 使用率严重线（%）。
pub const CPU_CRITICAL_PERCENT: f64 = 97.0;
/// 内置阈值：内存使用率警告线（%）。
pub const MEMORY_WARNING_PERCENT: f64 = 90.0;
/// 内置阈值：内存使用率严重线（%）。
pub const MEMORY_CRITICAL_PERCENT: f64 = 97.0;
/// 内置阈值：代表分区使用率警告线（%）。
pub const DISK_WARNING_PERCENT: f64 = 90.0;
/// 内置阈值：代表分区使用率严重线（%）。
pub const DISK_CRITICAL_PERCENT: f64 = 95.0;
/// 内置阈值：最高温度警告线（°C）。
pub const TEMPERATURE_WARNING_C: f64 = 85.0;
/// 内置阈值：最高温度严重线（°C）。
pub const TEMPERATURE_CRITICAL_C: f64 = 95.0;
/// 内置阈值：每核 1 分钟 load 警告线。
pub const LOAD_WARNING_PER_CORE: f64 = 1.5;
/// 内置阈值：每核 1 分钟 load 严重线。
pub const LOAD_CRITICAL_PER_CORE: f64 = 3.0;

/// 内置健康评估的输入信号；缺失的项跳过对应检查。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BuiltinSignals {
    /// 总 CPU 使用率（%）。
    pub cpu_usage_percent: Option<f64>,
    /// 内存使用率（%）。
    pub memory_usage_percent: Option<f64>,
    /// 代表分区（优先根分区）使用率（%）。
    pub disk_usage_percent: Option<f64>,
    /// 最高传感器温度（°C）。
    pub max_temperature_c: Option<f64>,
    /// 1 分钟平均负载。
    pub load1: Option<f64>,
    /// 逻辑核心数（用于 load 归一化）。
    pub logical_cores: Option<u32>,
}

/// 按内置阈值计算健康结论。
pub fn evaluate_builtin(signals: &BuiltinSignals) -> Health {
    let mut issues = Vec::new();

    if let Some(usage) = signals.cpu_usage_percent {
        push_issue(
            &mut issues,
            "cpu.usage",
            "CPU usage",
            usage,
            CPU_WARNING_PERCENT,
            CPU_CRITICAL_PERCENT,
            "%",
        );
    }
    if let Some(usage) = signals.memory_usage_percent {
        push_issue(
            &mut issues,
            "memory.usage",
            "Memory usage",
            usage,
            MEMORY_WARNING_PERCENT,
            MEMORY_CRITICAL_PERCENT,
            "%",
        );
    }
    if let Some(usage) = signals.disk_usage_percent {
        push_issue(
            &mut issues,
            "disk.usage",
            "Disk / usage",
            usage,
            DISK_WARNING_PERCENT,
            DISK_CRITICAL_PERCENT,
            "%",
        );
    }
    if let Some(temperature) = signals.max_temperature_c {
        push_issue(
            &mut issues,
            "thermal.temperature",
            "Hottest sensor",
            temperature,
            TEMPERATURE_WARNING_C,
            TEMPERATURE_CRITICAL_C,
            "°C",
        );
    }
    if let (Some(load1), Some(cores)) = (signals.load1, signals.logical_cores)
        && cores > 0
    {
        let per_core = load1 / f64::from(cores);
        if let Some(severity) =
            severity_for(per_core, LOAD_WARNING_PER_CORE, LOAD_CRITICAL_PER_CORE)
        {
            issues.push(Issue {
                    severity,
                    code: "load.average".to_owned(),
                    message: format!("Load per core at {per_core:.2} (1m load {load1:.2}, {cores} cores)"),
                    detail: Some(format!(
                        "observed={per_core:.2} warning>{LOAD_WARNING_PER_CORE} critical>{LOAD_CRITICAL_PER_CORE}"
                    )),
                });
        }
    }

    let status = overall_status(&issues);
    Health::from_issues(status, issues)
}

fn push_issue(
    issues: &mut Vec<Issue>,
    code: &str,
    label: &str,
    observed: f64,
    warning: f64,
    critical: f64,
    unit: &str,
) {
    if let Some(severity) = severity_for(observed, warning, critical) {
        issues.push(Issue {
            severity,
            code: code.to_owned(),
            message: format!("{label} at {observed:.1}{unit}"),
            detail: Some(format!(
                "observed={observed:.1}{unit} warning>{warning}{unit} critical>{critical}{unit}"
            )),
        });
    }
}

fn severity_for(observed: f64, warning: f64, critical: f64) -> Option<Severity> {
    if observed >= critical {
        Some(Severity::Critical)
    } else if observed >= warning {
        Some(Severity::Warning)
    } else {
        None
    }
}

fn overall_status(issues: &[Issue]) -> HealthStatus {
    if issues
        .iter()
        .any(|issue| issue.severity == Severity::Critical)
    {
        HealthStatus::Critical
    } else if issues
        .iter()
        .any(|issue| issue.severity == Severity::Warning)
    {
        HealthStatus::Warning
    } else {
        HealthStatus::Ok
    }
}

fn score_from_issues(issues: &[Issue]) -> u32 {
    let penalty: u32 = issues.iter().map(|issue| issue.severity.penalty()).sum();
    100u32.saturating_sub(penalty)
}

/// 尝试从 platform-health JSON 的 `health` 字段直接采纳结论。
///
/// 返回 None 表示没有可用的平台健康结论（缺 `health`、状态无法识别），
/// 调用方应回退到 [`evaluate_builtin`]。
pub fn adopt_platform_health(value: &Value) -> Option<Health> {
    let health = value.get("health")?.as_object()?;
    let status = health
        .get("status")
        .and_then(Value::as_str)
        .and_then(HealthStatus::parse)?;
    let reported_score = health
        .get("score")
        .and_then(Value::as_u64)
        .and_then(|score| u32::try_from(score).ok())
        .map(|score| score.min(100));
    let issues = health
        .get("issues")
        .and_then(Value::as_array)
        .map(|array| array.iter().filter_map(platform_issue).collect::<Vec<_>>())
        .unwrap_or_default();
    let score = reported_score.or_else(|| Some(score_from_issues(&issues)));
    Some(Health {
        status,
        score,
        issues,
    })
}

/// 把 platform-health 的单条 issue 转成契约形状。
fn platform_issue(value: &Value) -> Option<Issue> {
    let object = value.as_object()?;
    // 无法识别的级别按 warning 处理，至少不会把问题静默降级。
    let severity = object
        .get("severity")
        .and_then(Value::as_str)
        .and_then(Severity::parse)
        .unwrap_or(Severity::Warning);
    let code = object
        .get("code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .unwrap_or("platform_health.issue")
        .to_owned();
    let message = object
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| object.get("summary").and_then(Value::as_str))
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| code.clone());
    Some(Issue {
        severity,
        code,
        message,
        detail: issue_detail(object),
    })
}

fn issue_detail(object: &Map<String, Value>) -> Option<String> {
    if let Some(detail) = object
        .get("detail")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
    {
        return Some(detail.to_owned());
    }
    let mut parts = Vec::new();
    for key in ["subject", "observed", "threshold", "unit"] {
        let Some(value) = object.get(key) else {
            continue;
        };
        let rendered = match value {
            Value::String(text) if !text.trim().is_empty() => text.trim().to_owned(),
            Value::Number(number) => number.to_string(),
            _ => continue,
        };
        parts.push(format!("{key}={rendered}"));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// 便于测试/日志的 JSON 视图。
pub fn health_json(health: &Health) -> Value {
    json!(health)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signals() -> BuiltinSignals {
        BuiltinSignals {
            cpu_usage_percent: Some(10.0),
            memory_usage_percent: Some(40.0),
            disk_usage_percent: Some(50.0),
            max_temperature_c: Some(60.0),
            load1: Some(1.0),
            logical_cores: Some(8),
        }
    }

    fn codes(health: &Health) -> Vec<&str> {
        health
            .issues
            .iter()
            .map(|issue| issue.code.as_str())
            .collect()
    }

    #[test]
    fn healthy_signals_yield_ok_and_full_score() {
        let health = evaluate_builtin(&signals());
        assert_eq!(health.status, HealthStatus::Ok);
        assert_eq!(health.score, Some(100));
        assert!(health.issues.is_empty());
    }

    #[test]
    fn cpu_thresholds_are_applied() {
        let mut input = signals();
        input.cpu_usage_percent = Some(92.0);
        let health = evaluate_builtin(&input);
        assert_eq!(health.status, HealthStatus::Warning);
        assert_eq!(health.score, Some(90));
        assert_eq!(codes(&health), vec!["cpu.usage"]);

        input.cpu_usage_percent = Some(98.0);
        let health = evaluate_builtin(&input);
        assert_eq!(health.status, HealthStatus::Critical);
        assert_eq!(health.score, Some(70));
        assert_eq!(health.issues[0].severity, Severity::Critical);
    }

    #[test]
    fn memory_thresholds_are_applied() {
        let mut input = signals();
        input.memory_usage_percent = Some(95.0);
        let health = evaluate_builtin(&input);
        assert_eq!(health.status, HealthStatus::Warning);
        assert!(codes(&health).contains(&"memory.usage"));

        input.memory_usage_percent = Some(97.5);
        let health = evaluate_builtin(&input);
        assert_eq!(health.status, HealthStatus::Critical);
    }

    #[test]
    fn disk_thresholds_are_applied() {
        let mut input = signals();
        input.disk_usage_percent = Some(90.0);
        assert_eq!(evaluate_builtin(&input).status, HealthStatus::Warning);
        input.disk_usage_percent = Some(95.0);
        assert_eq!(evaluate_builtin(&input).status, HealthStatus::Critical);
        input.disk_usage_percent = Some(89.9);
        assert!(
            evaluate_builtin(&input)
                .issues
                .iter()
                .all(|issue| issue.code != "disk.usage")
        );
    }

    #[test]
    fn temperature_thresholds_are_applied() {
        let mut input = signals();
        input.max_temperature_c = Some(86.85);
        let health = evaluate_builtin(&input);
        assert_eq!(health.status, HealthStatus::Warning);
        assert_eq!(codes(&health), vec!["thermal.temperature"]);

        input.max_temperature_c = Some(96.0);
        assert_eq!(evaluate_builtin(&input).status, HealthStatus::Critical);
    }

    #[test]
    fn load_is_normalized_by_core_count() {
        let mut input = signals();
        input.logical_cores = Some(4);
        input.load1 = Some(6.0);
        let health = evaluate_builtin(&input);
        assert_eq!(health.issues[0].code, "load.average");
        assert_eq!(health.issues[0].severity, Severity::Warning);

        input.load1 = Some(13.0);
        assert_eq!(evaluate_builtin(&input).status, HealthStatus::Critical);

        // 没有核心数时跳过 load 检查，而不是误判。
        input.logical_cores = None;
        assert!(evaluate_builtin(&input).issues.is_empty());
    }

    #[test]
    fn missing_signals_are_skipped() {
        let health = evaluate_builtin(&BuiltinSignals::default());
        assert_eq!(health.status, HealthStatus::Ok);
        assert_eq!(health.score, Some(100));
        assert!(health.issues.is_empty());
    }

    #[test]
    fn score_is_clamped_and_status_escalates() {
        let input = BuiltinSignals {
            cpu_usage_percent: Some(99.0),
            memory_usage_percent: Some(99.0),
            disk_usage_percent: Some(99.0),
            max_temperature_c: Some(99.0),
            load1: Some(99.0),
            logical_cores: Some(1),
        };
        let health = evaluate_builtin(&input);
        assert_eq!(health.status, HealthStatus::Critical);
        assert_eq!(health.issues.len(), 5);
        assert_eq!(health.score, Some(0)); // 5 × 30 扣分，饱和到 0
    }

    #[test]
    fn adopts_platform_health_verbatim() {
        let value = json!({
            "schema": "platform-health/v2",
            "health": {
                "status": "critical",
                "score": 28,
                "issues": [
                    {
                        "code": "disk.usage",
                        "severity": "critical",
                        "subject": "/",
                        "message": "Disk / usage at 98%",
                        "observed": 97.5,
                        "threshold": 95.0,
                        "unit": "%"
                    },
                    {
                        "code": "thermal.temperature",
                        "severity": "warning",
                        "message": "k10temp Tctl at 82°C"
                    }
                ]
            }
        });
        let health = adopt_platform_health(&value).unwrap();
        assert_eq!(health.status, HealthStatus::Critical);
        assert_eq!(health.score, Some(28));
        assert_eq!(health.issues.len(), 2);
        assert_eq!(health.issues[0].code, "disk.usage");
        assert_eq!(health.issues[0].severity, Severity::Critical);
        assert_eq!(
            health.issues[0].detail.as_deref(),
            Some("subject=/ observed=97.5 threshold=95.0 unit=%")
        );
        assert_eq!(health.issues[1].severity, Severity::Warning);
        assert!(health.issues[1].detail.is_none());
    }

    #[test]
    fn platform_health_without_health_falls_back() {
        assert!(adopt_platform_health(&json!({"schema": "platform-health/v2"})).is_none());
        assert!(adopt_platform_health(&json!({"health": {"status": "bogus"}})).is_none());
        assert!(adopt_platform_health(&json!("nope")).is_none());
    }

    #[test]
    fn platform_health_score_is_clamped_and_derived_when_missing() {
        let value = json!({"health": {"status": "warning", "score": 250}});
        assert_eq!(adopt_platform_health(&value).unwrap().score, Some(100));

        let value = json!({"health": {"status": "warning", "issues": [
            {"code": "a", "severity": "warning", "message": "a"},
            {"code": "b", "severity": "critical", "message": "b"}
        ]}});
        assert_eq!(adopt_platform_health(&value).unwrap().score, Some(60));
    }

    #[test]
    fn platform_health_accepts_status_aliases() {
        for (raw, expected) in [
            ("ok", HealthStatus::Ok),
            ("healthy", HealthStatus::Ok),
            ("warn", HealthStatus::Warning),
            ("degraded", HealthStatus::Warning),
            ("fail", HealthStatus::Critical),
        ] {
            let value = json!({"health": {"status": raw}});
            assert_eq!(
                adopt_platform_health(&value).unwrap().status,
                expected,
                "{raw}"
            );
        }
    }

    #[test]
    fn health_serializes_contract_shape() {
        let health = evaluate_builtin(&signals());
        let value = health_json(&health);
        assert_eq!(value["status"], json!("ok"));
        assert_eq!(value["score"], json!(100));
        assert_eq!(value["issues"], json!([]));

        let mut input = signals();
        input.cpu_usage_percent = Some(92.0);
        let value = serde_json::to_value(evaluate_builtin(&input)).unwrap();
        assert_eq!(value["status"], json!("warning"));
        assert_eq!(value["issues"][0]["severity"], json!("warning"));
        assert_eq!(value["issues"][0]["code"], json!("cpu.usage"));
        assert!(value["issues"][0]["message"].is_string());
        assert!(value["issues"][0]["detail"].is_string());
    }
}
