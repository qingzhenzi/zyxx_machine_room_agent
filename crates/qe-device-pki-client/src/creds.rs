//! 本地凭证持久化：证书、私钥、CA 链，全部 0600。

use crate::error::{Error, Result};
use crate::trust::{default_data_dir, write_private};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Credentials {
    pub device_slot: String,
    pub serial: String,
    pub cert_pem: String,
    pub key_pem: String,
    pub ca_chain_pem: String,
    /// 注册端点（仅用于排错）
    pub endpoint: String,
    /// mTLS 通讯端点（后续所有业务连这里）
    pub mtls_endpoint: String,
}

impl Credentials {
    /// 默认落盘目录：`<data_dir>/creds/<slot>/`
    pub fn dir_for(slot: &str) -> Result<PathBuf> {
        Ok(default_data_dir()?.join("creds").join(sanitize(slot)))
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        write_private(&dir.join("cert.pem"), self.cert_pem.as_bytes())?;
        write_private(&dir.join("key.pem"), self.key_pem.as_bytes())?;
        write_private(&dir.join("ca-chain.pem"), self.ca_chain_pem.as_bytes())?;
        let meta = serde_json::json!({
            "device_slot": self.device_slot,
            "serial": self.serial,
            "endpoint": self.endpoint,
            "mtls_endpoint": self.mtls_endpoint,
        });
        write_private(
            &dir.join("meta.json"),
            serde_json::to_string_pretty(&meta)?.as_bytes(),
        )?;
        Ok(())
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let cert_pem = read_required(&dir.join("cert.pem"))?;
        let key_pem = read_required(&dir.join("key.pem"))?;
        let ca_chain_pem = read_required(&dir.join("ca-chain.pem"))?;
        let meta_raw = read_required(&dir.join("meta.json"))?;
        let meta: serde_json::Value = serde_json::from_str(&meta_raw)?;
        Ok(Self {
            device_slot: meta["device_slot"].as_str().unwrap_or_default().to_owned(),
            serial: meta["serial"].as_str().unwrap_or_default().to_owned(),
            cert_pem,
            key_pem,
            ca_chain_pem,
            endpoint: meta["endpoint"].as_str().unwrap_or_default().to_owned(),
            // 老版本凭证没有这个字段时回退到注册端点，避免读取失败
            mtls_endpoint: meta["mtls_endpoint"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| meta["endpoint"].as_str().unwrap_or_default())
                .to_owned(),
        })
    }

    pub fn exists(dir: &Path) -> bool {
        dir.join("cert.pem").exists() && dir.join("key.pem").exists()
    }

    /// 启动自检：文件齐全、PEM 可解析、私钥与证书匹配、未过期。
    ///
    /// 返回 `Ok(())` 表示可以安全用于 mTLS；否则给出分类问题。
    pub fn verify(&self) -> std::result::Result<(), CredIssue> {
        use x509_parser::prelude::*;

        let chain = self
            .cert_chain_der()
            .map_err(|e| CredIssue::Corrupt(format!("证书链: {e}")))?;
        let leaf = chain
            .first()
            .ok_or_else(|| CredIssue::Corrupt("证书链为空".into()))?;

        let key = self
            .private_key_der()
            .map_err(|e| CredIssue::Corrupt(format!("私钥: {e}")))?;

        let (_, parsed) = X509Certificate::from_der(leaf.as_ref())
            .map_err(|e| CredIssue::Corrupt(format!("证书 DER: {e}")))?;

        // 私钥与证书必须匹配。否则 mTLS 握手会以一个含糊的 TLS 错误失败，
        // 运维很难定位。这里用私钥重新导出公钥，与证书 SPKI 逐字节比对。
        match key_match(&key, &parsed) {
            Ok(true) => {}
            Ok(false) => {
                return Err(CredIssue::Corrupt(
                    "私钥与证书公钥不匹配（cert.pem/key.pem 可能来自不同次注册）".into(),
                ));
            }
            Err(e) => {
                // 该私钥类型不支持导出公钥时，不阻断（交由握手校验）
                tracing::debug!("跳过私钥/证书比对: {e}");
            }
        }

        let now = ASN1Time::now();
        if !parsed.validity().is_valid_at(now) {
            return Err(CredIssue::Expired {
                not_after: parsed.validity().not_after.to_string(),
            });
        }

        Ok(())
    }

