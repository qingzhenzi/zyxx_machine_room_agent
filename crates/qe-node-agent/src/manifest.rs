//! 镜像清单（与 `docs/storage-design.md` §14.2 字段严格对齐）、内容摘要与 Ed25519 验签。
//!
//! 服务端（`qe-domain`) 用 `serde_json::to_vec(manifest)` 的字节取 sha256 作为
//! 镜像 digest，用同一份字节做 Ed25519 签名。agent 必须：
//!
//! 1. 校验下载到的 manifest **原始字节**的 sha256 == 请求的 digest；
//! 2. 用 presign 响应里的公钥对**原始字节**验签；
//! 3. 检查反序列化后再序列化的字节是否仍等于 digest（规范化一致性），避免字段
//!    顺序/类型漂移导致签名对象与本地结构语义不一致。
//!
//! 本模块的结构体字段顺序与 §14.2 完全一致——这是 digest 稳定的前提，改动字段
//! 顺序会直接改变 `digest_hex()` 的结果。

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// manifest schema 版本（P1 只接受 1）。
pub const MANIFEST_SCHEMA: i32 = 1;

/// 镜像类型，对应 `image_artifacts.kind`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageKind {
    Docker,
    Oci,
    LxcRootfs,
    VmDisk,
}

impl ImageKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Oci => "oci",
            Self::LxcRootfs => "lxc_rootfs",
            Self::VmDisk => "vm_disk",
        }
    }
}

impl std::fmt::Display for ImageKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 镜像产物格式，对应 `image_artifacts.format`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
    DockerArchive,
    OciLayout,
    Tar,
    Qcow2,
    Raw,
}

impl ImageFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DockerArchive => "docker_archive",
            Self::OciLayout => "oci_layout",
            Self::Tar => "tar",
            Self::Qcow2 => "qcow2",
            Self::Raw => "raw",
        }
    }

    /// 组装后落盘的扩展名。
    pub const fn file_extension(self) -> &'static str {
        match self {
            Self::DockerArchive | Self::OciLayout | Self::Tar => "tar",
            Self::Qcow2 => "qcow2",
            Self::Raw => "raw",
        }
    }
}

impl std::fmt::Display for ImageFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 压缩算法；P1 只接受 `none`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    None,
    Zstd,
    Gzip,
}

impl Compression {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Zstd => "zstd",
            Self::Gzip => "gzip",
        }
    }
}

/// manifest 中的一个内容寻址分片。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestChunk {
    pub index: i32,
    pub object_key: String,
    pub sha256: String,
    pub size: i64,
    pub offset: i64,
}

/// manifest 中与运行时相关的可选信息（Docker / LXC / VM）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestRuntime {
    pub docker_image_ref: Option<String>,
    pub lxc_distro: Option<String>,
    pub lxc_release: Option<String>,
    pub vm_os: Option<String>,
    pub vm_disk_size: Option<i64>,
}

/// 镜像清单。字段顺序即 JSON 字段顺序（digest 稳定性依赖于此）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: i32,
    pub kind: ImageKind,
    pub name: String,
    pub version: String,
    pub platform: String,
    pub format: ImageFormat,
    pub compression: Compression,
    pub total_size: i64,
    pub chunks: Vec<ManifestChunk>,
    pub runtime: ManifestRuntime,
    pub annotations: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub created_by: Option<Uuid>,
}

