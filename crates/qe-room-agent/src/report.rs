//! 契约 payload（`docs/machine-telemetry.md` v1.1）与 HTTP 上报。
//!
//! [`Report`] 就是 `POST …/machine/report` 的请求体；[`Report::validate`] 在发送/打印前
//! 做契约字段校验，避免把坏数据推给服务端。[`Sender`] 是 reqwest blocking 客户端：
//! - mTLS 模式（默认）：客户端证书 + 设备 CA 链，`POST {DEVICE_PKI_MTLS_URL}/machine/report`；
//! - Bearer 模式（兼容）：`POST {MACHINE_API_BASE}/machine/report` + `Authorization: Bearer`。
//!
//! `tier` 复用 `qe_domain::machine::MachineReportTier`（fast / full），服务端按它分档落库。

use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use qe_domain::machine::MachineReportTier;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::{Config, ReportMode};

pub use crate::health::{Health, HealthStatus, Issue, Severity};

/// 契约版本，固定 1。
pub const SCHEMA_VERSION: u32 = 1;
/// 上报 HTTP 超时。
pub const REPORT_TIMEOUT: Duration = Duration::from_secs(10);
/// 允许的 `collected_at` 未来偏差（契约：未来 >5 分钟视为非法）。
pub const MAX_CLOCK_SKEW: chrono::TimeDelta = chrono::TimeDelta::minutes(5);

/// 上报 payload 的指标字段（全部可选）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    /// CPU 使用率（0–100）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_usage_percent: Option<f64>,
    /// 内存使用率（0–100）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_usage_percent: Option<f64>,
    /// 代表分区使用率（0–100）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_usage_percent: Option<f64>,
    /// 开机时长（秒）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_secs: Option<u64>,
}

/// 一次机器遥测上报。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// 契约版本，固定 1。
    pub schema: u32,
    /// 设备槽位（1–128 字符）。
    pub device_slot: String,
    /// agent 版本（1–64 字符）。
    pub agent_version: String,
    /// 采集时间（RFC3339）。
    pub collected_at: String,
    /// 主机名（必填，1–255 字符）。
    pub hostname: String,
    /// 发行版展示名。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_name: Option<String>,
    /// 内核版本。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel_version: Option<String>,
    /// 健康结论。
    pub health: Health,
    /// 指标摘要。
    #[serde(default)]
    pub metrics: Metrics,
    /// 采集档位：fast（内置指标）/ full（含外部工具）。
    pub tier: MachineReportTier,
    /// 任意 JSON 明细（外部工具原样数据 + agent 元数据）。
    #[serde(default)]
    pub sections: Map<String, Value>,
}

impl Report {
    /// 发送/打印前的契约校验。
    pub fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA_VERSION {
            bail!("schema 必须为 {SCHEMA_VERSION}，实际 {}", self.schema);
        }
        check_len("device_slot", &self.device_slot, 1, 128)?;
        check_len("agent_version", &self.agent_version, 1, 64)?;
        check_len("hostname", &self.hostname, 1, 255)?;

        let collected_at = DateTime::parse_from_rfc3339(&self.collected_at)
            .with_context(|| format!("collected_at 不是合法 RFC3339: {}", self.collected_at))?
            .with_timezone(&Utc);
        if collected_at > Utc::now() + MAX_CLOCK_SKEW {
            bail!(
                "collected_at 超过当前时间 5 分钟，拒绝上报: {}",
                self.collected_at
            );
        }

        if let Some(score) = self.health.score
            && score > 100
        {
            bail!("health.score 必须在 0..=100，实际 {score}");
        }
        if self.health.issues.len() > 256 {
            bail!(
                "health.issues 过多（{} 条，上限 256）",
                self.health.issues.len()
            );
        }
        check_percent("metrics.cpu_usage_percent", self.metrics.cpu_usage_percent)?;
        check_percent(
            "metrics.memory_usage_percent",
            self.metrics.memory_usage_percent,
        )?;
        check_percent(
            "metrics.disk_usage_percent",
            self.metrics.disk_usage_percent,
        )?;
        Ok(())
    }
}

fn check_len(field: &str, value: &str, min: usize, max: usize) -> Result<()> {
    let len = value.chars().count();
    if len < min || len > max {
        bail!("{field} 长度必须在 {min}..={max} 字符之间，实际 {len}");
    }
    Ok(())
}

