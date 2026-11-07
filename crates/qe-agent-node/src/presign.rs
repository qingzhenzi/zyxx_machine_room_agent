//! 预签名请求（契约 §16.2）：agent 在 mTLS 通道内换取短 TTL 的对象 URL。
//!
//! 服务端不代理字节，只回 `urls` 与 Ed25519 公钥。客户端只依赖
//! [`PresignClient`] trait，测试可用 mock，生产用 [`MtlsPresignClient`]。

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use qe_device_pki_client::transport::{TransportConfig, build_mtls_client};
use serde::{Deserialize, Serialize};

use crate::BoxFuture;

/// 默认预签名路径（下行 payload 里的 `presign_endpoint` 也默认是它）。
pub const DEFAULT_PRESIGN_PATH: &str = "/resources/presign";
/// 单次最多换取的分片 URL 数（与服务端 clamp 1..=64 一致）。
pub const MAX_CHUNKS_PER_PRESIGN: i32 = 64;

/// 预签名对象的类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PresignKind {
    Manifest,
    Signature,
    Chunk,
}

/// `POST /resources/presign` 请求体（字段与 §16.2 一致）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresignRequest {
    pub manifest_digest: String,
    pub kind: PresignKind,
    pub index: i32,
    pub count: i32,
}

impl PresignRequest {
    /// 取 manifest 对象（`manifests/{digest}.json`）。
    pub fn manifest(manifest_digest: impl Into<String>) -> Self {
        Self {
            manifest_digest: manifest_digest.into(),
            kind: PresignKind::Manifest,
            index: 0,
            count: 1,
        }
    }

    /// 取 manifest 签名对象（`signatures/{digest}.sig`）。
    pub fn signature(manifest_digest: impl Into<String>) -> Self {
        Self {
            manifest_digest: manifest_digest.into(),
            kind: PresignKind::Signature,
            index: 0,
            count: 1,
        }
    }

    /// 从 `index` 起取最多 [`MAX_CHUNKS_PER_PRESIGN`] 个分片。
    pub fn chunk_batch(manifest_digest: impl Into<String>, index: i32, count: i32) -> Self {
        Self {
            manifest_digest: manifest_digest.into(),
            kind: PresignKind::Chunk,
            index: index.max(0),
            count: count.clamp(1, MAX_CHUNKS_PER_PRESIGN),
        }
    }
}

/// 单个预签名 URL。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlItem {
    /// 对象键，如 `objects/<sha256>`。
    pub key: String,
    pub url: String,
    pub expires_at: DateTime<Utc>,
}

/// 预签名响应（字段与 §16.2 一致）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresignReply {
    pub urls: Vec<UrlItem>,
    /// Ed25519 公钥（32 字节标准 base64）；用于校验 manifest 签名。
    pub public_key_base64: String,
}

/// 预签名客户端抽象。实现必须可跨任务共享。
pub trait PresignClient: Send + Sync {
    fn presign<'a>(&'a self, request: &'a PresignRequest) -> BoxFuture<'a, Result<PresignReply>>;
}

/// 仅做 URL 拼接的部分，独立出来便于无凭证单测。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignEndpoint {
    pub endpoint: String,
    pub path: String,
}

impl PresignEndpoint {
    pub fn new(endpoint: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            path: path.into(),
        }
    }

    /// `{endpoint}{path}`，两端斜杠去重。
    pub fn url(&self) -> String {
        join_endpoint(&self.endpoint, &self.path)
    }
}

/// 拼接 mTLS 端点与路径，容忍任意一侧末尾/开头的斜杠。
pub fn join_endpoint(endpoint: &str, path: &str) -> String {
    let endpoint = endpoint.trim_end_matches('/');
    if path.is_empty() {
        return endpoint.to_owned();
    }
    if path.starts_with('/') {
        format!("{endpoint}{path}")
    } else {
        format!("{endpoint}/{path}")
    }
}

/// 通过 device-pki mTLS 端点换预签名 URL 的客户端。
pub struct MtlsPresignClient {
    client: reqwest::Client,
    endpoint: PresignEndpoint,
}

impl MtlsPresignClient {
    /// 用设备证书构建 mTLS 客户端，使用默认预签名路径。
    pub fn new(transport: &TransportConfig) -> Result<Self> {
        Self::with_path(transport, DEFAULT_PRESIGN_PATH)
    }

    /// 用配置文件或下行 payload 提供的 `presign_endpoint` 构建。
    pub fn with_path(transport: &TransportConfig, path: impl Into<String>) -> Result<Self> {
        let client = build_mtls_client(transport)
            .map_err(|error| anyhow::anyhow!("构建 mTLS 客户端失败: {error}"))?;
        Ok(Self {
            client,
            endpoint: PresignEndpoint::new(transport.mtls_endpoint.clone(), path),
        })
    }

