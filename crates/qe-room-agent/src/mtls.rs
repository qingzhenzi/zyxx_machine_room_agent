//! mTLS 支持：设备凭证加载、blocking 上报客户端构建、`TransportConfig` 构造。
//!
//! 凭证目录布局与 `qe-device-pki-client` 完全一致：`<DEVICE_DATA_DIR>/creds/<slot>`，
//! 由 `qe-agent enroll <注册链接>` 写入（`cert.pem` / `key.pem` / `ca-chain.pem` /
//! `meta.json`）。端点以环境变量 `DEVICE_PKI_MTLS_URL` 为准 —— 证书 `meta.json`
//! 里记录的是注册时的地址，可能已过时。

use std::time::Duration;

use anyhow::{Context, Result};
use qe_device_pki_client::creds::Credentials;
use qe_device_pki_client::transport::TransportConfig;

use crate::config::Config;

/// mTLS 上机器上报的路径（`POST {mtls_endpoint}/machine/report`）。
pub const MACHINE_REPORT_PATH: &str = "/machine/report";
/// mTLS 上策略拉取的路径（`GET {mtls_endpoint}/agent/config`）。
pub const AGENT_CONFIG_PATH: &str = "/agent/config";
/// mTLS 上报 HTTP 超时（与 Bearer 一致）。
pub const MTLS_TIMEOUT: Duration = Duration::from_secs(10);

/// 加载设备凭证（证书/私钥/CA 链）。
///
/// `DEVICE_PKI_MTLS_URL` 必须存在（`--print` 模式不会调用本函数）。
pub fn load_credentials(config: &Config) -> Result<Credentials> {
    let mtls_endpoint = config
        .mtls_endpoint
        .as_deref()
        .context("mTLS 模式缺少 DEVICE_PKI_MTLS_URL")?;
    let dir = config.creds_dir()?;
    let mut credentials = Credentials::load(&dir).with_context(|| {
        format!(
            "加载设备凭证失败: {}（该槽位是否已用 qe-agent enroll 注册？）",
            dir.display()
        )
    })?;
    if credentials.device_slot.is_empty() {
        credentials.device_slot = config.device_slot.clone();
    }
    // 以环境变量为准（证书 meta 里的端点可能是注册时的旧地址）。
    credentials.mtls_endpoint = mtls_endpoint.to_owned();
    Ok(credentials)
}

/// 构造 `qe-device-pki-client` 的传输配置（策略拉取用）。
pub fn transport_config(credentials: &Credentials, config: &Config) -> TransportConfig {
    let mut transport = TransportConfig::new(credentials.clone(), config.data_dir.clone());
    transport.mtls_endpoint = credentials.mtls_endpoint.clone();
    transport
}

/// 构建 blocking mTLS 客户端：客户端证书 + 设备 CA 链做服务端校验。
///
/// 与 `qe_device_pki_client::transport::build_mtls_client` 的信任配置一致，
/// 只是这里用 blocking 客户端（room-agent 主循环是同步的）。
pub fn build_blocking_client(config: &Config) -> Result<reqwest::blocking::Client> {
    let credentials = load_credentials(config)?;
    build_blocking_client_with(&credentials, MTLS_TIMEOUT)
}

/// 用给定凭证构建 blocking mTLS 客户端（测试/复用）。
pub fn build_blocking_client_with(
    credentials: &Credentials,
    timeout: Duration,
) -> Result<reqwest::blocking::Client> {
    let mut identity_pem = credentials.cert_pem.clone();
    identity_pem.push_str(&credentials.key_pem);
    let identity = reqwest::Identity::from_pem(identity_pem.as_bytes())
        .context("构建设备客户端证书失败（cert.pem / key.pem 是否配对？）")?;

    let mut builder = reqwest::blocking::Client::builder()
        .use_rustls_tls()
        .identity(identity)
        .timeout(timeout);
    for der in credentials.ca_roots().context("解析设备 CA 链失败")? {
        let cert = reqwest::Certificate::from_der(der.as_ref()).context("构建设备 CA 证书失败")?;
        builder = builder.add_root_certificate(cert);
    }
    builder.build().context("构建 mTLS HTTP 客户端失败")
}

/// 拼接 `{base}{path}`，避免出现双斜杠。
pub fn join_endpoint(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if path.is_empty() {
        return base.to_owned();
    }
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_endpoint_handles_slashes() {
        assert_eq!(
            join_endpoint("https://node:4441", "/machine/report"),
            "https://node:4441/machine/report"
        );
        assert_eq!(
            join_endpoint("https://node:4441/", "machine/report"),
            "https://node:4441/machine/report"
        );
        assert_eq!(join_endpoint("https://node:4441/", ""), "https://node:4441");
    }
}
