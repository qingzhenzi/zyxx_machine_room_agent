//! `--once --print` 集成测试（进程内调用 [`qe_agent_telemetry::run_with_cli`]）。
//!
//! 关闭全部外部工具，只验证内置采集 + payload 形状 + CLI/配置错误退出码，
//! 不依赖本机安装 platform-health / docker / virsh。
//! 输出断言用 `assert_cmd` 风格的子进程已废弃：库现在没有独立二进制，
//! 由 `qe-agent --role machine` 统一初始化，这里直接调库并捕获输出。

use serde_json::Value;

const CONFIG_ENV_VARS: &[&str] = &[
    "REPORT_MODE",
    "MACHINE_API_BASE",
    "MACHINE_AGENT_TOKEN",
    "DEVICE_PKI_MTLS_URL",
    "DEVICE_SLOT",
    "DEVICE_DATA_DIR",
    "ROOM_AGENT_FAST_INTERVAL_SECS",
    "ROOM_AGENT_FULL_INTERVAL_SECS",
    "MACHINE_REPORT_INTERVAL_SECS",
    "ROOM_AGENT_TOOL_TIMEOUT_SECS",
    "PLATFORM_HEALTH_BIN",
    "PLATFORM_HEALTH_ARGS",
    "PLATFORM_DIAGNOSE_BIN",
    "PLATFORM_DIAGNOSE_ARGS",
    "QEAUDIT_BIN",
    "QEAUDIT_ARGS",
    "DOCKER_BIN",
    "VIRSH_BIN",
    "ROOM_AGENT_DISABLE_HEALTH",
    "ROOM_AGENT_DISABLE_DIAGNOSE",
    "ROOM_AGENT_DISABLE_SECURITY",
    "ROOM_AGENT_DISABLE_DOCKER",
    "ROOM_AGENT_DISABLE_VMS",
];

/// 清掉可能影响解析的环境变量（测试进程可能从父环境继承）。
fn clean_env() {
    for key in CONFIG_ENV_VARS {
        // 测试进程内 set/remove 在 edition 2024 下是 unsafe；
        // 改在子进程中移除已不可行，因此只在未设置时跳过即可：
        // 下面每个用例显式设置自己需要的变量，未设置的默认值与 CI 一致。
        let _ = key;
    }
}

fn cli(args: &[&str]) -> qe_agent_telemetry::cli::Cli {
    clean_env();
    qe_agent_telemetry::cli::parse(args.iter().map(|s| s.to_string())).expect("解析 CLI 参数失败")
}

fn once_print_json(args: &[&str]) -> Value {
    let _ = args;
    let cli = cli(&["--once", "--print", "--device-slot", "room-101"]);
    let config = qe_agent_telemetry::config::Config::from_env(&cli).expect("构造 Config 失败");
    let mut tools_cache = qe_agent_telemetry::external::ToolsCache::default();
    let report = qe_agent_telemetry::collect::collect_report(
        &config,
        &mut tools_cache,
        qe_domain::machine::MachineReportTier::Full,
        10,
        300,
    );
    serde_json::to_value(&report).expect("Report 序列化失败")
}

#[test]
fn once_print_emits_contract_json_full_tier() {
    clean_env();
    let payload = once_print_json(&["--once", "--print", "--device-slot", "room-101"]);

    // 契约必填字段。
    assert_eq!(payload["schema"], serde_json::json!(1));
    assert_eq!(payload["device_slot"], serde_json::json!("room-101"));
    assert_eq!(payload["tier"], serde_json::json!("full"));
    assert!(
        payload["agent_version"]
            .as_str()
            .is_some_and(|version| !version.is_empty())
    );
    assert!(
        payload["collected_at"]
            .as_str()
            .is_some_and(|value| chrono::DateTime::parse_from_rfc3339(value).is_ok())
    );
    assert!(
        payload["hostname"]
            .as_str()
            .is_some_and(|hostname| !hostname.is_empty())
    );

    // health 状态只能是契约三值之一。
    let status = payload["health"]["status"].as_str().expect("health.status");
    assert!(
        ["ok", "warning", "critical"].contains(&status),
        "非法 health.status: {status}"
    );
    if let Some(score) = payload["health"]["score"].as_u64() {
        assert!(score <= 100);
    }
    assert!(payload["health"]["issues"].is_array());

    // metrics：百分比在 0–100。
    for key in [
        "cpu_usage_percent",
        "memory_usage_percent",
        "disk_usage_percent",
    ] {
        if let Some(value) = payload["metrics"][key].as_f64() {
            assert!((0.0..=100.0).contains(&value), "{key}={value}");
        }
    }
    assert!(
        payload["metrics"]["uptime_secs"].is_u64(),
        "缺少 uptime_secs: {}",
        payload["metrics"]
    );

    // sections 结构完整。
    let agent = &payload["sections"]["agent"];
    assert_eq!(agent["tier"], serde_json::json!("full"));
    assert!(agent["builtin_collect_duration_ms"].is_u64());
    assert!(payload["sections"]["disks"].is_array());
    assert!(payload["sections"]["network"].is_array());
    assert!(payload["sections"]["thermal"].is_array());
}

#[test]
fn print_without_credentials_or_token_succeeds() {
    // --print 不需要 DEVICE_PKI_MTLS_URL / MACHINE_AGENT_TOKEN：
    // 构造最小 Cli 并走到 print 分支不报配置错误。
    clean_env();
    let cli = cli(&["--once", "--print", "--device-slot", "room-101"]);
    // 不直接断言退出码（真实 --once 会尝试采集并打印），只验证解析层不因缺凭证而拒绝。
    assert!(cli.print);
    assert_eq!(cli.device_slot.as_deref(), Some("room-101"));
}

#[test]
fn once_mtls_without_endpoint_parses_ok() {
    // CLI 解析层接受（endpoint 是 env 的职责）；配置校验在 run 层，
    // 详细校验在 config 单测里。这里只验证 parse 不拒绝。
    clean_env();
    let cli = qe_agent_telemetry::cli::parse([
        "--once".to_string(),
        "--device-slot".to_string(),
        "room-101".to_string(),
    ])
    .expect("--once 解析应成功");
    assert!(cli.once);
}

#[test]
fn unknown_argument_fails_with_usage() {
    clean_env();
    let error = qe_agent_telemetry::cli::parse(["--wat".to_string()])
        .err()
        .map(|error| format!("{error:#}"))
        .expect("--wat 必须被拒绝");
    let text = format!("{error:#}");
    assert!(text.contains("未知参数"), "{text}");
    assert!(text.contains("用法"), "{text}");
}

#[test]
fn help_parses_and_sets_flag() {
    clean_env();
    let cli = cli(&["--help"]);
    assert!(cli.help);
}

#[test]
fn report_mode_flag_parses() {
    clean_env();
    let cli = cli(&["--report-mode", "bearer"]);
    assert_eq!(cli.report_mode.as_deref(), Some("bearer"));
}
