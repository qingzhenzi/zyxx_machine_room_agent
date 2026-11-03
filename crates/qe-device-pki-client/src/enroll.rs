//! 注册流程（契约第 3、4 节）。
//!
//! ```text
//! 解析链接 → POST /enroll/challenge → 生成密钥 + CSR + PoP
//!          → POST /enroll → auto: 直接落盘 / approval: 轮询后一次性下载 → 落盘
//! ```
//!
//! 注册成功后建议立即 `POST /enroll/confirm`（走 mTLS），那才算通讯真正打通。

use crate::creds::Credentials;
use crate::error::{Error, Result};
use crate::transcript::{self, POP_ALGORITHM};
use crate::trust::{Pin, TrustOutcome, TrustStore};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn is_loopback_host(authority: &str) -> bool {
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = host
        .trim_start_matches('[')
        .split(']')
        .next()
        .unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    host == "localhost" || host.starts_with("127.") || host == "::1"
}

/// 从注册链接解析出的上下文。
#[derive(Debug, Clone)]
pub struct EnrollTarget {
    pub endpoint: String,
    pub token: String,
    pub slot: String,
    pub pin: Option<Pin>,
}

impl EnrollTarget {
    /// 解析 `https://host:port/enroll?token=T&slot=S[&pin=sha256/...]`
    pub fn parse(link: &str) -> Result<Self> {
        let (scheme, rest) = link
            .split_once("://")
            .ok_or_else(|| Error::InvalidLink("缺少 scheme".into()))?;
        if scheme != "https" && scheme != "http" {
            return Err(Error::InvalidLink(format!("不支持的 scheme: {scheme}")));
        }
        let (authority, path) = rest
            .split_once('/')
            .ok_or_else(|| Error::InvalidLink("缺少路径".into()))?;
        // 注册通道传的是一次性 token + CSR，明文 http 等于把它们裸奔出去。
        // 只有回环地址（本地联调）允许 http。
        if scheme == "http" && !is_loopback_host(authority) {
            return Err(Error::InvalidLink(
                "注册链接必须是 https（仅 localhost/127.0.0.1 允许 http）".into(),
            ));
        }
        let query = path
            .split_once('?')
            .map(|(_, q)| q)
            .ok_or_else(|| Error::InvalidLink("缺少查询参数".into()))?;

        let mut token = None;
        let mut slot = None;
        let mut pin = None;
        for pair in query.split('&') {
            let (k, v) = match pair.split_once('=') {
                Some(kv) => kv,
                None => continue,
            };
            let v = percent_decode(v);
            match k {
                "token" => token = Some(v),
                "slot" => slot = Some(v),
                "pin" => pin = Some(Pin::parse(&v)?),
                _ => {}
            }
        }

        Ok(Self {
            endpoint: format!("{scheme}://{authority}"),
            token: token.ok_or_else(|| Error::InvalidLink("缺少 token".into()))?,
            slot: slot.ok_or_else(|| Error::InvalidLink("缺少 slot".into()))?,
            pin,
        })
    }
}

/// 逐**字节**解码百分号转义。
///
/// 不能用 `&s[i+1..i+3]` 这种字符串切片：`%` 后面跟多字节 UTF-8 时会切在
/// 字符边界之外直接 panic（解析注册链接就崩）。这里只看字节，最后再 lossy。
fn percent_decode(s: &str) -> String {
    fn hex_val(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }

    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug, Serialize)]
struct ClientInfo {
    hostname: String,
    machine_id: String,
}

#[derive(Debug, Serialize)]
struct ChallengeRequest<'a> {
    token: &'a str,
    slot: &'a str,
    client_info: ClientInfo,
}

