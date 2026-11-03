//! 服务端身份验证（契约第 4 节与讨论中的三优先级）。
//!
//! ```text
//! 优先级1: 链接内嵌 SPKI/证书指纹 pin（sha256/...）—— 只接受该指纹
//! 优先级2: 本地 trust store 里已确认过的指纹（TOFU 记录）
//! 优先级3: TOFU —— 展示指纹，人工确认后写入 trust store
//! ```
//!
//! 无论走哪条路径，最后都会把「客户端实际看到的证书指纹」返回给调用方，
//! 随注册请求上报（`tls_cert_seen_fp`），服务端据此做中间人检测。

use crate::error::{Error, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// 解析出的健康指纹（hex，小写）。
pub fn cert_fp_hex(der: &[u8]) -> String {
    hex::encode(Sha256::digest(der))
}

/// SPKI 指纹（对 SubjectPublicKeyInfo 求 sha256）。
///
/// 相比整证书指纹，SPKI 指纹在证书续期后仍然稳定，是 HPKP 式的常规做法。
pub fn spki_fp_hex(der: &[u8]) -> Result<String> {
    use x509_parser::prelude::*;
    let (_, cert) =
        X509Certificate::from_der(der).map_err(|e| Error::Crypto(format!("parse cert: {e}")))?;
    Ok(hex::encode(Sha256::digest(
        cert.tbs_certificate.subject_pki.raw,
    )))
}

/// pin 来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pin {
    /// `sha256/<hex>` —— 对整证书 DER 求 sha256
    CertSha256(String),
    /// `sha256spki/<hex>` —— 对 SPKI 求 sha256
    SpkiSha256(String),
}

impl Pin {
    /// 从注册链接的查询参数里解析 pin。
    pub fn parse(raw: &str) -> Result<Self> {
        if let Some(v) = raw.strip_prefix("sha256/") {
            return Ok(Pin::CertSha256(normalize_hex(v)?));
        }
        if let Some(v) = raw.strip_prefix("sha256spki/") {
            return Ok(Pin::SpkiSha256(normalize_hex(v)?));
        }
        // 裸 hex 视为证书指纹
        Ok(Pin::CertSha256(normalize_hex(raw)?))
    }

    /// 校验给定证书是否匹配该 pin。
    pub fn matches(&self, cert_der: &[u8]) -> Result<bool> {
        match self {
            Pin::CertSha256(expected) => Ok(&cert_fp_hex(cert_der) == expected),
            Pin::SpkiSha256(expected) => Ok(&spki_fp_hex(cert_der)? == expected),
        }
    }
}

fn normalize_hex(s: &str) -> Result<String> {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != ':')
        .collect();
    if cleaned.len() != 64 || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::InvalidLink(format!(
            "pin 必须是 64 位 hex，实际 {} 位",
            cleaned.len()
        )));
    }
    Ok(cleaned.to_ascii_lowercase())
}

/// TOFU 信任库：记录已人工确认过的服务端指纹。
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct TrustStore {
    /// endpoint → 已确认的证书指纹（hex）
    entries: std::collections::BTreeMap<String, String>,
}

