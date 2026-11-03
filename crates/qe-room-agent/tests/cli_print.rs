//! 二进制级集成测试：`--once --print` 的 stdout 必须是契约 JSON（v1.1，tier）。
//!
//! 关闭全部外部工具，只验证内置采集 + payload 形状 + CLI/配置错误退出码，
//! 不依赖本机安装 platform-health / docker / virsh。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::thread;

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

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_qe-room-agent"));
    for key in CONFIG_ENV_VARS {
        command.env_remove(key);
    }
    command.env_remove("RUST_LOG");
    command
}

fn disable_external_tools(command: &mut Command) -> &mut Command {
    command
        .env("ROOM_AGENT_DISABLE_HEALTH", "true")
        .env("ROOM_AGENT_DISABLE_DIAGNOSE", "true")
        .env("ROOM_AGENT_DISABLE_SECURITY", "true")
        .env("ROOM_AGENT_DISABLE_DOCKER", "true")
        .env("ROOM_AGENT_DISABLE_VMS", "true")
}

#[test]
fn once_print_emits_contract_json_full_tier() {
    let output =
        disable_external_tools(command().args(["--once", "--print", "--device-slot", "room-101"]))
            .output()
            .expect("启动 qe-room-agent 失败");

    assert!(
        output.status.success(),
        "exit={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let payload: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout 不是纯 JSON: {error}\n{stdout}"));

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

    // metrics：百分比在 0–100；Linux 上 uptime 一定可读。
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

    // agent 元数据 + 两档间隔 + 被禁用工具的状态都在 sections 里。
    let agent = &payload["sections"]["agent"];
    assert_eq!(agent["tier"], serde_json::json!("full"));
    assert_eq!(agent["report_mode"], serde_json::json!("mtls"));
    assert_eq!(agent["interval_secs"], serde_json::json!(300));
    assert_eq!(agent["fast_interval_secs"], serde_json::json!(10));
    assert_eq!(agent["full_interval_secs"], serde_json::json!(300));
    assert!(agent["builtin_collect_duration_ms"].is_u64());
    assert!(agent["host"]["machine_id"].is_string());
    assert!(payload["sections"]["disks"].is_array());
    assert!(payload["sections"]["network"].is_array());
    assert!(payload["sections"]["thermal"].is_array());
    for tool in [
        "platform_health",
        "platform_diagnose",
        "qeaudit",
        "docker",
        "virsh",
    ] {
        assert_eq!(
            agent["tools"][tool],
            serde_json::json!({"available": false, "disabled": true}),
            "tool {tool}"
        );
    }
}

#[test]
fn print_without_credentials_or_token_succeeds() {
    // --print 不需要 DEVICE_PKI_MTLS_URL / MACHINE_AGENT_TOKEN。
    let output =
        disable_external_tools(command().args(["--once", "--print", "--device-slot", "room-101"]))
            .output()
            .expect("启动 qe-room-agent 失败");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn once_mtls_without_endpoint_fails_with_config_error() {
    let output = command()
        .args(["--once", "--device-slot", "room-101"])
        .output()
        .expect("启动 qe-room-agent 失败");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("DEVICE_PKI_MTLS_URL"), "{stderr}");
}

#[test]
fn once_bearer_without_token_fails_with_config_error() {
    let output = command()
        .args(["--once", "--device-slot", "room-101"])
        .env("REPORT_MODE", "bearer")
        .output()
        .expect("启动 qe-room-agent 失败");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("MACHINE_AGENT_TOKEN"), "{stderr}");
}

#[test]
fn once_mtls_without_credentials_exits_nonzero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = disable_external_tools(
        command()
            .args(["--once", "--device-slot", "room-101"])
            .env("DEVICE_PKI_MTLS_URL", "https://127.0.0.1:9")
            .env("DEVICE_DATA_DIR", dir.path()),
    )
    .output()
    .expect("启动 qe-room-agent 失败");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("加载设备凭证失败"), "{stderr}");
}