#[derive(Debug, Deserialize)]
struct ChallengeReply {
    challenge_id: String,
    challenge: String,
    ttl_secs: u64,
    kind: String,
    /// approval 型：服务端在 challenge 阶段就给出注册单 id，客户端应立刻落盘
    #[serde(default)]
    enroll_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct EnrollRequest<'a> {
    challenge_id: &'a str,
    slot: &'a str,
    token: &'a str,
    csr_der: String,
    pop_signature: String,
    pop_algorithm: &'a str,
    tls_cert_seen_fp: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum EnrollReply {
    Issued {
        cert_pem: String,
        ca_chain_pem: String,
        serial: String,
        #[serde(default)]
        mtls_endpoint: Option<String>,
    },
    Pending {
        enroll_id: String,
        poll_after_secs: u64,
    },
}

#[derive(Debug, Deserialize)]
struct StatusReply {
    status: String,
    cert_pem: Option<String>,
    ca_chain_pem: Option<String>,
    serial: Option<String>,
    poll_after_secs: Option<u64>,
    /// 服务端下发的 mTLS 端点；缺了它 approval 型拿到证书也 confirm 不了
    #[serde(default)]
    mtls_endpoint: Option<String>,
}

#[derive(Debug)]
pub enum EnrollOutcome {
    Issued(Credentials),
    PendingApproval { enroll_id: String },
}

/// 注册选项。
#[derive(Debug, Clone)]
pub struct EnrollOptions {
    /// approval 型最长等待多久（None = 不等待，直接返回 pending）
    pub approval_wait: Option<Duration>,
    /// 用户已确认信任（TOFU 场景由 CLI 传入）
    pub trust_confirmed: bool,
    /// 数据目录覆盖（测试用）
    pub data_dir: Option<std::path::PathBuf>,
}

impl Default for EnrollOptions {
    fn default() -> Self {
        Self {
            approval_wait: Some(Duration::from_secs(300)),
            trust_confirmed: false,
            data_dir: None,
        }
    }
}

/// 执行完整注册。
pub async fn enroll(target: &EnrollTarget, opts: &EnrollOptions) -> Result<EnrollOutcome> {
    // 0. 先探一次服务端身份（拿 tls_cert_seen_fp），
    //    再把该指纹钉进**真正携带 token 的那条连接**。
    //    之前两者是分离的：pin/TOFU 只校验独立探测连接，传凭证的 reqwest 连接
    //    却 `danger_accept_invalid_certs(true)`，攻击者可以两条连接出示不同证书。
    let trust = verify_server_identity(target, opts).await?;
    if trust.seen_fp.is_empty() {
        return Err(Error::Transport(
            "无法取得服务端证书指纹，拒绝在未校验身份的连接上传送注册 token".into(),
        ));
    }
    let http = build_enroll_client(&trust.seen_fp)?;

    // 1. 请求挑战值
    let client_info = collect_client_info();
    let challenge_url = format!("{}/enroll/challenge", target.endpoint);
    let resp = http
        .post(&challenge_url)
        .json(&ChallengeRequest {
            token: &target.token,
            slot: &target.slot,
            client_info,
        })
        .send()
        .await
        .map_err(|e| Error::Transport(e.to_string()))?;
    let resp = check_status(resp).await?;
    let chal: ChallengeReply = resp
        .json()
        .await
        .map_err(|e| Error::Transport(e.to_string()))?;
    tracing::debug!(kind = %chal.kind, ttl_secs = chal.ttl_secs, "challenge issued");

    // 2. 生成密钥 + CSR + PoP
    let key = transcript::DeviceKey::generate()?;
    let csr_der = build_csr(&target.slot, &key)?;
    let csr_sha = transcript::sha256(&csr_der);
    let token_hash = transcript::sha256(target.token.as_bytes());
    let challenge_bytes = hex::decode(&chal.challenge)
        .map_err(|e| Error::Unexpected(format!("challenge 不是合法 hex: {e}")))?;
    let fp_bytes = hex::decode(&trust.seen_fp).unwrap_or_default();

    let msg = transcript::canonical_transcript(
        &challenge_bytes,
        &token_hash,
        &target.slot,
        &csr_sha,
        &fp_bytes,
    );
    let pop_sig = key.sign(&msg)?;

    // 2.5 私钥立刻落盘。CSR 一旦提交，私钥就再也换不回来（token 已消费），
    //     若只在"拿到证书后"才存盘，等待审批期间进程一退出这台设备就永久注册不上。
    let mut pending = PendingEnrollment {
        slot: target.slot.clone(),
        endpoint: target.endpoint.clone(),
        key_pem: key_to_pem(&key)?,
        enroll_id: chal.enroll_id.clone(),
    };
    save_pending(&pending, opts.data_dir.as_deref())?;

    // 3. 提交注册
    let b64 = base64::engine::general_purpose::STANDARD;
    let enroll_url = format!("{}/enroll", target.endpoint);
    let resp = http
        .post(&enroll_url)
        .json(&EnrollRequest {
            challenge_id: &chal.challenge_id,
            slot: &target.slot,
            token: &target.token,
            csr_der: b64.encode(&csr_der),
            pop_signature: b64.encode(&pop_sig),
            pop_algorithm: POP_ALGORITHM,
            tls_cert_seen_fp: trust.seen_fp.clone(),
        })
        .send()
        .await
        .map_err(|e| Error::Transport(e.to_string()))?;
    let resp = check_status(resp).await?;
    let reply: EnrollReply = resp
        .json()
        .await
        .map_err(|e| Error::Transport(e.to_string()))?;

    match reply {
        EnrollReply::Issued {
            cert_pem,
            ca_chain_pem,
            serial,
            mtls_endpoint,
        } => {
            let creds = persist(
                target,
                &key,
                cert_pem,
                ca_chain_pem,
                serial,
                mtls_endpoint,
                opts,
            )?;
            clear_pending(&target.slot, opts.data_dir.as_deref())?;
            Ok(EnrollOutcome::Issued(creds))
        }
        EnrollReply::Pending {
            enroll_id,
            poll_after_secs,
        } => {
            pending.enroll_id = Some(enroll_id.clone());
            save_pending(&pending, opts.data_dir.as_deref())?;
            match opts.approval_wait {
                None => Ok(EnrollOutcome::PendingApproval { enroll_id }),
                Some(max_wait) => {
                    let (cert_pem, ca_chain_pem, serial, mtls_endpoint) =
                        poll_until_approved(&http, target, &enroll_id, poll_after_secs, max_wait)
                            .await?;
                    let creds = persist(
                        target,
                        &key,
                        cert_pem,
                        ca_chain_pem,
                        serial,
                        mtls_endpoint,
                        opts,
                    )?;
                    clear_pending(&target.slot, opts.data_dir.as_deref())?;
                    Ok(EnrollOutcome::Issued(creds))
                }
            }
        }
    }
}

fn persist(
    target: &EnrollTarget,
    key: &transcript::DeviceKey,
    cert_pem: String,
    ca_chain_pem: String,
    serial: String,
    mtls_endpoint: Option<String>,
    opts: &EnrollOptions,
) -> Result<Credentials> {
    let key_pem = key_to_pem(key)?;
    let creds = Credentials {
        device_slot: target.slot.clone(),
        serial,
        cert_pem,
        key_pem,
        ca_chain_pem,
        endpoint: target.endpoint.clone(),
        // 服务端返回的 mTLS 端点优先；缺失时退化为注册端点（并打警告）
        mtls_endpoint: match mtls_endpoint {
            Some(e) if !e.is_empty() => e,
            _ => {
                tracing::warn!(
                    "服务端未返回 mtls_endpoint，回退到注册端点 —— \
                     confirm 很可能失败，请检查服务端 PUBLIC_MTLS_ENDPOINT 配置"
                );
                target.endpoint.clone()
            }
        },
    };
    // 路径必须和 `creds::locate` 走同一套 sanitize：否则 slot 里带 `/`、空格时，
    // 保存路径与加载路径不同（刚注册完 confirm 就报"未注册"），甚至写出数据目录之外。
    let dir = match &opts.data_dir {
        Some(d) => d.join("creds").join(crate::creds::sanitize(&target.slot)),
        None => Credentials::dir_for(&target.slot)?,
    };
    creds.save(&dir)?;
    Ok(creds)
}

/// 轮询审批结果，返回 `(cert_pem, ca_chain_pem, serial, mtls_endpoint)`。
///
/// 瞬时错误（网络抖动、响应体解析失败、服务端 5xx）**不终止流程**：一次抖动就
/// 退出会让"证书已在服务端备好"变成永久损失——服务端此时已置 downloaded_at，
/// 客户端又没落盘，token 也已消费。这里最多容忍 `MAX_TRANSIENT_FAILURES` 次。
async fn poll_until_approved(
    http: &reqwest::Client,
    target: &EnrollTarget,
    enroll_id: &str,
    poll_after_secs: u64,
    max_wait: Duration,
) -> Result<(String, String, String, Option<String>)> {
    const MAX_TRANSIENT_FAILURES: u32 = 5;

    let deadline = std::time::Instant::now() + max_wait;
    let mut interval = Duration::from_secs(poll_after_secs.max(1));
    let url = format!("{}/enroll/{enroll_id}", target.endpoint);
    let mut transient = 0u32;

    loop {
        if std::time::Instant::now() >= deadline {
            return Err(Error::PendingApproval(enroll_id.to_owned()));
        }
        tokio::time::sleep(interval).await;
        // 睡醒后再判一次：interval 可能比剩余时间长，先睡过 deadline
        // 再发请求等于白消耗一次（对一次性资源尤其不划算）。
        if std::time::Instant::now() >= deadline {
            return Err(Error::PendingApproval(enroll_id.to_owned()));
        }

        let resp = match http.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                transient += 1;
                if transient > MAX_TRANSIENT_FAILURES {
                    return Err(Error::Transport(format!(
                        "轮询连续失败 {transient} 次：{e}（可用 `poll --slot` 续跑）"
                    )));
                }
                tracing::warn!(attempt = transient, error = %e, "轮询瞬时失败，继续重试");
                continue;
            }
        };
        if resp.status() == reqwest::StatusCode::GONE {
            return Err(Error::AlreadyDownloaded);
        }
        if resp.status().is_server_error() {
            transient += 1;
            if transient > MAX_TRANSIENT_FAILURES {
                return Err(Error::Unexpected(format!(
                    "轮询连续 {transient} 次服务端 {}（可用 `poll --slot` 续跑）",
                    resp.status()
                )));
            }
            tracing::warn!(attempt = transient, status = %resp.status(), "服务端暂不可用，继续重试");
            continue;
        }
        let resp = check_status(resp).await?;
        let st: StatusReply = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                transient += 1;
                if transient > MAX_TRANSIENT_FAILURES {
                    return Err(Error::Transport(format!("轮询响应解析连续失败：{e}")));
                }
                tracing::warn!(attempt = transient, error = %e, "轮询响应解析失败，继续重试");
                continue;
            }
        };
        transient = 0;
        // 服务端可以动态调整轮询节奏
        if let Some(p) = st.poll_after_secs.filter(|p| *p > 0) {
            interval = Duration::from_secs(p);
        }

        match st.status.as_str() {
            "pending" => continue,
            "rejected" => return Err(Error::ApprovalRejected),
            "approved" => {
                let cert = st
                    .cert_pem
                    .ok_or_else(|| Error::Unexpected("approved 但无 cert_pem".into()))?;
                let ca = st
                    .ca_chain_pem
                    .ok_or_else(|| Error::Unexpected("approved 但无 ca_chain_pem".into()))?;
                let serial = st
                    .serial
                    .ok_or_else(|| Error::Unexpected("approved 但无 serial".into()))?;
                return Ok((cert, ca, serial, st.mtls_endpoint));
            }
            other => return Err(Error::Unexpected(format!("未知状态 {other}"))),
        }
    }
}

