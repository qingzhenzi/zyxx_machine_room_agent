//! 阶段 6：服务初始化（容器 / 虚拟化 / P2P）。
//!
//! 各服务探测 + 记录状态；真正的长驻运行仍由 serve 原有路径负责
//! （node agent / room agent）。这里只补齐"检测与就绪标记"。

use crate::boot::checks;
use crate::boot::state::SharedState;

pub fn run(state: &SharedState) {
    state.begin(crate::boot::Phase::Services);

    let mut summary: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();

    // 虚拟化。
    if checks::kvm_available() {
        summary.push("KVM 就绪".into());
    } else {
        failed.push("KVM 不可用".into());
    }

    // 容器运行时。
    if which::which("bwrap").is_ok() {
        summary.push("容器后端(bwrap) 就绪".into());
    } else {
        failed.push("容器后端缺失".into());
    }

    // P2P：能绑定默认端口即认为可启动（真正启动由 node agent 负责）。
    summary.push("P2P 待 node agent 拉起".into());

    let detail = serde_json::json!({
        "kvm": checks::kvm_available(),
        "container": which::which("bwrap").is_ok(),
    });

    if failed.is_empty() {
        state.finish(
            crate::boot::Phase::Services,
            "ok",
            summary.join("；"),
            Some(detail),
        );
    } else {
        state.finish(
            crate::boot::Phase::Services,
            "failed",
            format!("{}（{}）", summary.join("；"), failed.join("；")),
            Some(detail),
        );
    }
}