    /// 私钥 PEM 用 PKCS#8 包装，便于 rustls 直接解析。
    pub fn cert_chain_der(&self) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
        let mut rd = std::io::BufReader::new(self.cert_pem.as_bytes());
        let certs: std::result::Result<Vec<_>, _> = rustls_pemfile::certs(&mut rd).collect();
        certs.map_err(|e| Error::Storage(format!("parse cert.pem: {e}")))
    }

    pub fn private_key_der(&self) -> Result<rustls::pki_types::PrivateKeyDer<'static>> {
        let mut rd = std::io::BufReader::new(self.key_pem.as_bytes());
        rustls_pemfile::private_key(&mut rd)
            .map_err(|e| Error::Storage(format!("parse key.pem: {e}")))?
            .ok_or_else(|| Error::Storage("key.pem 中没有私钥".into()))
    }

    /// 从证书中提取 CN（设备槽位标识）
    pub fn extract_cn_from_cert_pem(cert_pem: &str) -> Result<String> {
        use x509_parser::prelude::*;
        let mut rd = std::io::BufReader::new(cert_pem.as_bytes());
        let certs: std::result::Result<Vec<_>, _> = rustls_pemfile::certs(&mut rd).collect();
        let certs = certs.map_err(|e| Error::Storage(format!("parse cert.pem: {e}")))?;
        let der = certs
            .first()
            .ok_or_else(|| Error::Storage("证书链为空".into()))?;
        let (_, parsed) = X509Certificate::from_der(der.as_ref())
            .map_err(|e| Error::Storage(format!("证书 DER: {e}")))?;
        let cn = parsed
            .subject()
            .iter_common_name()
            .next()
            .and_then(|attr| attr.as_str().ok())
            .ok_or_else(|| Error::Storage("证书中未找到 CN".into()))?;
        Ok(cn.to_owned())
    }

    /// 从本地凭证目录加载并从证书提取 CN
    pub fn load_with_cn(dir: &Path) -> Result<(Self, String)> {
        let creds = Self::load(dir)?;
        let cn = Self::extract_cn_from_cert_pem(&creds.cert_pem)?;
        Ok((creds, cn))
    }

    pub fn ca_roots(&self) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
        let mut rd = std::io::BufReader::new(self.ca_chain_pem.as_bytes());
        let certs: std::result::Result<Vec<_>, _> = rustls_pemfile::certs(&mut rd).collect();
        certs.map_err(|e| Error::Storage(format!("parse ca-chain.pem: {e}")))
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredIssue {
    /// 从未注册过
    NotEnrolled,
    /// 文件缺失（部分损坏）
    MissingFiles(Vec<String>),
    /// 内容不可解析（PEM 损坏 / 私钥与证书不匹配）
    Corrupt(String),
    /// 已过期
    Expired { not_after: String },
}

impl CredIssue {
    /// 给运维看的修复建议。
    pub fn advice(&self) -> String {
        match self {
            CredIssue::NotEnrolled => "该节点尚未注册。用管理端下发的注册链接执行：\n  \
                 qe-device-pki-client enroll '<link>' --trust"
                .into(),
            CredIssue::MissingFiles(files) => format!(
                "凭证不完整（缺少 {}）。重新注册：\n  \
                 qe-device-pki-client enroll '<link>' --trust",
                files.join(", ")
            ),
            CredIssue::Corrupt(why) => format!(
                "凭证损坏（{why}）。删除凭证目录后重新注册：\n  \
                 rm -rf <creds-dir> && qe-device-pki-client enroll '<link>' --trust"
            ),
            CredIssue::Expired { not_after } => {
                format!("证书已于 {not_after} 过期。重新注册或延长有效期。")
            }
        }
    }
}

impl std::fmt::Display for CredIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredIssue::NotEnrolled => write!(f, "未注册"),
            CredIssue::MissingFiles(v) => write!(f, "缺少文件: {}", v.join(", ")),
            CredIssue::Corrupt(w) => write!(f, "凭证损坏: {w}"),
            CredIssue::Expired { not_after } => write!(f, "证书已过期: {not_after}"),
        }
    }
}