// ---------------- 待审批状态持久化与续跑 ----------------

/// 一次进行中的 approval 注册。私钥必须在这里就落盘，
/// 否则等待审批期间进程退出 = CSR 已提交却换不回私钥 = 这次注册作废。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingEnrollment {
    pub slot: String,
    pub endpoint: String,
    /// PKCS#8 PEM
    pub key_pem: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enroll_id: Option<String>,
}

fn pending_path(slot: &str, data_dir: Option<&Path>) -> Result<PathBuf> {
    let base = match data_dir {
        Some(d) => d.to_path_buf(),
        None => crate::trust::default_data_dir()?,
    };
    Ok(base
        .join("pending")
        .join(format!("{}.json", crate::creds::sanitize(slot))))
}

pub fn save_pending(p: &PendingEnrollment, data_dir: Option<&Path>) -> Result<()> {
    let path = pending_path(&p.slot, data_dir)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(p)?;
    crate::trust::write_private(&path, &json)?;
    Ok(())
}

pub fn load_pending(slot: &str, data_dir: Option<&Path>) -> Result<Option<PendingEnrollment>> {
    let path = pending_path(slot, data_dir)?;
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Storage(e.to_string())),
    }
}

pub fn clear_pending(slot: &str, data_dir: Option<&Path>) -> Result<()> {
    let path = pending_path(slot, data_dir)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Storage(e.to_string())),
    }
}

