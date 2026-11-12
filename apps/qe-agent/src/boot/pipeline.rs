//! 流水线编排：在独立线程按顺序执行各阶段。

use std::path::PathBuf;

use crate::boot::config::BootConfig;
use crate::boot::display;
use crate::boot::enroll;
use crate::boot::firewall;
use crate::boot::monitor;
use crate::boot::services;
use crate::boot::state::{Phase, SharedState};

/// 流水线句柄：IPC 侧只持有它读状态、调周期检查配置。
#[derive(Clone)]
pub struct PipelineHandle {
    pub state: SharedState,
    pub monitor: Option<monitor::MonitorHandle>,
}

impl PipelineHandle {
    /// IPC `/v1/state` 的响应体。
    pub fn state_json(&self) -> serde_json::Value {
        let snapshot = self.state.snapshot();
        serde_json::to_value(&snapshot)
            .unwrap_or_else(|_| serde_json::json!({ "overall": "unknown" }))
    }

    /// 应用远端下发的检查间隔。
    pub fn set_checks_interval(&self, secs: u64) {
        if let Some(monitor) = &self.monitor {
            monitor.set_interval(secs);
        }
    }

    /// 推送展示层指令（版本自增，boot-display 轮询到即切换模板）。
    pub fn push_display(&self, template: &str, params: serde_json::Value) {
        self.state.push_display(template, params);
    }

    /// 当前周期检查间隔（IPC 查询用）。
    pub fn monitor_interval_secs(&self) -> u64 {
        self.monitor
            .as_ref()
            .map(|monitor| monitor.interval_secs())
            .unwrap_or(0)
    }
}

/// 启动 boot 流水线（非阻塞；阶段在专用线程执行）。
pub fn spawn(data_dir: PathBuf, slot: Option<String>) -> PipelineHandle {
    let config = BootConfig::load(&data_dir);
    let state = SharedState::new();
    let handle = PipelineHandle {
        state: state.clone(),
        monitor: None,
    };

    {
        let thread_state = state.clone();
        let thread_config = config.clone();
        let thread_data = data_dir.clone();
        let thread_slot = slot.clone();
        std::thread::Builder::new()
            .name("qe-agent-boot".to_owned())
            .spawn(move || {
                run_all(thread_state, thread_config, thread_data, thread_slot);
            })
            .expect("启动 boot 线程失败");
    }

    // 周期检查线程（首次检查在 run_all 内，之后按间隔复检）。
    let monitor_handle = monitor::spawn(state, config.checks_interval_secs);

    // 展示层 supervise（配置禁用时 supervise 内部自动退出）。
    if config.display.enabled {
        let display_config = config.display.clone();
        std::thread::Builder::new()
            .name("qe-agent-display".to_owned())
            .spawn(move || crate::boot::display::supervise(display_config))
            .ok();
    }

    PipelineHandle {
        monitor: Some(monitor_handle),
        ..handle
    }
}

fn run_all(state: SharedState, config: BootConfig, data_dir: PathBuf, slot: Option<String>) {
    // 1. IPC：serve 在调本函数前已启动 IPC，这里登记为 ok。
    state.finish(Phase::Ipc, "ok", "IPC socket 就绪", None);

    // 2. 防火墙（可配置关闭）。
    firewall::apply_default(&state, config.firewall_enabled);

    // 3. 展示层（配置文件 + 环境变量）。
    display::spawn_if_enabled(&state, &config.display);

    // 4. 全量检查（首次必检；周期复检由 monitor 线程负责）。
    state.begin(Phase::Checks);
    let outcome = crate::boot::checks::run_full_checks();
    let status = if outcome.passed { "ok" } else { "failed" };
    state.finish(
        Phase::Checks,
        status,
        outcome.summary.clone(),
        Some(outcome.detail),
    );

    // 5. 注册（配置不足/失败都不阻塞）。
    enroll::run(&state, &data_dir, slot.as_deref());

    // 6. 桌面机器：boot 关键阶段完成就退出展示层，不阻塞在后续深度服务探测上。
    if config.display.exit_after_boot {
        display::shutdown();
        eprintln!("boot-display 已按 exit_after_boot 配置退出");
    }

    // 7. 服务（P2P 真启动；容器/虚拟化深度探测）。
    services::run(&state, &data_dir);
}