/// 凭证定位：显式路径 → 环境变量 → 用户数据目录。
///
/// 让部署系统（systemd unit / 容器）可以通过 `QE_DEVICE_PKI_DATA_DIR`
/// 或直接给目录来指定位置，而不必让每个使用方自己拼路径。
pub fn locate(slot: &str, explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = explicit {
        return Ok(dir.join("creds").join(sanitize(slot)));
    }
    if let Ok(dir) = std::env::var("QE_DEVICE_PKI_DATA_DIR")
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir).join("creds").join(sanitize(slot)));
    }
    Credentials::dir_for(slot)
}

/// 用私钥导出公钥并与证书 SPKI 比对。
///
/// 只支持 PKCS#8 包装的 Ed25519 / P-256 / P-384（本项目生成的即此类）。
fn key_match(
    key: &rustls::pki_types::PrivateKeyDer<'_>,
    cert: &x509_parser::certificate::X509Certificate<'_>,
) -> std::result::Result<bool, String> {
    use ring::signature::KeyPair as _;
    use rustls::pki_types::PrivateKeyDer;

    let pkcs8 = match key {
        PrivateKeyDer::Pkcs8(k) => k.secret_pkcs8_der(),
        other => return Err(format!("不支持的私钥格式: {other:?}")),
    };

    // 尝试按 P-256 解析（本项目使用的算法）
    let rng = ring::rand::SystemRandom::new();
    let kp = ring::signature::EcdsaKeyPair::from_pkcs8(
        &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
        pkcs8,
        &rng,
    )
    .map_err(|e| format!("私钥不是 P-256 PKCS#8: {e}"))?;

    let derived = kp.public_key().as_ref();
    // 证书里是完整 SPKI，需要剥掉 AlgorithmIdentifier
    let cert_pub = cert
        .tbs_certificate
        .subject_pki
        .subject_public_key
        .data
        .to_vec();
    Ok(derived == cert_pub.as_slice())
}

fn read_required(p: &Path) -> Result<String> {
    std::fs::read_to_string(p).map_err(|e| {
        Error::Storage(format!(
            "读取 {} 失败: {e}（是否已完成注册？）",
            p.display()
        ))
    })
}