/// 续跑一次被中断的 approval 注册（重新轮询直到批准并落盘凭证）。
///
/// `enroll_id` 与私钥都来自 `pending/` 里的落盘状态，因此不需要原注册链接
/// （token 此时也早已消费）。
pub async fn resume_poll(
    slot: &str,
    data_dir: Option<&Path>,
    opts: &EnrollOptions,
) -> Result<Credentials> {
    let state = load_pending(slot, data_dir)?.ok_or_else(|| {
        Error::Unexpected(format!(
            "没有 {slot} 的待审批注册记录（{}）。请用注册链接重新 enroll",
            pending_path(slot, data_dir)
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        ))
    })?;
    let enroll_id = state.enroll_id.clone().ok_or_else(|| {
        Error::Unexpected("pending 记录缺少 enroll_id（enroll 未完成提交）".into())
    })?;

    // 复用同一套身份校验：注册通道仍需 pin/TOFU 校验，只是这次不传 token
    let target = EnrollTarget {
        endpoint: state.endpoint.clone(),
        token: String::new(),
        slot: state.slot.clone(),
        pin: None,
    };
    let trust = verify_server_identity(&target, opts).await?;
    let http = build_enroll_client(&trust.seen_fp)?;

    let wait = opts.approval_wait.unwrap_or(Duration::from_secs(300));
    let (cert_pem, ca_chain_pem, serial, mtls_endpoint) =
        poll_until_approved(&http, &target, &enroll_id, 5, wait).await?;

    let key = key_from_pem(&state.key_pem)?;
    let creds = persist(
        &target,
        &key,
        cert_pem,
        ca_chain_pem,
        serial,
        mtls_endpoint,
        opts,
    )?;
    clear_pending(slot, data_dir)?;
    Ok(creds)
}

