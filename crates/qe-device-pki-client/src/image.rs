//! 镜像制作上传（阶段 2）：`presign-upload` + `register`（device-pki mTLS）。
//!
//! 契约（`room_node/docs/vm-base-image-and-diff-disk.md` §8.5）：
//! - `POST /machine/images/presign-upload` body `{sha256, size_bytes}` →
//!   `{object_key, upload_url, expires_at, already_exists}`；对象已存在时 agent 跳过 PUT；
//! - `POST /machine/images/register` body
//!   `{name, version, platform, kind:"vm_disk", format:"qcow2", compression:"none",
//!     total_size, parts:[{sha256,size}], annotations?}` →
//!   `{image_id, manifest_digest, status}`（manifest 由 device-pki 构造并签名）。
//!
//! PUT 到 presigned URL 是普通 HTTPS（不走设备证书），由调用方（qe-agent-node
//! 的 `image_make`）用 reqwest 完成，超时 600s。

use crate::error::{Error, Result};
use crate::transport::{TransportConfig, build_mtls_client};
use serde::{Deserialize, Serialize};

/// 预签名上传路径。
pub const PRESIGN_UPLOAD_PATH: &str = "/machine/images/presign-upload";
/// 镜像登记路径。
pub const REGISTER_PATH: &str = "/machine/images/register";

/// `POST /machine/images/presign-upload` 请求体。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PresignUploadRequest {
    /// 对象内容的 sha256（64 位小写 hex）。
    pub sha256: String,
    /// 对象字节数。
    pub size_bytes: u64,
}

/// `POST /machine/images/presign-upload` 响应体。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PresignUploadReply {
    #[serde(default)]
    pub object_key: String,
    /// `already_exists=true` 时可能缺省。
    #[serde(default)]
    pub upload_url: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    /// 对象已存在（大小一致）→ agent 跳过 PUT。
    #[serde(default)]
    pub already_exists: bool,
}

/// manifest 的一个分片（32 MiB 切片后逐片登记）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImagePart {
    pub sha256: String,
    pub size: u64,
}

/// `POST /machine/images/register` 请求体。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegisterImageRequest {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub kind: String,
    pub format: String,
    pub compression: String,
    pub total_size: u64,
    pub parts: Vec<ImagePart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<serde_json::Value>,
}

impl RegisterImageRequest {
    /// 阶段 2 的固定形态：单文件 qcow2 → 多分片 manifest。
    pub fn new_vm_disk(
        name: impl Into<String>,
        version: impl Into<String>,
        platform: impl Into<String>,
        total_size: u64,
        parts: Vec<ImagePart>,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            platform: platform.into(),
            kind: "vm_disk".to_owned(),
            format: "qcow2".to_owned(),
            compression: "none".to_owned(),
            total_size,
            parts,
            annotations: None,
        }
    }

    /// 附加 annotations（例如 base_volume / description）。
    pub fn with_annotations(mut self, annotations: Option<serde_json::Value>) -> Self {
        self.annotations = annotations;
        self
    }
}

/// `POST /machine/images/register` 响应体。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RegisterImageReply {
    /// 镜像 id（服务端 image_artifacts.id，字符串形式，通常是 UUID）。
    pub image_id: String,
    /// `sha256(manifest_json)`（64 位小写 hex）。
    pub manifest_digest: String,
    /// 例如 `manifest_ready`。
    #[serde(default)]
    pub status: Option<String>,
}

/// 交换一个对象（sha256+size）的上传预签名 URL。
pub async fn presign_upload(
    cfg: &TransportConfig,
    sha256: &str,
    size_bytes: u64,
) -> Result<PresignUploadReply> {
    let client = build_mtls_client(cfg)?;
    let url = format!("{}{}", cfg.mtls_endpoint, PRESIGN_UPLOAD_PATH);
    let body = PresignUploadRequest {
        sha256: sha256.to_owned(),
        size_bytes,
    };
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Transport(format!("请求 presign-upload 失败（mTLS）: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::Unexpected(format!(
            "POST {PRESIGN_UPLOAD_PATH} 返回 {status}: {body}"
        )));
    }
    resp.json()
        .await
        .map_err(|e| Error::Transport(format!("解析 presign-upload 响应失败: {e}")))
}

