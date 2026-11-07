//! 遥测角色运行入口；由统一入口 `qe-agent --role machine` 调用。
//!
//! 调度（契约 v1.1）：
//! - fast 档：每 `fast_interval_secs`（默认 10s）采一份内置指标上报；
//! - full 档：每 `full_interval_secs`（默认 300s）采一份含外部能力的完整 payload；
//! - **安全扫描独立规则**：`security_interval_secs`（默认 300s，0 = 跟随 full 档）
//!   单独驱动 qeaudit 扫描；扫描完成**立即**组装 full 快照经 mTLS 实时回传，
//!   不等到下一个上报周期（结果有缓存规则，与 full 档共享，不会重复扫描）；
//! - `--once`：只采/发一份 **full** 后退出（失败退出码 1）；
//! - mTLS 模式启动时先 `GET /agent/config` 拉远端两档间隔（失败 warn 后继续），
//!   运行期每 [`TelemetryIntervals::REFRESH_EVERY_FAST_CYCLES`] 个 fast 周期刷新一次。

use std::time::{Duration, Instant};

use qe_domain::machine::MachineReportTier;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::cli;
use crate::collect;
use crate::config::Config;
use crate::external::ToolsCache;
use crate::policy::{self, TelemetryIntervals};
use crate::report::Sender;

/// 解析 CLI 参数并运行（`run_with_cli` 的便捷包装，测试用）。
pub fn run_from_args(args: Vec<String>) -> i32 {
    // rustls 依赖树里同时存在 aws-lc-rs 与 ring 时无法自动选 provider，必须显式安装。
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cli = match cli::parse(args) {
        Ok(cli) => cli,
        Err(error) => {
            eprintln!("参数错误: {error:#}");
            eprintln!();
            print!("{}", cli::USAGE);
            return 2;
        }
    };
    run_with_cli(cli)
}

/// 已解析 CLI 后运行（统一 `qe-agent` 入口）。
pub fn run_with_cli(cli: cli::Cli) -> i32 {
    // rustls 依赖树里同时存在 aws-lc-rs 与 ring 时无法自动选 provider，必须显式安装。
    let _ = rustls::crypto::ring::default_provider().install_default();
    init_tracing();
    if cli.help {
        print!("{}", cli::USAGE);
        return 0;
    }
    let config = match Config::from_env(&cli) {
        Ok(config) => config,
        Err(error) => {
            error!(error = %format!("{error:#}"), "配置错误");
            return 2;
        }
    };
    run_agent(&config)
}

/// daemon/once 主流程，返回进程退出码。
fn run_agent(config: &Config) -> i32 {
    info!(
        report_mode = config.report_mode.as_str(),
        api_base = %config.api_base,
        mtls_endpoint = ?config.mtls_endpoint,
        device_slot = %config.device_slot,
        data_dir = %config.data_dir.display(),
        fast_interval_secs = config.fast_interval_secs,
        full_interval_secs = config.full_interval_secs,
        once = config.once,
        print = config.print,
        "启动 qe-agent-telemetry"
    );
    if config.print {
        info!("--print 模式：payload 输出到 stdout，不发送");
    }

    let sender = if config.print {
        None
    } else {
        match Sender::new(config) {
            Ok(sender) => Some(sender),
            Err(error) => {
                error!(error = %format!("{error:#}"), "初始化上报客户端失败");
                return 1;
            }
        }
    };

    // 远端策略只在 mTLS 模式可拉；失败只用 warn，继续用本地间隔。
    let mut intervals = TelemetryIntervals::from_config(config);
    if !config.print && config.is_mtls() {
        intervals = policy::fetch_and_apply(config, intervals);
    }
    info!(
        fast_interval_secs = intervals.fast_secs,
        full_interval_secs = intervals.full_secs,
        source = if config.is_mtls() && !config.print {
            "remote-or-local"
        } else {
            "local"
        },
        "当前生效采集间隔"
    );

    let mut tools_cache = ToolsCache::default();
    if config.once {
        let success = run_round(
            config,
            sender.as_ref(),
            &mut tools_cache,
            MachineReportTier::Full,
            intervals,
            1,
        );
        if success {
            info!("单轮模式完成（full）");
            0
        } else {
            warn!("单轮模式失败，退出码 1");
            1
        }
    } else {
        run_daemon(config, sender.as_ref(), &mut tools_cache, intervals)
    }
}