/// PKCS#8 PEM → DeviceKey。
fn key_from_pem(pem: &str) -> Result<transcript::DeviceKey> {
    use base64::engine::general_purpose::STANDARD;
    let body: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    let der = STANDARD
        .decode(body.trim())
        .map_err(|e| Error::Crypto(format!("pending 私钥 base64 解码失败: {e}")))?;
    transcript::DeviceKey::from_pkcs8(der)
}

/// 构建**注册通道** HTTP 客户端：整条连接钉死在 `pinned_fp` 上。
///
/// 自定义 verifier 只接受与已校验指纹逐字节相同的证书 —— 比"信任某个 CA"更强，
/// 且必须用在真正发 token/CSR 的这条连接上（`use_preconfigured_tls`）。
/// 服务端侧还会再比一次 `tls_cert_seen_fp`，两端口径一致。
fn build_enroll_client(pinned_fp: &str) -> Result<reqwest::Client> {
    // 未安装 provider 时（单测/二次调用）安装失败也无妨
    let _ = rustls::crypto::ring::default_provider().install_default();

    let tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(PinnedServerCert {
            expected_fp: pinned_fp.to_ascii_lowercase(),
        }))
        .with_no_client_auth();

    reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Transport(e.to_string()))
}

/// 只接受"与已校验指纹相同"的服务端证书。
#[derive(Debug)]
struct PinnedServerCert {
    expected_fp: String,
}