    /// 实际请求的 URL（测试/日志用）。
    pub fn url(&self) -> String {
        self.endpoint.url()
    }
}

impl PresignClient for MtlsPresignClient {
    fn presign<'a>(&'a self, request: &'a PresignRequest) -> BoxFuture<'a, Result<PresignReply>> {
        Box::pin(async move {
            let url = self.url();
            let response = self
                .client
                .post(&url)
                .json(request)
                .send()
                .await
                .with_context(|| format!("预签名请求失败: POST {url}"))?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                bail!(
                    "预签名请求被拒绝: POST {url} 返回 {status}（{}）",
                    body.chars().take(256).collect::<String>()
                );
            }
            response
                .json::<PresignReply>()
                .await
                .with_context(|| format!("预签名响应解析失败: POST {url}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_json_matches_contract() {
        let request = PresignRequest {
            manifest_digest: "a".repeat(64),
            kind: PresignKind::Chunk,
            index: 3,
            count: 2,
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "manifest_digest": "a".repeat(64),
                "kind": "chunk",
                "index": 3,
                "count": 2,
            })
        );

        assert_eq!(
            serde_json::to_value(PresignKind::Manifest).unwrap(),
            "manifest"
        );
        assert_eq!(
            serde_json::to_value(PresignKind::Signature).unwrap(),
            "signature"
        );
    }

    #[test]
    fn constructors_set_expected_fields() {
        let manifest = PresignRequest::manifest("d");
        assert_eq!(manifest.kind, PresignKind::Manifest);
        assert_eq!((manifest.index, manifest.count), (0, 1));

        let signature = PresignRequest::signature("d");
        assert_eq!(signature.kind, PresignKind::Signature);

        let chunks = PresignRequest::chunk_batch("d", 5, 1000);
        assert_eq!(chunks.kind, PresignKind::Chunk);
        assert_eq!(chunks.index, 5);
        assert_eq!(chunks.count, MAX_CHUNKS_PER_PRESIGN, "单次最多 64 个");

        let chunks = PresignRequest::chunk_batch("d", -1, 0);
        assert_eq!(chunks.index, 0);
        assert_eq!(chunks.count, 1, "count 至少为 1");
    }

    #[test]
    fn reply_deserializes_with_rfc3339_expiry() {
        let reply: PresignReply = serde_json::from_value(serde_json::json!({
            "urls": [
                {"key": "objects/abc", "url": "https://s3.example/objects/abc?X-Amz-Signature=x",
                 "expires_at": "2024-01-02T03:04:05Z"},
                {"key": "manifests/def", "url": "https://s3.example/manifests/def",
                 "expires_at": "2024-01-02T04:04:05+01:00"}
            ],
            "public_key_base64": "cHVibGlj"
        }))
        .unwrap();
        assert_eq!(reply.urls.len(), 2);
        assert_eq!(reply.urls[0].key, "objects/abc");
        assert_eq!(
            reply.urls[1].expires_at.to_rfc3339(),
            "2024-01-02T03:04:05+00:00"
        );
        assert_eq!(reply.public_key_base64, "cHVibGlj");
    }

    #[test]
    fn reply_missing_public_key_is_rejected() {
        let error = serde_json::from_value::<PresignReply>(serde_json::json!({"urls": []}))
            .expect_err("缺少 public_key_base64 必须反序列化失败");
        assert!(error.to_string().contains("public_key_base64"), "{error}");
    }

    #[test]
    fn endpoint_joining_handles_slashes() {
        assert_eq!(
            join_endpoint("https://node:4441", "/resources/presign"),
            "https://node:4441/resources/presign"
        );
        assert_eq!(
            join_endpoint("https://node:4441/", "/resources/presign"),
            "https://node:4441/resources/presign"
        );
        assert_eq!(
            join_endpoint("https://node:4441", "resources/presign"),
            "https://node:4441/resources/presign"
        );
        assert_eq!(
            join_endpoint("https://node:4441///", "///resources/presign"),
            "https://node:4441///resources/presign",
            "已有斜杠不重复添加，且不吞掉 path 自带的多个斜杠"
        );
        assert_eq!(join_endpoint("https://node:4441/", ""), "https://node:4441");
    }

    #[test]
    fn endpoint_struct_uses_default_path() {
        let endpoint = PresignEndpoint::new("https://node:4441/", DEFAULT_PRESIGN_PATH);
        assert_eq!(endpoint.url(), "https://node:4441/resources/presign");
    }
}