/// 常驻循环：fast/full 两条独立周期 + 安全扫描独立规则；采集耗时超过周期时顺延（不补跑、不突发）。
fn run_daemon(
    config: &Config,
    sender: Option<&Sender>,
    tools_cache: &mut ToolsCache,
    mut intervals: TelemetryIntervals,
) -> i32 {
    let mut next_fast = Instant::now();
    let mut next_full = Instant::now();
    let mut fast_round: u64 = 0;
    let mut full_round: u64 = 0;
    let mut fast_cycles_since_refresh: u64 = 0;

    // 安全扫描独立规则 tick：security_interval_secs 驱动（禁用或 0 = 跟随 full 档）。
    let security_secs = config.security_interval_secs;
    let mut next_security = (!config.disable_security && security_secs > 0).then(Instant::now);

    loop {
        let now = Instant::now();
        let mut next = next_fast.min(next_full);
        if let Some(security) = next_security {
            next = next.min(security);
        }
        if next > now {
            std::thread::sleep(next - now);
        }
        let now = Instant::now();

        if now >= next_fast {
            fast_round += 1;
            run_round(
                config,
                sender,
                tools_cache,
                MachineReportTier::Fast,
                intervals,
                fast_round,
            );
            next_fast = advance(next_fast, intervals.fast_secs, now);
            fast_cycles_since_refresh += 1;
        }
        if now >= next_full {
            full_round += 1;
            run_round(
                config,
                sender,
                tools_cache,
                MachineReportTier::Full,
                intervals,
                full_round,
            );
            next_full = advance(next_full, intervals.full_secs, now);
        }

        // 安全扫描规则触发 → 扫描完成即刻经 mTLS 实时回传一份 full 快照。
        // 缓存与 full 档共享：full 档刚刷新过则这里自然跳过，不重复扫描/上报。
        if let Some(deadline) = next_security
            && now >= deadline
        {
            if crate::external::refresh_security(config, tools_cache) {
                info!(
                    security_interval_secs = security_secs,
                    "安全扫描刷新完成，即时回传 full 快照（mTLS）"
                );
                full_round += 1;
                run_round(
                    config,
                    sender,
                    tools_cache,
                    MachineReportTier::Full,
                    intervals,
                    full_round,
                );
            }
            next_security = Some(advance(deadline, security_secs, now));
        }

        // 运行期策略刷新（简单实现：每 N 个 fast 周期重拉一次）。
        if config.is_mtls()
            && !config.print
            && fast_cycles_since_refresh >= TelemetryIntervals::REFRESH_EVERY_FAST_CYCLES
        {
            fast_cycles_since_refresh = 0;
            intervals = policy::fetch_and_apply(config, intervals);
            info!(
                fast_interval_secs = intervals.fast_secs,
                full_interval_secs = intervals.full_secs,
                "已刷新采集间隔"
            );
        }
    }
}

/// 下一轮 deadline：正常按周期推进；落后超过一个周期时从当前时刻重新起算。
fn advance(deadline: Instant, interval_secs: u64, now: Instant) -> Instant {
    let interval = Duration::from_secs(interval_secs.max(1));
    let next = deadline + interval;
    if next <= now { now + interval } else { next }
}

/// 采集 → 校验 → 打印/上报一轮；返回是否成功。
fn run_round(
    config: &Config,
    sender: Option<&Sender>,
    tools_cache: &mut ToolsCache,
    tier: MachineReportTier,
    intervals: TelemetryIntervals,
    round: u64,
) -> bool {
    let started = Instant::now();
    let report = collect::collect_report(
        config,
        tools_cache,
        tier,
        intervals.fast_secs,
        intervals.full_secs,
    );

    if let Err(error) = report.validate() {
        error!(
            round,
            tier = tier.as_str(),
            error = %format!("{error:#}"),
            "payload 校验失败"
        );
        return false;
    }

    if config.print {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => {
                println!("{json}");
                return true;
            }
            Err(error) => {
                error!(
                    round,
                    tier = tier.as_str(),
                    error = %format!("{error:#}"),
                    "序列化 payload 失败"
                );
                return false;
            }
        }
    }

    let Some(sender) = sender else {
        error!(
            round,
            tier = tier.as_str(),
            "非 --print 模式却没有 HTTP 客户端"
        );
        return false;
    };
    match sender.send(config, &report) {
        Ok(receipt) => {
            info!(
                round,
                tier = tier.as_str(),
                status = receipt.http_status,
                snapshot_id = ?receipt.snapshot_id,
                serial = ?receipt.serial,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "上报成功"
            );
            true
        }
        Err(error) => {
            warn!(
                round,
                tier = tier.as_str(),
                error = %format!("{error:#}"),
                "上报失败"
            );
            false
        }
    }
}

fn init_tracing() {
    // 日志写 stderr：`--print` 的 stdout 必须是可被 jq 解析的纯 JSON。
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,qe_agent_telemetry=debug"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .compact()
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_keeps_cadence_when_on_time() {
        let now = Instant::now();
        let deadline = now - Duration::from_secs(1);
        let next = advance(deadline, 10, now);
        assert_eq!(next, deadline + Duration::from_secs(10));
    }

    #[test]
    fn advance_does_not_burst_after_long_collect() {
        let now = Instant::now();
        // 上一轮 deadline 已过去 30s（采集远慢于周期）→ 从 now 起算，不补跑。
        let deadline = now - Duration::from_secs(30);
        let next = advance(deadline, 10, now);
        assert_eq!(next, now + Duration::from_secs(10));
    }

    #[test]
    fn advance_never_uses_zero_interval() {
        let now = Instant::now();
        let next = advance(now, 0, now);
        assert_eq!(next, now + Duration::from_secs(1));
    }
}