impl Manifest {
    /// 序列化为 manifest JSON（字段顺序固定，供签名与内容寻址使用）。
    pub fn to_json(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).context("manifest 序列化失败")
    }

    /// `sha256(manifest_json)` 的小写 hex，即镜像 digest。
    pub fn digest_hex(&self) -> Result<String> {
        Ok(sha256_hex(&self.to_json()?))
    }

    /// 用公钥校验清单自身（规范化序列化字节）的签名。
    pub fn verify_signature(&self, public_key_base64: &str, signature: &[u8]) -> Result<()> {
        let message = self.to_json()?;
        verify_signature(public_key_base64, &message, signature)
    }

    /// 用公钥校验**下载到的原始字节**的签名（签名对象就是这份字节）。
    pub fn verify_signature_bytes(
        &self,
        public_key_base64: &str,
        manifest_json: &[u8],
        signature: &[u8],
    ) -> Result<()> {
        verify_signature(public_key_base64, manifest_json, signature)
    }

    /// 结构校验（§14.2 的 `validate_manifest` 中 agent 侧必须做的部分）。
    ///
    /// 任何一项不满足都拒绝，避免把格式不对的产物交给 importer。
    pub fn validate(&self) -> Result<()> {
        if self.schema != MANIFEST_SCHEMA {
            bail!(
                "manifest schema 必须为 {MANIFEST_SCHEMA}，实际 {}",
                self.schema
            );
        }
        if self.name.trim().is_empty() {
            bail!("manifest name 不能为空");
        }
        if self.version.trim().is_empty() {
            bail!("manifest version 不能为空");
        }
        if self.platform.trim().is_empty() {
            bail!("manifest platform 不能为空");
        }
        if self.compression != Compression::None {
            bail!(
                "P1 只接受 compression=none，实际 {}",
                self.compression.as_str()
            );
        }
        let pair_ok = matches!(
            (self.kind, self.format),
            (ImageKind::Docker, ImageFormat::DockerArchive)
                | (ImageKind::Oci, ImageFormat::OciLayout)
                | (ImageKind::LxcRootfs, ImageFormat::Tar)
                | (ImageKind::VmDisk, ImageFormat::Qcow2)
                | (ImageKind::VmDisk, ImageFormat::Raw)
        );
        if !pair_ok {
            bail!(
                "kind/format 组合非法: {}/{}",
                self.kind.as_str(),
                self.format.as_str()
            );
        }
        if self.chunks.is_empty() {
            bail!("manifest 至少需要一个分片");
        }
        if self.total_size <= 0 {
            bail!("manifest total_size 必须 > 0，实际 {}", self.total_size);
        }

        let mut expected_offset: i64 = 0;
        let mut seen = std::collections::HashSet::new();
        for (position, chunk) in self.chunks.iter().enumerate() {
            let expected_index = i32::try_from(position).context("分片数量溢出 i32")?;
            if chunk.index != expected_index {
                bail!(
                    "分片 index 必须从 0 连续：位置 {position} 期望 {expected_index}，实际 {}",
                    chunk.index
                );
            }
            if chunk.offset != expected_offset {
                bail!(
                    "分片 offset 必须从 0 连续：分片 {} 期望 {expected_offset}，实际 {}",
                    chunk.index,
                    chunk.offset
                );
            }
            if chunk.size <= 0 {
                bail!("分片 {} 的 size 必须 > 0，实际 {}", chunk.index, chunk.size);
            }
            if chunk.object_key.trim().is_empty() {
                bail!("分片 {} 的 object_key 不能为空", chunk.index);
            }
            if !is_sha256_hex(&chunk.sha256) {
                bail!("分片 {} 的 sha256 不是 64 位小写 hex", chunk.index);
            }
            if !seen.insert(chunk.sha256.as_str()) {
                bail!("分片 {} 的 sha256 与前面的分片重复", chunk.index);
            }
            expected_offset = expected_offset
                .checked_add(chunk.size)
                .context("分片 offset 溢出 i64")?;
        }
        if expected_offset != self.total_size {
            bail!(
                "manifest total_size 与分片大小之和不一致：{expected_offset} != {}",
                self.total_size
            );
        }
        Ok(())
    }

    /// 逐分片累计的结束偏移（用于组装时校验）。
    pub fn chunk_end(&self, index: usize) -> Option<i64> {
        let chunk = self.chunks.get(index)?;
        chunk.offset.checked_add(chunk.size)
    }
}

/// 计算字节内容的 sha256 小写 hex。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}

/// 是否为合法的 sha256 hex（64 位、小写）。
pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Ed25519 严格验签。
///
/// - 公钥必须是标准 base64 编码的 32 字节；
/// - 签名必须是 64 字节；
/// - 使用 `verify_strict`：拒绝弱公钥（small-order）、非规范 R/S 编码。
pub fn verify_signature(public_key_base64: &str, message: &[u8], signature: &[u8]) -> Result<()> {
    let key_bytes = decode_public_key(public_key_base64)?;
    let verifying_key = VerifyingKey::from_bytes(&key_bytes)
        .map_err(|error| anyhow::anyhow!("manifest 公钥不合法: {error}"))?;
    let signature = Signature::from_slice(signature)
        .map_err(|error| anyhow::anyhow!("manifest 签名长度/格式不合法: {error}"))?;
    verifying_key
        .verify_strict(message, &signature)
        .map_err(|error| anyhow::anyhow!("manifest 签名校验失败: {error}"))
}

