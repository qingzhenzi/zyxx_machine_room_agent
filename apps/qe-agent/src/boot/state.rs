//! 流水线共享状态：线程安全、可序列化、IPC 可读。

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// 流水线阶段（顺序即执行顺序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Ipc,
    Firewall,
    Display,
    Checks,
    Enroll,
    Services,
}

impl Phase {
    pub const ALL: [Phase; 6] = [
        Phase::Ipc,
        Phase::Firewall,
        Phase::Display,
        Phase::Checks,
        Phase::Enroll,
        Phase::Services,
    ];

    #[allow(dead_code)] // 调试用 / 未来日志输出用
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ipc => "ipc",
            Self::Firewall => "firewall",
            Self::Display => "display",
            Self::Checks => "checks",
            Self::Enroll => "enroll",
            Self::Services => "services",
        }
    }
}

/// 单阶段状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseStatus {
    pub phase: Phase,
    /// pending | running | ok | failed | skipped
    pub status: String,
    /// 人类可读摘要（成功/失败原因）。
    #[serde(default)]
    pub message: String,
    /// 阶段产出的结构化详情（防火墙报告、检查结果摘要等）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
    /// 开始时间（Unix 秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    /// 耗时（毫秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}

impl PhaseStatus {
    fn pending(phase: Phase) -> Self {
        Self {
            phase,
            status: "pending".into(),
            message: String::new(),
            detail: None,
            started_at: None,
            elapsed_ms: None,
        }
    }
}

/// 展示层指令：boot-display 轮询到版本变化即切换模板。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayDirective {
    /// 单调递增；boot-display 由此判断"有新指令"。
    pub version: u64,
    /// 模板名：boot | progress | idle | message | enroll | custom。
    pub template: String,
    /// 模板参数（键值对，模板自解释）。
    #[serde(default)]
    pub params: serde_json::Value,
}

impl Default for DisplayDirective {
    fn default() -> Self {
        Self {
            version: 0,
            template: "boot".into(),
            params: serde_json::json!({}),
        }
    }
}

/// 整条流水线的快照（IPC 返回值）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineState {
    /// booting | ready | degraded
    pub overall: String,
    /// 阶段是否配置信息充足（enroll 前置条件）。
    pub config_sufficient: bool,
    pub phases: Vec<PhaseStatus>,
    /// 展示层指令（boot-display 消费）。
    pub display: DisplayDirective,
}

/// 内部可变状态。
#[derive(Clone)]
pub struct SharedState {
    inner: Arc<Mutex<PipelineState>>,
}

impl SharedState {
    pub fn new() -> Self {
        let phases = Phase::ALL
            .iter()
            .map(|p| PhaseStatus::pending(*p))
            .collect();
        Self {
            inner: Arc::new(Mutex::new(PipelineState {
                overall: "booting".into(),
                config_sufficient: false,
                phases,
                display: DisplayDirective::default(),
            })),
        }
    }

    pub fn snapshot(&self) -> PipelineState {
        self.inner.lock().expect("boot state poisoned").clone()
    }

    /// 标记阶段开始。
    pub fn begin(&self, phase: Phase) {
        let mut state = self.inner.lock().expect("boot state poisoned");
        if let Some(slot) = state.phases.iter_mut().find(|p| p.phase == phase) {
            slot.status = "running".into();
            slot.started_at = Some(now_secs());
        }
    }

    /// 标记阶段结束。
    pub fn finish(
        &self,
        phase: Phase,
        status: &str,
        message: impl Into<String>,
        detail: Option<serde_json::Value>,
    ) {
        let mut state = self.inner.lock().expect("boot state poisoned");
        if let Some(slot) = state.phases.iter_mut().find(|p| p.phase == phase) {
            slot.status = status.into();
            slot.message = message.into();
            slot.detail = detail;
            slot.elapsed_ms = slot
                .started_at
                .map(|start| now_secs().saturating_sub(start) * 1000);
        }
        self.recompute_overall(&mut state);
    }

    /// 用外部修改过的快照整体刷新（周期检查用），保持耗时字段不被覆盖。
    pub fn refresh_from(&self, updated: PipelineState) {
        let mut state = self.inner.lock().expect("boot state poisoned");
        *state = updated;
        self.recompute_overall(&mut state);
    }

    /// 推送展示层指令（版本自增；boot-display 轮询到即切换）。
    pub fn push_display(&self, template: &str, params: serde_json::Value) {
        let mut state = self.inner.lock().expect("boot state poisoned");
        state.display.version += 1;
        state.display.template = template.into();
        state.display.params = params;
    }

    pub fn set_config_sufficient(&self, sufficient: bool) {
        let mut state = self.inner.lock().expect("boot state poisoned");
        state.config_sufficient = sufficient;
    }

    /// overall: 全部阶段 ok/ skipped → ready；任一 failed → degraded。
    fn recompute_overall(&self, state: &mut PipelineState) {
        let all_done = state
            .phases
            .iter()
            .all(|p| p.status != "pending" && p.status != "running");
        let any_failed = state.phases.iter().any(|p| p.status == "failed");
        state.overall = if all_done {
            if any_failed { "degraded" } else { "ready" }
        } else {
            "booting"
        }
        .into();
    }
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
