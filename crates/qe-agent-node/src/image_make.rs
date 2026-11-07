//! `image.make`：VM 导出 → 32 MiB 分片 → 逐片 presign/PUT → register。
//!
//! 阶段 2「从 VM 制作镜像」的 agent 侧实现（设计见
//! `room_node/docs/vm-base-image-and-diff-disk.md` §8.5）：
//!
//! 1. 调用方先 `qemu-img convert` 导出扁平 qcow2（见 `vm_control`）；
//! 2. 本模块按 [`CHUNK_SIZE`]（32 MiB）切分，逐片计算 sha256；
//! 3. 每片向 device-pki 换 `presign-upload` URL；`already_exists=true` 时跳过 PUT；
//! 4. 全部上传后 `register`（device-pki 校验对象并构造/签名 manifest）；
//! 5. 返回 image_id / manifest_digest 等；调用方负责删除本地导出文件。
//!
//! 切片大小远小于 S3/verify 的 64 MiB get_bytes 上限，因此复用现有
//! image_chunks + verify + publish 全链路。

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use qe_device_pki_client::image::{
    ImagePart, RegisterImageRequest, presign_upload, register_image,
};
use qe_device_pki_client::transport::TransportConfig;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// 分片大小：32 MiB。
pub const CHUNK_SIZE: u64 = 32 * 1024 * 1024;
/// 最多分片数（manifest/verify 上限）。
pub const MAX_PARTS: usize = 2048;
/// 单镜像最大字节数：MAX_PARTS × CHUNK_SIZE = 64 GiB。
pub const MAX_TOTAL_SIZE: u64 = 64 * 1024 * 1024 * 1024;
/// 单次 PUT 上传超时（契约 600s）。
pub const UPLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// 一个分片在导出文件中的位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkSlice {
    pub offset: u64,
    pub size: u64,
}

/// 按 32 MiB 规划分片（最后一片可小于 32 MiB）。
///
/// 拒绝空文件、超过 64 GiB 或分片数超过 [`MAX_PARTS`] 的输入。
pub fn plan_chunks(total_size: u64) -> Result<Vec<ChunkSlice>> {
    if total_size == 0 {
        bail!("镜像文件为空（total_size=0），拒绝上传");
    }
    if total_size > MAX_TOTAL_SIZE {
        bail!(
            "镜像过大：{} 字节 > 上限 {} 字节（{} GiB）",
            total_size,
            MAX_TOTAL_SIZE,
            MAX_TOTAL_SIZE / (1024 * 1024 * 1024)
        );
    }
    let mut slices = Vec::with_capacity(total_size.div_ceil(CHUNK_SIZE) as usize);
    let mut offset = 0u64;
    while offset < total_size {
        let size = CHUNK_SIZE.min(total_size - offset);
        slices.push(ChunkSlice { offset, size });
        offset += size;
    }
    if slices.len() > MAX_PARTS {
        bail!("分片数 {} 超过上限 {MAX_PARTS}", slices.len());
    }
    Ok(slices)
}

/// 计算文件指定区间的 sha256（小写 hex）。
pub fn sha256_range(path: &Path, slice: &ChunkSlice) -> Result<String> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("打开镜像文件失败: {}", path.display()))?;
    file.seek(SeekFrom::Start(slice.offset))
        .with_context(|| format!("seek 到分片 offset {} 失败", slice.offset))?;
    let mut hasher = Sha256::new();
    let mut remaining = slice.size;
    let mut buffer = vec![0u8; 256 * 1024];
    while remaining > 0 {
        let want = remaining.min(buffer.len() as u64) as usize;
        let read = file.read(&mut buffer[..want])?;
        if read == 0 {
            bail!(
                "读取分片到文件末尾（offset={}，仍缺 {remaining} 字节）",
                slice.offset
            );
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    Ok(hex::encode(hasher.finalize()))
}

/// 读取文件指定区间到内存（最多一个分片，32 MiB）。
fn read_range(path: &Path, slice: &ChunkSlice) -> Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("打开镜像文件失败: {}", path.display()))?;
    file.seek(SeekFrom::Start(slice.offset))?;
    let mut bytes = vec![0u8; slice.size as usize];
    file.read_exact(&mut bytes)
        .with_context(|| format!("读取分片 offset={} size={} 失败", slice.offset, slice.size))?;
    Ok(bytes)
}