fn check_percent(field: &str, value: Option<f64>) -> Result<()> {
    if let Some(value) = value
        && (!value.is_finite() || !(0.0..=100.0).contains(&value))
    {
        bail!("{field} 必须在 0..=100，实际 {value}");
    }
    Ok(())
}

/// 拼接上报 URL：`{base}/machine/report`（base 可为 api_base 或 mTLS 端点）。
pub fn report_url(base: &str) -> String {
    format!("{}/machine/report", base.trim_end_matches('/'))
}

/// 上报成功后的回执信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportReceipt {
    /// HTTP 状态码（契约成功为 202）。
    pub http_status: u16,
    /// 服务端生成的快照 id。
    pub snapshot_id: Option<i64>,
    /// 服务端解析出的设备 serial。
    pub serial: Option<String>,
}

/// reqwest blocking 上报客户端（按 [`ReportMode`] 决定认证层）。
pub struct Sender {
    client: reqwest::blocking::Client,
    mode: ReportMode,
    /// Bearer 模式为 api_base，mTLS 模式为 mTLS 端点。
    base: String,
}

impl std::fmt::Debug for Sender {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Sender")
            .field("mode", &self.mode)
            .field("base", &self.base)
            .field("timeout", &REPORT_TIMEOUT)
            .finish()
    }
}

impl Sender {
    /// 按配置构建客户端（mTLS 模式会加载设备凭证）。
    pub fn new(config: &Config) -> Result<Self> {
        match config.report_mode {
            ReportMode::Bearer => {
                let client = reqwest::blocking::Client::builder()
                    .timeout(REPORT_TIMEOUT)
                    .build()
                    .context("构建 HTTP 客户端失败")?;
                Ok(Self {
                    client,
                    mode: ReportMode::Bearer,
                    base: config.api_base.clone(),
                })
            }
            ReportMode::Mtls => {
                let client = crate::mtls::build_blocking_client(config)?;
                let base = config
                    .mtls_endpoint
                    .clone()
                    .context("mTLS 模式缺少 DEVICE_PKI_MTLS_URL")?;
                Ok(Self {
                    client,
                    mode: ReportMode::Mtls,
                    base,
                })
            }
        }
    }

    /// 上报模式（日志/测试用）。
    pub fn mode(&self) -> ReportMode {
        self.mode
    }

    /// 实际上报 URL。
    pub fn url(&self) -> String {
        report_url(&self.base)
    }