/// 登记镜像（device-pki 校验所有 part 对象后构造 manifest 并签名）。
pub async fn register_image(
    cfg: &TransportConfig,
    request: &RegisterImageRequest,
) -> Result<RegisterImageReply> {
    let client = build_mtls_client(cfg)?;
    let url = format!("{}{}", cfg.mtls_endpoint, REGISTER_PATH);
    let resp = client
        .post(&url)
        .json(request)
        .send()
        .await
        .map_err(|e| Error::Transport(format!("请求 register 失败（mTLS）: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::Unexpected(format!(
            "POST {REGISTER_PATH} 返回 {status}: {body}"
        )));
    }
    resp.json()
        .await
        .map_err(|e| Error::Transport(format!("解析 register 响应失败: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presign_request_body_shape() {
        let body = PresignUploadRequest {
            sha256: "a".repeat(64),
            size_bytes: 33_554_432,
        };
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"sha256": "a".repeat(64), "size_bytes": 33_554_432})
        );
    }

    #[test]
    fn presign_reply_parses_full_and_already_exists() {
        let reply: PresignUploadReply = serde_json::from_str(
            r#"{"object_key":"objects/aaa","upload_url":"https://s3/presigned",
                "expires_at":"2026-11-08T00:00:00Z","already_exists":false}"#,
        )
        .unwrap();
        assert_eq!(reply.object_key, "objects/aaa");
        assert_eq!(reply.upload_url.as_deref(), Some("https://s3/presigned"));
        assert!(!reply.already_exists);

        // 已存在时服务端可能只回 object_key/already_exists
        let reply: PresignUploadReply =
            serde_json::from_str(r#"{"object_key":"objects/aaa","already_exists":true}"#).unwrap();
        assert!(reply.already_exists);
        assert!(reply.upload_url.is_none());
    }

    #[test]
    fn register_request_shape() {
        let request = RegisterImageRequest::new_vm_disk(
            "base-win11",
            "1.0.0",
            "linux/amd64",
            33_554_432,
            vec![ImagePart {
                sha256: "b".repeat(64),
                size: 33_554_432,
            }],
        )
        .with_annotations(Some(serde_json::json!({"base_volume": "base-win11"})));
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["name"], "base-win11");
        assert_eq!(value["version"], "1.0.0");
        assert_eq!(value["platform"], "linux/amd64");
        assert_eq!(value["kind"], "vm_disk");
        assert_eq!(value["format"], "qcow2");
        assert_eq!(value["compression"], "none");
        assert_eq!(value["total_size"], 33_554_432u64);
        assert_eq!(value["parts"][0]["sha256"], "b".repeat(64));
        assert_eq!(value["parts"][0]["size"], 33_554_432u64);
        assert_eq!(value["annotations"]["base_volume"], "base-win11");

        // annotations 为空时不应序列化该字段
        let request = RegisterImageRequest::new_vm_disk(
            "x",
            "1",
            "linux/amd64",
            1,
            vec![ImagePart {
                sha256: "c".repeat(64),
                size: 1,
            }],
        );
        let value = serde_json::to_value(&request).unwrap();
        assert!(value.get("annotations").is_none());
    }

    #[test]
    fn register_reply_parses_with_and_without_status() {
        let reply: RegisterImageReply = serde_json::from_str(
            r#"{"image_id":"img-1","manifest_digest":"d","status":"manifest_ready"}"#,
        )
        .unwrap();
        assert_eq!(reply.image_id, "img-1");
        assert_eq!(reply.manifest_digest, "d");
        assert_eq!(reply.status.as_deref(), Some("manifest_ready"));

        let reply: RegisterImageReply =
            serde_json::from_str(r#"{"image_id":"img-2","manifest_digest":"e"}"#).unwrap();
        assert!(reply.status.is_none());
    }
}