impl rustls::client::danger::ServerCertVerifier for PinnedServerCert {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let seen = crate::trust::cert_fp_hex(end_entity.as_ref());
        if seen.eq_ignore_ascii_case(&self.expected_fp) {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "服务端证书与已校验指纹不符（expected {}，seen {seen}）",
                self.expected_fp
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// 服务端身份校验：pin → TOFU 记录 → 人工确认。
async fn verify_server_identity(
    target: &EnrollTarget,
    opts: &EnrollOptions,
) -> Result<TrustOutcome> {
    // 探测连接拿服务端证书 DER（reqwest 不暴露对端证书，这里用 tokio-rustls 直连）。
    // 注意：这里只负责**判定**，强制发生在 `build_enroll_client` —— 判出来的指纹
    // 会被钉进真正传 token 的那条连接，否则校验的连接和传凭证的连接不是同一条。
    let endpoint_key = target.endpoint.clone();
    let store_path = match &opts.data_dir {
        Some(d) => d.join("trust-store.json"),
        None => TrustStore::path()?,
    };
    let mut store = TrustStore::load(&store_path)?;

    // 拿不到服务端证书就**硬失败**。早先这里是"降级为无指纹继续走"，
    // 等于 pin 语义被静默跳过（带 pin 时 needs_confirmation=false），
    // 而注释还写着"服务端会放过空指纹"——服务端早已不放过了。
    let der = probe_server_cert(&target.endpoint)
        .await?
        .ok_or_else(|| Error::Transport("探测连接未返回服务端证书，无法校验身份".into()))?;

    let outcome = store.check(&endpoint_key, target.pin.as_ref(), &der)?;
    // 无论走哪条路径都把判定结果打出来：指纹是运维唯一能带外核对的依据
    tracing::info!(fp = %outcome.seen_fp, reason = %outcome.reason, "服务端身份校验");

    if outcome.needs_confirmation {
        // 之前的 TOFU 是"盲确认"：`--trust` 是个布尔开关，指纹从不展示，
        // 用户在看不到任何信息的情况下就写进了信任库。
        eprintln!("服务端证书指纹（SHA-256）：{}", outcome.seen_fp);
        eprintln!("判定原因：{}", outcome.reason);
        let confirmed = if opts.trust_confirmed {
            eprintln!("（已指定 --trust，跳过交互确认）");
            true
        } else if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            dialoguer::Confirm::new()
                .with_prompt("请人工核对上面的指纹（建议带外确认）后决定是否信任该服务端")
                .default(false)
                .interact()
                .unwrap_or_else(|e| {
                    eprintln!("交互确认失败：{e}");
                    false
                })
        } else {
            eprintln!("非交互环境：请人工核对上面的指纹后，加 --trust 重新执行");
            false
        };
        if !confirmed {
            return Err(Error::TrustRejected);
        }
        if !outcome.seen_fp.is_empty() {
            store.remember(&endpoint_key, &outcome.seen_fp);
            store.save(&store_path)?;
            eprintln!("✓ 已写入信任库：{}", store_path.display());
        }
    }
    Ok(outcome)
}

/// 探测服务端证书 DER。纯 Rust TLS 栈不暴露对端证书时返回 None。
async fn probe_server_cert(endpoint: &str) -> Result<Option<Vec<u8>>> {
    // 用 tokio-rustls 直连一次，从连接里读对端证书。
    let host_port = endpoint
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_owned();

    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    // 主动接受自签证书：这里只取证书做指纹比对，信任判定在 TrustStore
    cfg.dangerous()
        .set_certificate_verifier(std::sync::Arc::new(AcceptAnyCert));
    let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(cfg));

    let server_name = match host_port.split(':').next() {
        Some(h) => rustls::pki_types::ServerName::try_from(h.to_owned())
            .map_err(|e| Error::Transport(format!("invalid host: {e}")))?,
        None => return Ok(None),
    };

    let tcp = match tokio::net::TcpStream::connect(&host_port).await {
        Ok(t) => t,
        Err(e) => return Err(Error::Transport(format!("connect {host_port}: {e}"))),
    };
    let tls = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| Error::Transport(format!("tls handshake: {e}")))?;
    let der = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|c| c.first())
        .map(|c| c.as_ref().to_vec());
    Ok(der)
}

/// 接受任意证书的 verifier —— 仅用于"先取证书再比对指纹"的场景。
#[derive(Debug)]
struct AcceptAnyCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

async fn check_status(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let code = resp
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|v| v["code"].as_str().map(|s| s.to_owned()))
        .unwrap_or_default();
    Err(match code.as_str() {
        "token_invalid" => Error::TokenInvalid,
        "slot_conflict" => Error::SlotConflict,
        "challenge_expired" => Error::ChallengeExpired,
        "challenge_consumed" => Error::ChallengeConsumed,
        "challenge_mismatch" => Error::ChallengeMismatch,
        "pop_invalid" => Error::PopRejected,
        "fp_mismatch" => Error::FingerprintMismatch,
        "approval_rejected" => Error::ApprovalRejected,
        "already_downloaded" => Error::AlreadyDownloaded,
        "enrollment_not_found" => Error::Unexpected(
            "注册单不存在（enroll_id 失效或已被管理员清理）。用 `poll --slot <slot>` 续跑，或重新 enroll".into(),
        ),
        "unauthorized" => Error::Unexpected("鉴权失败（HTTP 401）".into()),
        "bad_request" => Error::Unexpected(format!(
            "服务端拒绝请求（HTTP {status} bad_request），通常是请求体不合法"
        )),
        "internal" => Error::Unexpected(format!(
            "服务端内部错误（HTTP {status} internal），稍后重试"
        )),
        other => Error::Unexpected(format!("HTTP {status} code={other}")),
    })
}