    /// 校验并上报一份 payload。
    pub fn send(&self, config: &Config, report: &Report) -> Result<ReportReceipt> {
        report.validate().context("payload 校验失败，拒绝上报")?;
        let url = self.url();

        let mut request = self
            .client
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/json");
        if self.mode == ReportMode::Bearer {
            let token = config
                .token
                .as_deref()
                .context("缺少 MACHINE_AGENT_TOKEN，无法上报")?;
            request = request.bearer_auth(token);
        }

        let response = request
            .json(report)
            .send()
            .with_context(|| format!("POST {url} 失败"))?;
        let status = response.status();
        let body = response.text().unwrap_or_default();
        if !status.is_success() {
            bail!(
                "上报失败: HTTP {}（{}），响应: {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("unknown"),
                truncate(&body, 500)
            );
        }

        let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        Ok(ReportReceipt {
            http_status: status.as_u16(),
            snapshot_id: parsed.get("snapshot_id").and_then(Value::as_i64),
            serial: parsed
                .get("serial")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }
}

fn truncate(text: &str, limit: usize) -> String {
    let mut result: String = text.chars().take(limit).collect();
    if text.chars().count() > limit {
        result.push('…');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use qe_device_pki_client::creds::Credentials;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::Path;
    use std::thread;

    fn sample_report() -> Report {
        let mut sections = Map::new();
        sections.insert(
            "agent".to_owned(),
            serde_json::json!({
                "agent_version": env!("CARGO_PKG_VERSION"),
                "interval_secs": 60,
                "tools": {},
                "host": {"machine_id": "abc"},
            }),
        );
        Report {
            schema: SCHEMA_VERSION,
            device_slot: "room-101".to_owned(),
            agent_version: env!("CARGO_PKG_VERSION").to_owned(),
            collected_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            hostname: "room-101-node".to_owned(),
            os_name: Some("Debian 13".to_owned()),
            kernel_version: Some("6.12.0-amd64".to_owned()),
            health: Health {
                status: HealthStatus::Ok,
                score: Some(96),
                issues: Vec::new(),
            },
            metrics: Metrics {
                cpu_usage_percent: Some(14.7),
                memory_usage_percent: Some(42.1),
                disk_usage_percent: Some(61.2),
                uptime_secs: Some(3280),
            },
            tier: MachineReportTier::Full,
            sections,
        }
    }

    fn bearer_config(api_base: &str) -> Config {
        let mut map = std::collections::HashMap::new();
        map.insert("REPORT_MODE".to_owned(), "bearer".to_owned());
        map.insert("MACHINE_AGENT_TOKEN".to_owned(), "fleet-token".to_owned());
        map.insert("DEVICE_SLOT".to_owned(), "room-101".to_owned());
        map.insert("MACHINE_API_BASE".to_owned(), api_base.to_owned());
        Config::from_lookup(&Cli::default(), |key| map.get(key).cloned()).unwrap()
    }

    fn mtls_config(data_dir: &Path) -> Config {
        let mut map = std::collections::HashMap::new();
        map.insert(
            "DEVICE_PKI_MTLS_URL".to_owned(),
            "https://node:4441".to_owned(),
        );
        map.insert("DEVICE_SLOT".to_owned(), "room-101".to_owned());
        map.insert("DEVICE_DATA_DIR".to_owned(), data_dir.display().to_string());
        Config::from_lookup(&Cli::default(), |key| map.get(key).cloned()).unwrap()
    }

    /// 生成一份真实可解析的客户端证书 + 私钥 + CA 链，并落盘到 `<data>/creds/<slot>`。
    fn write_credentials(data_dir: &Path) {
        use rcgen::{
            BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair,
        };

        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::default();
        let mut ca_dn = DistinguishedName::new();
        ca_dn.push(DnType::CommonName, "Test CA");
        ca_params.distinguished_name = ca_dn;
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();

        let key = KeyPair::generate().unwrap();
        let mut leaf_params = CertificateParams::default();
        let mut leaf_dn = DistinguishedName::new();
        leaf_dn.push(DnType::CommonName, "room-101");
        leaf_params.distinguished_name = leaf_dn;
        let leaf = leaf_params.signed_by(&key, &ca_cert, &ca_key).unwrap();

        let credentials = Credentials {
            device_slot: "room-101".to_owned(),
            serial: "abcd".to_owned(),
            cert_pem: leaf.pem(),
            key_pem: key.serialize_pem(),
            ca_chain_pem: ca_cert.pem(),
            endpoint: "node:4440".to_owned(),
            mtls_endpoint: "node:4441".to_owned(),
        };
        let dir = data_dir.join("creds").join("room-101");
        credentials.save(&dir).unwrap();
    }

    #[test]
    fn report_url_has_no_double_slash() {
        assert_eq!(
            report_url("http://127.0.0.1:8080/api/v1"),
            "http://127.0.0.1:8080/api/v1/machine/report"
        );
        assert_eq!(
            report_url("http://127.0.0.1:8080/api/v1/"),
            "http://127.0.0.1:8080/api/v1/machine/report"
        );
        assert_eq!(
            report_url("https://node:4441/"),
            "https://node:4441/machine/report"
        );
    }

    #[test]
    fn valid_report_passes() {
        sample_report().validate().unwrap();
    }

    #[test]
    fn invalid_schema_is_rejected() {
        let mut report = sample_report();
        report.schema = 2;
        assert!(
            report
                .validate()
                .unwrap_err()
                .to_string()
                .contains("schema")
        );
    }

    #[test]
    fn field_lengths_are_enforced() {
        let mut report = sample_report();
        report.device_slot = "  ".to_owned();
        // 空白槽位长度非 0，但契约语义上非法；配置层已拒绝，这里只覆盖长度。
        assert!(report.validate().is_ok());

        report.device_slot = String::new();
        assert!(
            report
                .validate()
                .unwrap_err()
                .to_string()
                .contains("device_slot")
        );

        report.device_slot = "x".repeat(129);
        assert!(
            report
                .validate()
                .unwrap_err()
                .to_string()
                .contains("device_slot")
        );

        let mut report = sample_report();
        report.hostname = String::new();
        assert!(
            report
                .validate()
                .unwrap_err()
                .to_string()
                .contains("hostname")
        );

        let mut report = sample_report();
        report.agent_version = "x".repeat(65);
        assert!(
            report
                .validate()
                .unwrap_err()
                .to_string()
                .contains("agent_version")
        );
    }

    #[test]
    fn collected_at_must_be_rfc3339_and_not_far_in_future() {
        let mut report = sample_report();
        report.collected_at = "yesterday".to_owned();
        assert!(
            report
                .validate()
                .unwrap_err()
                .to_string()
                .contains("collected_at")
        );

        let future = Utc::now() + chrono::TimeDelta::minutes(10);
        report.collected_at = future.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let err = report.validate().unwrap_err();
        assert!(err.to_string().contains("超过当前时间"), "{err}");
    }

    #[test]
    fn metric_ranges_are_enforced() {
        let mut report = sample_report();
        report.metrics.cpu_usage_percent = Some(101.0);
        assert!(
            report
                .validate()
                .unwrap_err()
                .to_string()
                .contains("cpu_usage_percent")
        );

        report.metrics.cpu_usage_percent = Some(f64::NAN);
        assert!(report.validate().is_err());

        report.metrics.cpu_usage_percent = Some(14.7);
        report.metrics.disk_usage_percent = Some(-1.0);
        assert!(report.validate().is_err());

        report.metrics.disk_usage_percent = Some(61.2);
        report.health.score = Some(120);
        assert!(report.validate().unwrap_err().to_string().contains("score"));
    }

    #[test]
    fn serializes_contract_shape() {
        let value = serde_json::to_value(sample_report()).unwrap();
        assert_eq!(value["schema"], serde_json::json!(1));
        assert_eq!(value["device_slot"], serde_json::json!("room-101"));
        assert_eq!(value["tier"], serde_json::json!("full"));
        assert_eq!(
            value["agent_version"],
            serde_json::json!(env!("CARGO_PKG_VERSION"))
        );
        assert!(value["collected_at"].is_string());
        assert_eq!(value["hostname"], serde_json::json!("room-101-node"));
        assert_eq!(value["health"]["status"], serde_json::json!("ok"));
        assert_eq!(value["health"]["score"], serde_json::json!(96));
        assert_eq!(
            value["metrics"]["cpu_usage_percent"],
            serde_json::json!(14.7)
        );
        assert_eq!(value["metrics"]["uptime_secs"], serde_json::json!(3280));
        assert!(value["sections"]["agent"]["interval_secs"].is_number());

        // 契约示例里的必填字段一个都不能少。
        for key in [
            "schema",
            "device_slot",
            "agent_version",
            "collected_at",
            "hostname",
            "health",
            "metrics",
            "tier",
            "sections",
        ] {
            assert!(value.get(key).is_some(), "缺少字段 {key}");
        }

        // 反序列化回结构体（服务端兼容性冒烟）。
        let roundtrip: Report = serde_json::from_value(value).unwrap();
        assert_eq!(roundtrip.device_slot, "room-101");
        assert_eq!(roundtrip.tier, MachineReportTier::Full);
    }

    #[test]
    fn tier_serializes_lowercase() {
        let mut report = sample_report();
        report.tier = MachineReportTier::Fast;
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["tier"], serde_json::json!("fast"));
        let roundtrip: Report = serde_json::from_value(value).unwrap();
        assert_eq!(roundtrip.tier, MachineReportTier::Fast);
    }

    #[test]
    fn optional_fields_are_omitted_when_none() {
        let mut report = sample_report();
        report.os_name = None;
        report.kernel_version = None;
        report.health.score = None;
        report.metrics.cpu_usage_percent = None;
        let value = serde_json::to_value(&report).unwrap();
        assert!(value.get("os_name").is_none());
        assert!(value.get("kernel_version").is_none());
        assert!(value["health"].get("score").is_none());
        assert!(value["metrics"].get("cpu_usage_percent").is_none());
    }

    // ---- HTTP mock server -------------------------------------------------

    fn spawn_mock_server(
        status_line: &'static str,
        response_body: &'static str,
    ) -> (String, thread::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定 mock server 失败");
        let address = listener.local_addr().expect("读取 mock server 地址失败");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept 失败");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                request.extend_from_slice(&buffer[..read]);
                if let Some(header_end) = find_header_end(&request) {
                    let headers =
                        String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
                    let content_length = content_length(&headers);
                    if request.len() >= header_end + 4 + content_length {
                        break;
                    }
                }
            }
            let response = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
            request
        });
        (format!("http://{address}/api/v1"), handle)
    }

    fn find_header_end(request: &[u8]) -> Option<usize> {
        request.windows(4).position(|window| window == b"\r\n\r\n")
    }

    fn content_length(headers: &str) -> usize {
        headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0)
    }

    #[test]
    fn bearer_send_posts_bearer_json_and_parses_receipt() {
        let (api_base, server) = spawn_mock_server(
            "202 Accepted",
            r#"{"snapshot_id":123,"serial":"4f40ccef","received_at":"2026-10-31T14:53:02Z"}"#,
        );
        let config = bearer_config(&api_base);
        let report = sample_report();
        let sender = Sender::new(&config).unwrap();
        assert_eq!(sender.mode(), ReportMode::Bearer);
        assert_eq!(sender.url(), format!("{api_base}/machine/report"));
        let receipt = sender.send(&config, &report).unwrap();

        assert_eq!(
            receipt,
            ReportReceipt {
                http_status: 202,
                snapshot_id: Some(123),
                serial: Some("4f40ccef".to_owned()),
            }
        );

        let request = server.join().unwrap();
        let request_text = String::from_utf8_lossy(&request);
        let lowercase = request_text.to_ascii_lowercase();
        assert!(
            lowercase.contains("authorization: bearer fleet-token"),
            "{lowercase}"
        );
        assert!(
            lowercase.contains("content-type: application/json"),
            "{lowercase}"
        );
        assert!(
            request_text.contains("/api/v1/machine/report"),
            "{request_text}"
        );
        assert!(
            request_text.contains(r#""device_slot":"room-101""#),
            "{request_text}"
        );
        assert!(request_text.contains(r#""schema":1"#), "{request_text}");
        assert!(request_text.contains(r#""tier":"full""#), "{request_text}");
    }

    #[test]
    fn bearer_non_2xx_response_is_an_error_with_body() {
        let (api_base, server) = spawn_mock_server(
            "500 Internal Server Error",
            r#"{"error":{"code":"internal","message":"boom"}}"#,
        );
        let config = bearer_config(&api_base);
        let error = Sender::new(&config)
            .unwrap()
            .send(&config, &sample_report())
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("HTTP 500"), "{message}");
        assert!(message.contains("boom"), "{message}");
        server.join().unwrap();
    }

    #[test]
    fn missing_token_is_an_error_before_request() {
        // --print 模式允许没有 token；此时若误调用 send 必须报错。
        let mut map = std::collections::HashMap::new();
        map.insert("REPORT_MODE".to_owned(), "bearer".to_owned());
        map.insert("DEVICE_SLOT".to_owned(), "room-101".to_owned());
        let cli = Cli {
            print: true,
            once: true,
            ..Cli::default()
        };
        let config = Config::from_lookup(&cli, |key| map.get(key).cloned()).unwrap();
        let error = Sender::new(&config)
            .unwrap()
            .send(&config, &sample_report())
            .unwrap_err();
        assert!(format!("{error:#}").contains("MACHINE_AGENT_TOKEN"));
    }

    #[test]
    fn mtls_sender_loads_credentials_and_targets_mtls_endpoint() {
        let data_dir = tempfile::tempdir().unwrap();
        write_credentials(data_dir.path());
        let config = mtls_config(data_dir.path());

        let sender = Sender::new(&config).unwrap();
        assert_eq!(sender.mode(), ReportMode::Mtls);
        assert_eq!(sender.url(), "https://node:4441/machine/report");
        // mTLS 模式不需要 token
        assert!(config.token.is_none());
    }

    #[test]
    fn mtls_sender_without_credentials_fails_with_actionable_error() {
        let data_dir = tempfile::tempdir().unwrap();
        let config = mtls_config(data_dir.path());
        let error = Sender::new(&config).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("加载设备凭证失败"), "{message}");
        assert!(message.contains("room-101"), "{message}");
    }

    #[test]
    fn mtls_send_validates_payload_before_network() {
        // 校验失败必须在触网（TLS 握手）之前返回。
        let data_dir = tempfile::tempdir().unwrap();
        write_credentials(data_dir.path());
        let config = mtls_config(data_dir.path());
        let sender = Sender::new(&config).unwrap();
        let mut report = sample_report();
        report.schema = 2;
        let error = sender.send(&config, &report).unwrap_err();
        assert!(format!("{error:#}").contains("payload 校验失败"));
    }
}