/// `image.make` 的输入（来自 `vm.command` args）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageMakeRequest {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub base_volume: Option<String>,
    pub description: Option<String>,
}

/// 制作完成的镜像信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MadeImage {
    pub image_id: String,
    pub manifest_digest: String,
    pub name: String,
    pub version: String,
    pub total_size: u64,
    pub chunks: usize,
    pub uploaded_chunks: usize,
    pub skipped_chunks: usize,
}

/// 上传 + 登记（异步）。`export_path` 是已导出的扁平 qcow2。
pub async fn upload_and_register(
    transport: &TransportConfig,
    export_path: &Path,
    request: &ImageMakeRequest,
) -> Result<MadeImage> {
    let metadata = std::fs::metadata(export_path)
        .with_context(|| format!("读取导出文件元数据失败: {}", export_path.display()))?;
    if !metadata.is_file() {
        bail!("导出路径不是普通文件: {}", export_path.display());
    }
    let total_size = metadata.len();
    let slices = plan_chunks(total_size)?;

    let put_client = reqwest::Client::builder()
        .timeout(UPLOAD_TIMEOUT)
        .build()
        .context("构建上传 HTTP 客户端失败")?;

    let mut parts = Vec::with_capacity(slices.len());
    let mut uploaded_chunks = 0usize;
    let mut skipped_chunks = 0usize;
    for (index, slice) in slices.iter().enumerate() {
        let sha256 = sha256_range(export_path, slice)
            .with_context(|| format!("image.make 阶段 upload {index}：计算 sha256 失败"))?;
        let presign = presign_upload(transport, &sha256, slice.size)
            .await
            .with_context(|| format!("image.make 阶段 upload {index}：presign-upload 失败"))?;
        if presign.already_exists {
            skipped_chunks += 1;
            tracing::debug!(index, sha256 = %sha256, "对象已存在，跳过上传");
        } else {
            let upload_url = presign.upload_url.as_deref().ok_or_else(|| {
                anyhow::anyhow!("image.make 阶段 upload {index}：presign 响应缺少 upload_url")
            })?;
            let bytes = read_range(export_path, slice)
                .with_context(|| format!("image.make 阶段 upload {index}：读取分片失败"))?;
            let response = put_client
                .put(upload_url)
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .body(bytes)
                .send()
                .await
                .with_context(|| format!("image.make 阶段 upload {index}：PUT 失败"))?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                bail!("image.make 阶段 upload {index}：PUT 返回 {status}: {body}");
            }
            uploaded_chunks += 1;
            tracing::debug!(index, sha256 = %sha256, "分片上传完成");
        }
        parts.push(ImagePart {
            sha256,
            size: slice.size,
        });
    }

    let annotations = build_annotations(request);
    let register = RegisterImageRequest::new_vm_disk(
        request.name.clone(),
        request.version.clone(),
        request.platform.clone(),
        total_size,
        parts,
    )
    .with_annotations(annotations);
    let reply = register_image(transport, &register)
        .await
        .context("image.make 阶段 register：register_image 失败")?;

    Ok(MadeImage {
        image_id: reply.image_id,
        manifest_digest: reply.manifest_digest,
        name: request.name.clone(),
        version: request.version.clone(),
        total_size,
        chunks: slices.len(),
        uploaded_chunks,
        skipped_chunks,
    })
}

impl MadeImage {
    /// 转成 vm.command 的 data JSON。
    pub fn to_json(&self) -> Value {
        json!({
            "image_id": self.image_id,
            "manifest_digest": self.manifest_digest,
            "name": self.name,
            "version": self.version,
            "total_size": self.total_size,
            "chunks": self.chunks,
            "uploaded_chunks": self.uploaded_chunks,
            "skipped_chunks": self.skipped_chunks,
        })
    }
}