fn collect_client_info() -> ClientInfo {
    ClientInfo {
        hostname: hostname(),
        machine_id: machine_id(),
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_owned())
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "unknown".into())
}

fn machine_id() -> String {
    std::fs::read_to_string("/etc/machine-id")
        .or_else(|_| std::fs::read_to_string("/var/lib/dbus/machine-id"))
        .map(|s| s.trim().to_owned())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// 用 rcgen 生成 CSR（复用 transcript 里的 P-256 私钥）。
fn build_csr(slot: &str, key: &transcript::DeviceKey) -> Result<Vec<u8>> {
    use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};

    // 把 PKCS#8 交给 rcgen
    let key_pem = key_to_pem(key)?;
    let rc_key =
        KeyPair::from_pem(&key_pem).map_err(|e| Error::Crypto(format!("rcgen load key: {e}")))?;

    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, slot);
    params.distinguished_name = dn;

    let csr = params
        .serialize_request(&rc_key)
        .map_err(|e| Error::Crypto(format!("build CSR: {e}")))?;
    Ok(csr.der().to_vec())
}

/// PKCS#8 → PEM。
fn key_to_pem(key: &transcript::DeviceKey) -> Result<String> {
    use base64::engine::general_purpose::STANDARD;
    let b64 = STANDARD.encode(key.pkcs8());
    let mut out = String::from("-----BEGIN PRIVATE KEY-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push('\n');
    }
    out.push_str("-----END PRIVATE KEY-----\n");
    Ok(out)
}

/// 便捷入口：解析链接 + 注册。
pub async fn enroll_link(link: &str, opts: &EnrollOptions) -> Result<EnrollOutcome> {
    let target = EnrollTarget::parse(link)?;
    enroll(&target, opts).await
}

/// 供 CLI 显示链接摘要（不泄漏 token；只留 4 位便于核对是哪一张链接）。
pub fn link_summary(link: &str) -> Result<String> {
    let t = EnrollTarget::parse(link)?;
    let masked: String = t.token.chars().take(4).collect();
    Ok(format!(
        "endpoint={} slot={} pin={} token={masked}…(已遮蔽)",
        t.endpoint,
        t.slot,
        t.pin.is_some(),
    ))
}