impl TrustStore {
    pub fn path() -> Result<PathBuf> {
        Ok(data_dir()?.join("trust-store.json"))
    }

    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(serde_json::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::Storage(e.to_string())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            set_dir_private(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        write_private(path, json.as_bytes())
    }

    pub fn get(&self, endpoint: &str) -> Option<&str> {
        self.entries.get(endpoint).map(|s| s.as_str())
    }

    pub fn remember(&mut self, endpoint: &str, fp_hex: &str) {
        self.entries.insert(endpoint.to_owned(), fp_hex.to_owned());
    }

    /// 校验：pin 优先，其次已记录的 TOFU 指纹。
    ///
    /// 返回 `TrustOutcome`，其中 `needs_confirmation` 表示必须走人工确认。
    pub fn check(
        &self,
        endpoint: &str,
        pin: Option<&Pin>,
        cert_der: &[u8],
    ) -> Result<TrustOutcome> {
        let seen = cert_fp_hex(cert_der);

        if let Some(pin) = pin {
            if pin.matches(cert_der)? {
                return Ok(TrustOutcome {
                    seen_fp: seen,
                    needs_confirmation: false,
                    reason: "pin 匹配".into(),
                });
            }
            return Err(Error::FingerprintMismatch);
        }

        if let Some(known) = self.get(endpoint) {
            if known.eq_ignore_ascii_case(&seen) {
                return Ok(TrustOutcome {
                    seen_fp: seen,
                    needs_confirmation: false,
                    reason: "已信任（TOFU 记录）".into(),
                });
            }
            return Err(Error::FingerprintMismatch);
        }

        Ok(TrustOutcome {
            seen_fp: seen,
            needs_confirmation: true,
            reason: "首次连接（TOFU），需要人工确认".into(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct TrustOutcome {
    pub seen_fp: String,
    pub needs_confirmation: bool,
    pub reason: String,
}

// ---------------- 私有写盘 ----------------

fn data_dir() -> Result<PathBuf> {
    // 手写平台目录逻辑，避免引入 dirs/directories 带来的多余传递依赖
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME")
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg).join("qe-device-pki-client"));
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return Ok(PathBuf::from(home)
            .join(".local/share")
            .join("qe-device-pki-client"));
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        return Ok(PathBuf::from(appdata).join("qe-device-pki-client"));
    }
    Err(Error::Storage(
        "无法确定数据目录（HOME / XDG_DATA_HOME / APPDATA 均不可用）".into(),
    ))
}

pub fn default_data_dir() -> Result<PathBuf> {
    data_dir()
}

#[cfg(unix)]
fn set_dir_private(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_dir_private(_p: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600) // 新建时就是 0600，避免"先 0644 再 chmod"的窗口
        .open(path)?;
    // `OpenOptions::mode()` 只在**创建**时生效：文件已存在且是 0644 时，
    // 覆盖写完仍是 0644。所以每次写都显式改一次权限。
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    f.write_all(bytes)?;
    Ok(())
}

#[cfg(not(unix))]
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_parses_cert_and_spki_forms() {
        let hex64 = "ab".repeat(32);
        assert_eq!(
            Pin::parse(&format!("sha256/{hex64}")).unwrap(),
            Pin::CertSha256(hex64.clone())
        );
        assert_eq!(
            Pin::parse(&format!("sha256spki/{hex64}")).unwrap(),
            Pin::SpkiSha256(hex64.clone())
        );
        assert!(Pin::parse("short").is_err());
        assert!(Pin::parse("sha256/zz").is_err());
    }

    #[test]
    fn pin_normalizes_case_and_separators() {
        let upper = "AB".repeat(32);
        let with_colons = upper
            .as_bytes()
            .chunks(2)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join(":");
        let parsed = Pin::parse(&format!("sha256/{with_colons}")).unwrap();
        assert_eq!(parsed, Pin::CertSha256("ab".repeat(32)));
    }

    #[test]
    fn tofu_records_and_matches() {
        let mut store = TrustStore::default();
        let der = b"fake-cert-der";
        let fp = cert_fp_hex(der);

        let first = store.check("node:4440", None, der).unwrap();
        assert!(first.needs_confirmation);
        assert_eq!(first.seen_fp, fp);

        store.remember("node:4440", &fp);
        let second = store.check("node:4440", None, der).unwrap();
        assert!(!second.needs_confirmation);
    }

    #[test]
    fn tofu_rejects_changed_cert() {
        let mut store = TrustStore::default();
        store.remember("node:4440", &cert_fp_hex(b"old"));
        let err = store.check("node:4440", None, b"new").unwrap_err();
        assert!(matches!(err, Error::FingerprintMismatch));
    }

    #[test]
    fn pin_mismatch_is_hard_failure() {
        let store = TrustStore::default();
        let pin = Pin::CertSha256("00".repeat(32));
        let err = store.check("node:4440", Some(&pin), b"cert").unwrap_err();
        assert!(matches!(err, Error::FingerprintMismatch));
    }

    #[test]
    fn pin_match_needs_no_confirmation() {
        let der = b"cert-bytes";
        let pin = Pin::CertSha256(cert_fp_hex(der));
        let store = TrustStore::default();
        let out = store.check("node:4440", Some(&pin), der).unwrap();
        assert!(!out.needs_confirmation);
    }

    #[test]
    fn trust_store_roundtrips_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let mut store = TrustStore::load(&path).unwrap();
        store.remember("a:1", &"11".repeat(32));
        store.save(&path).unwrap();
        let loaded = TrustStore::load(&path).unwrap();
        assert_eq!(loaded.get("a:1"), Some("11".repeat(32).as_str()));
    }

    #[test]
    fn missing_trust_store_is_empty_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = TrustStore::load(&dir.path().join("nope.json")).unwrap();
        assert!(store.get("anything").is_none());
    }
}