/// 解析 base64 公钥（标准字母表，兼容无 padding 的写法）。
pub fn decode_public_key(public_key_base64: &str) -> Result<[u8; 32]> {
    let trimmed = public_key_base64.trim();
    if trimmed.is_empty() {
        bail!("presign 响应缺少 public_key_base64");
    }
    // 服务端用标准字母表 + padding 编码；这里额外容忍 URL-safe 与省略 padding
    // 的写法（与 qe-storage 的解码器一致，便于手工配置的密钥）。
    let decoded = STANDARD
        .decode(trimmed)
        .or_else(|_| STANDARD_NO_PAD.decode(trimmed))
        .or_else(|_| URL_SAFE.decode(trimmed))
        .or_else(|_| URL_SAFE_NO_PAD.decode(trimmed))
        .context("public_key_base64 不是合法 base64")?;
    let bytes: [u8; 32] = decoded
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("Ed25519 公钥必须是 32 字节，实际 {}", decoded.len()))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};

    fn chunk(index: i32, data: &[u8], offset: i64) -> ManifestChunk {
        ManifestChunk {
            index,
            object_key: format!("objects/{}", sha256_hex(data)),
            sha256: sha256_hex(data),
            size: data.len() as i64,
            offset,
        }
    }

    fn manifest() -> Manifest {
        let first = b"hello ";
        let second = b"world";
        Manifest {
            schema: 1,
            kind: ImageKind::Docker,
            name: "debian".into(),
            version: "12".into(),
            platform: "linux/amd64".into(),
            format: ImageFormat::DockerArchive,
            compression: Compression::None,
            total_size: (first.len() + second.len()) as i64,
            chunks: vec![chunk(0, first, 0), chunk(1, second, first.len() as i64)],
            runtime: ManifestRuntime {
                docker_image_ref: Some("debian:12".into()),
                ..ManifestRuntime::default()
            },
            annotations: serde_json::json!({}),
            created_at: "2024-01-02T03:04:05Z".parse().unwrap(),
            created_by: None,
        }
    }

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn public_key_base64(key: &SigningKey) -> String {
        STANDARD.encode(key.verifying_key().as_bytes())
    }

    /// §14.2 的 JSON 字段顺序：任何字段顺序变化都会让服务端签名失效。
    #[test]
    fn json_matches_contract_field_order() {
        let json = manifest().to_json().unwrap();
        let text = String::from_utf8(json.clone()).unwrap();
        let expected = format!(
            concat!(
                r#"{{"schema":1,"kind":"docker","name":"debian","version":"12","#,
                r#""platform":"linux/amd64","format":"docker_archive","compression":"none","#,
                r#""total_size":11,"chunks":["#,
                r#"{{"index":0,"object_key":"objects/{first_sha}","sha256":"{first_sha}","size":6,"offset":0}},"#,
                r#"{{"index":1,"object_key":"objects/{second_sha}","sha256":"{second_sha}","size":5,"offset":6}}"#,
                r#"],"runtime":{{"docker_image_ref":"debian:12","lxc_distro":null,"lxc_release":null,"#,
                r#""vm_os":null,"vm_disk_size":null}},"annotations":{{}},"#,
                r#""created_at":"2024-01-02T03:04:05Z","created_by":null}}"#
            ),
            first_sha = sha256_hex(b"hello "),
            second_sha = sha256_hex(b"world"),
        );
        assert_eq!(text, expected);
        assert_eq!(
            manifest().digest_hex().unwrap(),
            sha256_hex(expected.as_bytes())
        );
    }

    #[test]
    fn digest_is_stable_across_roundtrip() {
        let original = manifest();
        let first = original.digest_hex().unwrap();
        let decoded: Manifest = serde_json::from_slice(&original.to_json().unwrap()).unwrap();
        assert_eq!(decoded.digest_hex().unwrap(), first);
        // 重复计算必须稳定
        assert_eq!(original.digest_hex().unwrap(), first);
        assert_eq!(first.len(), 64);
        assert!(is_sha256_hex(&first));
    }

    #[test]
    fn signature_verifies_with_matching_key() {
        let manifest = manifest();
        let key = signing_key();
        let json = manifest.to_json().unwrap();
        let signature = key.sign(&json).to_bytes().to_vec();
        let public_key = public_key_base64(&key);

        manifest.verify_signature(&public_key, &signature).unwrap();
        manifest
            .verify_signature_bytes(&public_key, &json, &signature)
            .unwrap();
    }

    #[test]
    fn tampered_manifest_fails_signature() {
        let manifest = manifest();
        let key = signing_key();
        let signature = key.sign(&manifest.to_json().unwrap()).to_bytes().to_vec();

        let mut tampered = manifest.clone();
        tampered.name = "ubuntu".into();
        let err = tampered
            .verify_signature(&public_key_base64(&key), &signature)
            .expect_err("篡改后的 manifest 必须验签失败");
        assert!(err.to_string().contains("签名"), "{err}");
    }

    #[test]
    fn tampered_signature_fails() {
        let manifest = manifest();
        let key = signing_key();
        let mut signature = key.sign(&manifest.to_json().unwrap()).to_bytes().to_vec();
        signature[0] ^= 0xff;
        assert!(
            manifest
                .verify_signature(&public_key_base64(&key), &signature)
                .is_err()
        );
    }

    #[test]
    fn wrong_key_fails() {
        let manifest = manifest();
        let signer = signing_key();
        let other = SigningKey::from_bytes(&[9u8; 32]);
        let signature = signer
            .sign(&manifest.to_json().unwrap())
            .to_bytes()
            .to_vec();
        let err = manifest
            .verify_signature(&public_key_base64(&other), &signature)
            .expect_err("换钥匙必须验签失败");
        assert!(err.to_string().contains("签名"), "{err}");
    }

    #[test]
    fn malformed_keys_and_signatures_are_rejected() {
        let manifest = manifest();
        let json = manifest.to_json().unwrap();
        let key = signing_key();
        let signature = key.sign(&json).to_bytes().to_vec();

        assert!(verify_signature("not-base64!!", &json, &signature).is_err());
        assert!(verify_signature(&STANDARD.encode([0u8; 31]), &json, &signature).is_err());
        assert!(verify_signature(&public_key_base64(&key), &json, &signature[..63]).is_err());
        assert!(verify_signature("", &json, &signature).is_err());
    }

    #[test]
    fn accepts_public_key_without_padding_and_url_safe_alphabet() {
        let key = signing_key();
        let padded = public_key_base64(&key);
        let unpadded = padded.trim_end_matches('=');
        // 32 字节 base64 的 padding 可能不存在；两种写法都必须能解出同一把钥匙
        assert_eq!(
            decode_public_key(&padded).unwrap(),
            decode_public_key(unpadded).unwrap()
        );
        // 服务端解码器同时接受 URL-safe 字母表，客户端保持一致
        assert_eq!(
            decode_public_key(&padded.replace('+', "-").replace('/', "_")).unwrap(),
            decode_public_key(&padded).unwrap()
        );
    }

    #[test]
    fn validate_accepts_well_formed_manifest() {
        manifest().validate().unwrap();
    }

    #[test]
    fn validate_rejects_bad_schema_and_empty_chunks() {
        let mut bad = manifest();
        bad.schema = 2;
        assert!(bad.validate().unwrap_err().to_string().contains("schema"));

        let mut bad = manifest();
        bad.chunks.clear();
        assert!(bad.validate().unwrap_err().to_string().contains("分片"));
    }

    #[test]
    fn validate_rejects_non_contiguous_offsets_and_bad_total() {
        let mut bad = manifest();
        bad.chunks[1].offset = 99;
        assert!(bad.validate().unwrap_err().to_string().contains("offset"));

        let mut bad = manifest();
        bad.total_size += 1;
        assert!(
            bad.validate()
                .unwrap_err()
                .to_string()
                .contains("total_size")
        );
    }

    #[test]
    fn validate_rejects_bad_kind_format_pair() {
        let mut bad = manifest();
        bad.format = ImageFormat::Qcow2;
        assert!(
            bad.validate()
                .unwrap_err()
                .to_string()
                .contains("kind/format")
        );
    }

    #[test]
    fn validate_rejects_duplicate_and_invalid_sha() {
        let mut bad = manifest();
        bad.chunks[1].sha256 = bad.chunks[0].sha256.clone();
        assert!(bad.validate().unwrap_err().to_string().contains("重复"));

        let mut bad = manifest();
        bad.chunks[0].sha256 = "ABC".into();
        assert!(bad.validate().unwrap_err().to_string().contains("sha256"));
    }

    #[test]
    fn validate_rejects_compression_and_zeroshard() {
        let mut bad = manifest();
        bad.compression = Compression::Zstd;
        assert!(
            bad.validate()
                .unwrap_err()
                .to_string()
                .contains("compression")
        );

        let mut bad = manifest();
        bad.chunks[0].size = 0;
        assert!(bad.validate().unwrap_err().to_string().contains("size"));
    }

    #[test]
    fn chunk_end_is_checked() {
        let manifest = manifest();
        assert_eq!(manifest.chunk_end(0), Some(6));
        assert_eq!(manifest.chunk_end(1), Some(11));
        assert_eq!(manifest.chunk_end(2), None);
    }
}