/// 落盘目录解析（CLI 用）。
pub fn creds_dir(slot: &str, data_dir: Option<&Path>) -> Result<std::path::PathBuf> {
    match data_dir {
        Some(d) => Ok(d.join("creds").join(slot)),
        None => Credentials::dir_for(slot),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 续跑（`poll`）依赖两件事：pending 状态能落盘读回、
    /// 且能从里面还原出**当初提交 CSR 的那把私钥**——否则拿到证书也用不了。
    #[test]
    fn pending_state_roundtrips_and_key_survives() {
        let dir = tempfile::tempdir().unwrap();
        let key = transcript::DeviceKey::generate().unwrap();
        let pending = PendingEnrollment {
            slot: "slot-x".into(),
            endpoint: "https://n:4440".into(),
            key_pem: key_to_pem(&key).unwrap(),
            enroll_id: Some(uuid::Uuid::new_v4().to_string()),
        };
        save_pending(&pending, Some(dir.path())).unwrap();

        let back = load_pending("slot-x", Some(dir.path()))
            .unwrap()
            .expect("pending 应可读回");
        assert_eq!(back.enroll_id, pending.enroll_id);
        let restored = key_from_pem(&back.key_pem).expect("私钥应可还原");
        assert_eq!(
            restored.public_key_der(),
            key.public_key_der(),
            "续跑必须复用同一把私钥，否则 CSR 对不上"
        );

        clear_pending("slot-x", Some(dir.path())).unwrap();
        assert!(
            load_pending("slot-x", Some(dir.path())).unwrap().is_none(),
            "注册完成后 pending 状态应被清掉"
        );
    }

    /// 注册链接只允许 https（回环地址除外）。
    #[test]
    fn plain_http_link_rejected_off_loopback() {
        let long = "a".repeat(64);
        assert!(
            EnrollTarget::parse(&format!("http://evil.example/enroll?token={long}&slot=s"))
                .is_err(),
            "非回环 http 必须被拒"
        );
        assert!(
            EnrollTarget::parse(&format!("http://localhost:4440/enroll?token={long}&slot=s"))
                .is_ok(),
            "本地联调的 http 应放行"
        );
    }

    /// 百分号解码必须按字节处理：`%` 后面跟多字节 UTF-8 时不能 panic。
    #[test]
    fn percent_decode_survives_multibyte_after_percent() {
        let out = percent_decode("%中");
        assert_eq!(out, "%中", "不合法的转义应原样保留");

        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("a+b"), "a b");
    }

    #[test]
    fn parse_link_full() {
        let t = EnrollTarget::parse(
            "https://node.example:4440/enroll?token=abc123&slot=slot-1&pin=sha256/aa",
        );
        // pin 太短应报错
        assert!(t.is_err());

        let t = EnrollTarget::parse("https://node.example:4440/enroll?token=abc123&slot=slot-1")
            .unwrap();
        assert_eq!(t.endpoint, "https://node.example:4440");
        assert_eq!(t.token, "abc123");
        assert_eq!(t.slot, "slot-1");
        assert!(t.pin.is_none());
    }

    #[test]
    fn parse_link_with_valid_pin() {
        let pin_hex = "cd".repeat(32);
        let t = EnrollTarget::parse(&format!(
            "https://node:4440/enroll?token=t&slot=s&pin=sha256/{pin_hex}"
        ))
        .unwrap();
        assert_eq!(t.pin, Some(Pin::CertSha256(pin_hex)));
    }

    #[test]
    fn parse_link_rejects_bad_input() {
        assert!(EnrollTarget::parse("no-scheme").is_err());
        assert!(EnrollTarget::parse("ftp://x/y?token=a&slot=b").is_err());
        assert!(EnrollTarget::parse("https://x/enroll?slot=b").is_err());
        assert!(EnrollTarget::parse("https://x/enroll?token=a").is_err());
        assert!(EnrollTarget::parse("https://x/enroll").is_err());
    }

    #[test]
    fn percent_decoding_works() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn link_summary_hides_token() {
        let s = link_summary("https://n:1/enroll?token=supersecrettoken&slot=s").unwrap();
        assert!(s.contains("slot=s"));
        assert!(!s.contains("supersecrettoken"));
        // 只留 4 位前缀便于核对是哪一张链接，其余全部遮蔽
        assert!(s.contains("supe…"), "实际: {s}");
        assert!(!s.contains("secrettoken"));
    }

    #[test]
    fn csr_built_with_cn_equal_slot() {
        use x509_parser::prelude::*;
        let key = transcript::DeviceKey::generate().unwrap();
        let csr_der = build_csr("slot-7", &key).unwrap();
        let (_, csr) = X509CertificationRequest::from_der(&csr_der).unwrap();
        let info = &csr.certification_request_info;
        let cn = info.subject.iter_common_name().next().unwrap();
        assert_eq!(cn.as_str().unwrap(), "slot-7");
    }

    #[test]
    fn csr_public_key_is_full_spki_containing_raw_key() {
        let key = transcript::DeviceKey::generate().unwrap();
        let csr_der = build_csr("s", &key).unwrap();
        use x509_parser::prelude::*;
        let (_, csr) = X509CertificationRequest::from_der(&csr_der).unwrap();
        let spki = csr.certification_request_info.subject_pki.raw;
        // CSR 里是完整 SPKI（含 AlgorithmIdentifier），ring 给的是裸公钥位
        assert!(spki.len() > key.public_key_der().len());
        assert!(
            spki.ends_with(key.public_key_der()),
            "SPKI 应包含裸公钥作为结尾"
        );
    }

    #[test]
    fn key_to_pem_shape() {
        let key = transcript::DeviceKey::generate().unwrap();
        let pem = key_to_pem(&key).unwrap();
        assert!(pem.starts_with("-----BEGIN PRIVATE KEY-----\n"));
        assert!(pem.ends_with("-----END PRIVATE KEY-----\n"));
        // rcgen 应该能解析回去
        assert!(rcgen::KeyPair::from_pem(&pem).is_ok());
    }
}
