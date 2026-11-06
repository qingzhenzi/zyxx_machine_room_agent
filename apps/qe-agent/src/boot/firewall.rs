//! 阶段 2：应用防火墙。

use crate::boot::state::SharedState;
use qe_nftables::{FirewallSpec, ServicePreset};

/// 应用默认防火墙（LAN 预设）。失败不中断 boot。
pub fn apply_default(state: &SharedState, enabled: bool) {
    state.begin(crate::boot::Phase::Firewall);
    let spec = FirewallSpec::default()
        .with_preset(ServicePreset::Lan)
        .with_enabled(enabled);
    if !enabled {
        state.finish(
            crate::boot::Phase::Firewall,
            "skipped",
            "配置禁用防火墙",
            None,
        );
        return;
    }
    let report = qe_nftables::apply(&spec);
    let detail = serde_json::to_value(&report).ok();
    if report.applied {
        state.finish(
            crate::boot::Phase::Firewall,
            "ok",
            format!(
                "防火墙已应用（{} 放行 / {} 阻断，{}ms）",
                report.rules.ports, report.rules.blocked, report.elapsed_ms
            ),
            detail,
        );
    } else if !qe_nftables::nft_available() {
        // 环境里没有 nft（如容器内开发），降级跳过而非失败。
        state.finish(
            crate::boot::Phase::Firewall,
            "skipped",
            "nft 不可用，跳过防火墙配置",
            detail,
        );
    } else {
        state.finish(
            crate::boot::Phase::Firewall,
            "failed",
            format!("防火墙应用失败: {}", report.error),
            detail,
        );
    }
}
