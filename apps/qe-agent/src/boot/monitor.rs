//! 周期性检查：boot 全量检查之后，按配置间隔复检并更新 `/v1/state`。
//!
//! * 首次必检（流水线阶段 4）。
//! * 之后由本模块的线程按 `checks_interval_secs` 复检。
//! * 连上控制服务器后可调 [`MonitorHandle::set_interval`] 应用远端间隔。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::boot::config::CHECKS_INTERVAL_RANGE;
use crate::boot::state::{Phase, SharedState};

/// 周期检查句柄。
#[derive(Clone)]
pub struct MonitorHandle {
    interval_secs: Arc<AtomicU64>,
}

impl MonitorHandle {
    pub fn interval_secs(&self) -> u64 {
        self.interval_secs.load(Ordering::Relaxed)
    }

    /// 远端/配置更新检查间隔；越界值忽略。
    pub fn set_interval(&self, secs: u64) {
        if CHECKS_INTERVAL_RANGE.contains(&secs) {
            self.interval_secs.store(secs, Ordering::Relaxed);
        }
    }
}

/// 启动周期检查线程。
pub fn spawn(state: SharedState, interval_secs: u64) -> MonitorHandle {
    let handle = MonitorHandle {
        interval_secs: Arc::new(AtomicU64::new(interval_secs)),
    };

    let thread_state = state;
    let thread_interval = handle.clone();
    std::thread::Builder::new()
        .name("qe-agent-checks".to_owned())
        .spawn(move || {
            loop {
                let secs = thread_interval.interval_secs().max(1);
                std::thread::sleep(std::time::Duration::from_secs(secs));
                let outcome = crate::boot::checks::run_full_checks();
                let status = if outcome.passed { "ok" } else { "failed" };
                // 复检只更新 checks 阶段的 message/detail，不改 elapsed。
                let mut shared = thread_state.snapshot();
                if let Some(slot) = shared.phases.iter_mut().find(|p| p.phase == Phase::Checks) {
                    slot.status = status.into();
                    slot.message = outcome.summary;
                    slot.detail = Some(outcome.detail);
                }
                thread_state.refresh_from(shared);
            }
        })
        .expect("启动 checks 线程失败");

    handle
}