/// base_volume / description 放进 manifest annotations（契约无独立字段）。
fn build_annotations(request: &ImageMakeRequest) -> Option<Value> {
    let mut map = serde_json::Map::new();
    if let Some(base_volume) = request.base_volume.as_deref().map(str::trim)
        && !base_volume.is_empty()
    {
        map.insert("base_volume".to_owned(), json!(base_volume));
    }
    if let Some(description) = request.description.as_deref().map(str::trim)
        && !description.is_empty()
    {
        map.insert("description".to_owned(), json!(description));
    }
    if map.is_empty() {
        None
    } else {
        Some(Value::Object(map))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sizes(slices: &[ChunkSlice]) -> Vec<u64> {
        slices.iter().map(|slice| slice.size).collect()
    }

    #[test]
    fn plan_chunks_exact_multiple() {
        let slices = plan_chunks(64 * 1024 * 1024).unwrap();
        assert_eq!(sizes(&slices), vec![CHUNK_SIZE, CHUNK_SIZE]);
        assert_eq!(slices[0].offset, 0);
        assert_eq!(slices[1].offset, CHUNK_SIZE);
    }

    #[test]
    fn plan_chunks_with_remainder() {
        let slices = plan_chunks(CHUNK_SIZE + 1).unwrap();
        assert_eq!(sizes(&slices), vec![CHUNK_SIZE, 1]);
        assert_eq!(slices[1].offset, CHUNK_SIZE);
    }

    #[test]
    fn plan_chunks_single_byte_and_under_chunk() {
        assert_eq!(sizes(&plan_chunks(1).unwrap()), vec![1]);
        assert_eq!(sizes(&plan_chunks(1234).unwrap()), vec![1234]);
    }

    #[test]
    fn plan_chunks_rejects_empty() {
        let error = plan_chunks(0).unwrap_err().to_string();
        assert!(error.contains("为空"), "{error}");
    }

    #[test]
    fn plan_chunks_bounds() {
        // 恰好 64 GiB → 2048 片，允许
        let slices = plan_chunks(MAX_TOTAL_SIZE).unwrap();
        assert_eq!(slices.len(), MAX_PARTS);
        assert_eq!(slices.last().unwrap().size, CHUNK_SIZE);
        // 超过 64 GiB → 拒绝
        assert!(plan_chunks(MAX_TOTAL_SIZE + 1).is_err());
    }

    #[test]
    fn sha256_range_and_read_range_work() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.qcow2");
        let data: Vec<u8> = (0..10_000u32)
            .map(|value| u8::try_from(value % 251).unwrap())
            .collect();
        std::fs::write(&path, &data).unwrap();

        let first = ChunkSlice {
            offset: 0,
            size: 4_000,
        };
        assert_eq!(
            sha256_range(&path, &first).unwrap(),
            sha256_hex(&data[..4_000])
        );
        let second = ChunkSlice {
            offset: 4_000,
            size: 6_000,
        };
        assert_eq!(
            sha256_range(&path, &second).unwrap(),
            sha256_hex(&data[4_000..])
        );
        assert_eq!(read_range(&path, &second).unwrap(), data[4_000..].to_vec());

        // 越界读取会失败
        let beyond = ChunkSlice {
            offset: 9_000,
            size: 2_000,
        };
        assert!(sha256_range(&path, &beyond).is_err());
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hex::encode(hasher.finalize())
    }

    #[test]
    fn annotations_include_base_volume_and_description() {
        let request = ImageMakeRequest {
            name: "base".into(),
            version: "1.0".into(),
            platform: "linux/amd64".into(),
            base_volume: Some("base-win11".into()),
            description: Some("考场基线".into()),
        };
        let annotations = build_annotations(&request).unwrap();
        assert_eq!(annotations["base_volume"], "base-win11");
        assert_eq!(annotations["description"], "考场基线");

        let empty = ImageMakeRequest {
            base_volume: None,
            description: Some("  ".into()),
            ..request
        };
        assert!(build_annotations(&empty).is_none());
    }

    #[test]
    fn made_image_json_shape() {
        let made = MadeImage {
            image_id: "img-1".into(),
            manifest_digest: "d".repeat(64),
            name: "base".into(),
            version: "1.0".into(),
            total_size: 100,
            chunks: 2,
            uploaded_chunks: 1,
            skipped_chunks: 1,
        };
        let value = made.to_json();
        assert_eq!(value["image_id"], "img-1");
        assert_eq!(value["manifest_digest"], "d".repeat(64));
        assert_eq!(value["chunks"], 2);
        assert_eq!(value["uploaded_chunks"], 1);
        assert_eq!(value["skipped_chunks"], 1);
    }
}