/// 把 slot 名转成安全目录名。**保存与加载必须用同一套规则**（enroll 落盘、
/// `locate`/`dir_for` 加载），否则含 `/`、空格的 slot 会出现保存/加载路径分叉，
/// 甚至把 key.pem 写到数据目录之外。
pub fn sanitize(slot: &str) -> String {
    slot.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Credentials {
        Credentials {
            device_slot: "slot-1".into(),
            serial: "abcd".into(),
            cert_pem: "-----BEGIN CERTIFICATE-----\nX\n-----END CERTIFICATE-----\n".into(),
            key_pem: "-----BEGIN PRIVATE KEY-----\nY\n-----END PRIVATE KEY-----\n".into(),
            ca_chain_pem: "-----BEGIN CERTIFICATE-----\nZ\n-----END CERTIFICATE-----\n".into(),
            endpoint: "node:4440".into(),
            mtls_endpoint: "node:4441".into(),
        }
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let creds = sample();
        creds.save(dir.path()).unwrap();
        assert!(Credentials::exists(dir.path()));
        let loaded = Credentials::load(dir.path()).unwrap();
        assert_eq!(loaded.device_slot, "slot-1");
        assert_eq!(loaded.serial, "abcd");
        assert_eq!(loaded.cert_pem, creds.cert_pem);
        assert_eq!(loaded.endpoint, "node:4440");
    }

    #[cfg(unix)]
    #[test]
    fn files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        sample().save(dir.path()).unwrap();
        for name in ["cert.pem", "key.pem", "ca-chain.pem", "meta.json"] {
            let mode = std::fs::metadata(dir.path().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{name} 应为 0600，实际 {mode:o}");
        }
        let dir_mode = std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
    }

    #[test]
    fn load_missing_errors_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let err = Credentials::load(dir.path()).unwrap_err();
        assert!(matches!(err, Error::Storage(_)));
        assert!(!Credentials::exists(dir.path()));
    }

    /// 生成一对真正匹配的证书+私钥 PEM（用自签 CA 签，仅测试用）
    fn real_creds() -> Credentials {
        use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
        let ca_key = KeyPair::generate().unwrap();
        let mut cp = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "Test CA");
        cp.distinguished_name = dn;
        cp.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca_cert = cp.self_signed(&ca_key).unwrap();

        let key = KeyPair::generate().unwrap();
        let mut lp = CertificateParams::default();
        let mut ldn = DistinguishedName::new();
        ldn.push(DnType::CommonName, "slot-1");
        lp.distinguished_name = ldn;
        let now = time::OffsetDateTime::now_utc();
        lp.not_before = now;
        lp.not_after = now + time::Duration::days(36500);
        let leaf = lp.signed_by(&key, &ca_cert, &ca_key).unwrap();

        Credentials {
            device_slot: "slot-1".into(),
            serial: "aa".into(),
            cert_pem: leaf.pem(),
            key_pem: key.serialize_pem(),
            ca_chain_pem: ca_cert.pem(),
            endpoint: "node:4440".into(),
            mtls_endpoint: "node:4441".into(),
        }
    }

    #[test]
    fn verify_accepts_matching_cert_and_key() {
        let creds = real_creds();
        assert!(creds.verify().is_ok(), "{:?}", creds.verify());
    }

    #[test]
    fn verify_detects_mismatched_key() {
        let mut creds = real_creds();
        let other = real_creds();
        creds.key_pem = other.key_pem; // 换成另一份私钥
        match creds.verify() {
            Err(CredIssue::Corrupt(msg)) => assert!(msg.contains("不匹配"), "{msg}"),
            other => panic!("期望 Corrupt，实际 {other:?}"),
        }
    }

    #[test]
    fn verify_detects_expired_cert() {
        use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
        let ca_key = KeyPair::generate().unwrap();
        let mut cp = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "Test CA");
        cp.distinguished_name = dn;
        cp.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca_cert = cp.self_signed(&ca_key).unwrap();

        let key = KeyPair::generate().unwrap();
        let mut lp = CertificateParams::default();
        let mut ldn = DistinguishedName::new();
        ldn.push(DnType::CommonName, "old");
        lp.distinguished_name = ldn;
        let past = time::OffsetDateTime::now_utc() - time::Duration::days(400);
        lp.not_before = past - time::Duration::days(10);
        lp.not_after = past;
        let leaf = lp.signed_by(&key, &ca_cert, &ca_key).unwrap();

        let creds = Credentials {
            device_slot: "s".into(),
            serial: "1".into(),
            cert_pem: leaf.pem(),
            key_pem: key.serialize_pem(),
            ca_chain_pem: ca_cert.pem(),
            endpoint: "n:4440".into(),
            mtls_endpoint: "n:4441".into(),
        };
        assert!(matches!(creds.verify(), Err(CredIssue::Expired { .. })));
    }

    #[test]
    fn verify_detects_corrupt_pem() {
        let mut creds = sample();
        creds.cert_pem = "not a pem".into();
        assert!(matches!(creds.verify(), Err(CredIssue::Corrupt(_))));
    }

    #[test]
    fn issue_advice_is_actionable() {
        let a = CredIssue::NotEnrolled.advice();
        assert!(a.contains("enroll"), "建议里应给出 enroll 命令: {a}");
        let b = CredIssue::Corrupt("x".into()).advice();
        assert!(b.contains("rm -rf"), "建议里应说明清理目录: {b}");
        let c = CredIssue::MissingFiles(vec!["key.pem".into()]).advice();
        assert!(c.contains("key.pem"));
    }

    #[test]
    fn locate_prefers_explicit_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let p = locate("slot-1", Some(tmp.path())).unwrap();
        assert_eq!(p, tmp.path().join("creds").join("slot-1"));
    }

    #[test]
    fn slot_sanitized_for_path() {
        assert_eq!(sanitize("a/b c"), "a_b_c");
        assert_eq!(sanitize("slot-1"), "slot-1");
    }
}