#[test]
fn unknown_argument_fails_with_usage() {
    let output = command()
        .args(["--wat"])
        .output()
        .expect("启动 qe-room-agent 失败");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("未知参数"), "{stderr}");
    assert!(stderr.contains("用法"), "{stderr}");
}

#[test]
fn help_exits_zero() {
    let output = command()
        .args(["--help"])
        .output()
        .expect("启动 qe-room-agent 失败");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("用法"), "{stdout}");
    assert!(stdout.contains("--once"), "{stdout}");
    assert!(stdout.contains("--device-slot"), "{stdout}");
    assert!(stdout.contains("--fast-interval"), "{stdout}");
    assert!(stdout.contains("--full-interval"), "{stdout}");
    assert!(stdout.contains("--report-mode"), "{stdout}");
}

#[test]
fn once_send_failure_exits_nonzero() {
    // 拿一个刚释放的端口：连接必然被拒绝，验证 --once 的失败退出码。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定临时端口失败");
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let output = disable_external_tools(
        command()
            .args(["--once", "--device-slot", "room-101"])
            .env("REPORT_MODE", "bearer")
            .env("MACHINE_AGENT_TOKEN", "fleet-token")
            .env(
                "MACHINE_API_BASE",
                format!("http://127.0.0.1:{port}/api/v1"),
            ),
    )
    .output()
    .expect("启动 qe-room-agent 失败");

    assert_eq!(output.status.code(), Some(1), "上报失败时 --once 必须非 0");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("上报失败"), "{stderr}");
}

/// 起一个只处理一次请求的 mock server，返回 (api_base, 读取请求的 JoinHandle)。
fn spawn_mock_server() -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定 mock server 失败");
    let address = listener.local_addr().expect("读取 mock server 地址失败");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept 失败");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let read = match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            request.extend_from_slice(&buffer[..read]);
            if let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
                let content_length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
        }
        stream
            .write_all(
                b"HTTP/1.1 202 Accepted\r\nContent-Type: application/json\r\nContent-Length: 30\r\nConnection: close\r\n\r\n{\"snapshot_id\":7,\"serial\":\"abc\"}",
            )
            .unwrap();
        stream.flush().unwrap();
        String::from_utf8_lossy(&request).into_owned()
    });
    (format!("http://{address}/api/v1"), handle)
}

#[test]
fn once_bearer_posts_full_tier_contract_payload() {
    let (api_base, server) = spawn_mock_server();
    let output = disable_external_tools(
        command()
            .args(["--once", "--device-slot", "room-101"])
            .env("REPORT_MODE", "bearer")
            .env("MACHINE_AGENT_TOKEN", "fleet-token")
            .env("MACHINE_API_BASE", &api_base),
    )
    .output()
    .expect("启动 qe-room-agent 失败");
    assert!(
        output.status.success(),
        "exit={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let request = server.join().unwrap();
    assert!(
        request.contains("/api/v1/machine/report"),
        "URL 错误: {request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer fleet-token"),
        "缺少 Bearer 头: {request}"
    );
    assert!(request.contains(r#""tier":"full""#), "{request}");
    assert!(request.contains(r#""device_slot":"room-101""#), "{request}");
}

#[test]
fn fast_interval_out_of_range_fails() {
    let output = command()
        .args([
            "--once",
            "--print",
            "--device-slot",
            "room-101",
            "--fast-interval",
            "4",
        ])
        .output()
        .expect("启动 qe-room-agent 失败");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("fast 上报间隔"), "{stderr}");
}

#[test]
fn full_interval_out_of_range_fails() {
    let output = command()
        .args([
            "--once",
            "--print",
            "--device-slot",
            "room-101",
            "--full-interval",
            "59",
        ])
        .output()
        .expect("启动 qe-room-agent 失败");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("full 上报间隔"), "{stderr}");
}

#[test]
fn invalid_report_mode_fails() {
    let output = command()
        .args([
            "--once",
            "--print",
            "--device-slot",
            "room-101",
            "--report-mode",
            "ftp",
        ])
        .output()
        .expect("启动 qe-room-agent 失败");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("report-mode"), "{stderr}");
}
