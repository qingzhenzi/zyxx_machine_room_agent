//! 按 manifest 拉取、校验并组装镜像产物（契约 §16.4）。
//!
//! 流程：
//! 1. 通过 mTLS 预签名取 `manifests/{digest}.json`，校验原始字节的 sha256；
//! 2. 用 presign 下发的 Ed25519 公钥对**原始字节**验签；
//! 3. 分片命中缓存直接用（size 校验），缺失的按 `DOWNLOAD_CONCURRENCY` 并发下载，
//!    写入内容寻址缓存前校验 sha256/size；
//! 4. 按 offset 顺序组装，逐片流式重算 sha256（顺带发现被篡改的缓存），
//!    校验总大小与格式 magic（qcow2 / tar）;
//! 5. 进度回调：download 0-80、verify 80-90。
//!
//! 网络字节获取抽象成 [`ObjectFetcher`]，测试可用内存 mock，无需真实 S3。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::BoxFuture;
use crate::cache::ObjectCache;
use crate::manifest::{ImageFormat, ImageKind, Manifest, is_sha256_hex, sha256_hex};
use crate::presign::{PresignClient, PresignReply, PresignRequest};
use crate::report::{Phase, ProgressEvent};

/// 下载对象的超时（分片可能较大，给足时间）。
const FETCH_TIMEOUT: Duration = Duration::from_secs(600);
/// 组装/校验时的流式缓冲区。
const COPY_BUFFER: usize = 1024 * 1024;
/// qcow2 magic：`QFI\xfb`。
const QCOW2_MAGIC: [u8; 4] = [0x51, 0x46, 0x49, 0xfb];
/// tar magic 在 offset 257。
const TAR_MAGIC_OFFSET: u64 = 257;
const TAR_MAGIC: &[u8; 5] = b"ustar";

/// 按 URL 取对象字节。生产实现是 HTTP(S)；测试用内存 mock。
pub trait ObjectFetcher: Send + Sync {
    fn fetch<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Vec<u8>>>;
}

/// 生产实现：普通 HTTPS GET（预签名 URL 自带授权，无需设备证书）。
pub struct HttpObjectFetcher {
    client: reqwest::Client,
}

impl HttpObjectFetcher {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()
            .context("构建对象下载客户端失败")?;
        Ok(Self { client })
    }

    /// 复用外部构造的客户端（测试注入超时/代理等）。
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl ObjectFetcher for HttpObjectFetcher {
    fn fetch<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(async move {
            let response = self
                .client
                .get(url)
                .send()
                .await
                .with_context(|| format!("下载对象失败: GET {url}"))?;
            let status = response.status();
            if !status.is_success() {
                bail!("下载对象失败: GET {url} 返回 {status}");
            }
            let bytes = response
                .bytes()
                .await
                .with_context(|| format!("读取对象响应体失败: GET {url}"))?;
            Ok(bytes.to_vec())
        })
    }
}

/// 一次 pull 的输入（由 `image.pull` 下行 payload 转换而来）。
#[derive(Debug, Clone)]
pub struct PullRequest {
    /// 仅用于日志/进度关联。
    pub transfer_id: String,
    pub manifest_digest: String,
    pub kind: ImageKind,
    pub format: ImageFormat,
    pub name: String,
    pub version: String,
    pub total_size: i64,
    pub chunk_count: usize,
    /// 组装产物落盘目录（每次 transfer 独立）。
    pub staging_dir: PathBuf,
}

/// pull 的结果（已验签、已校验、已组装）。
#[derive(Debug, Clone)]
pub struct PulledImage {
    pub manifest: Manifest,
    pub staging_dir: PathBuf,
    /// 组装后的单个产物（tar / qcow2 / raw）。
    pub artifact_path: PathBuf,
    /// 实际组装字节数（等于 manifest.total_size）。
    pub bytes: i64,
}

/// 拉取器：预签名、下载、缓存、校验、组装。
pub struct Puller {
    presign: Arc<dyn PresignClient>,
    fetcher: Arc<dyn ObjectFetcher>,
    cache: Arc<ObjectCache>,
    concurrency: usize,
}

impl Puller {
    pub fn new(
        presign: Arc<dyn PresignClient>,
        fetcher: Arc<dyn ObjectFetcher>,
        cache: Arc<ObjectCache>,
        concurrency: usize,
    ) -> Self {
        Self {
            presign,
            fetcher,
            cache,
            concurrency: concurrency.max(1),
        }
    }

    /// 执行完整拉取。进度通过同步回调上报（job 负责转成异步上报）。
    pub async fn pull(
        &self,
        request: &PullRequest,
        progress: &(dyn Fn(ProgressEvent) + Send + Sync),
    ) -> Result<PulledImage> {
        if !is_sha256_hex(&request.manifest_digest) {
            bail!(
                "manifest_digest 不是 64 位小写 hex: `{}`",
                request.manifest_digest
            );
        }
        let total = request.total_size;
        if total <= 0 {
            bail!("image.pull 的 total_size 必须 > 0，实际 {total}");
        }
        progress(ProgressEvent::new(Phase::Download, 0, 0));

        let manifest = self.load_manifest(request).await?;
        self.check_request_matches(request, &manifest)?;

        // 先腾空间，并保护本镜像的全部分片（当前版本必须保留）。
        let needed: HashSet<String> = manifest.chunks.iter().map(|c| c.sha256.clone()).collect();
        if let Err(error) = self.cache.evict_to_limit(&needed).await {
            tracing::warn!(error = %error, "缓存预淘汰失败，继续拉取");
        }

        let chunk_paths = self.ensure_chunks(request, &manifest, progress).await?;

        progress(ProgressEvent::new(Phase::Verify, 80, manifest.total_size));
        let artifact_path = self
            .assemble(&manifest, &chunk_paths, &request.staging_dir, progress)
            .await?;
        if let Err(error) = check_magic(&artifact_path, manifest.format).await {
            // 校验失败的产物不能留在 staging，避免 importer 误用。
            let _ = fs::remove_file(&artifact_path).await;
            return Err(error);
        }
        progress(ProgressEvent::new(Phase::Verify, 90, manifest.total_size));

        if let Err(error) = self.cache.evict_to_limit(&needed).await {
            tracing::warn!(error = %error, "缓存淘汰失败，不影响本次拉取");
        }

        tracing::info!(
            transfer_id = %request.transfer_id,
            digest = %request.manifest_digest,
            bytes = manifest.total_size,
            artifact = %artifact_path.display(),
            "镜像拉取、校验、组装完成"
        );

        Ok(PulledImage {
            bytes: manifest.total_size,
            manifest,
            staging_dir: request.staging_dir.clone(),
            artifact_path,
        })
    }

    /// 取 manifest + 签名并验签。
    async fn load_manifest(&self, request: &PullRequest) -> Result<Manifest> {
        let reply = self
            .presign
            .presign(&PresignRequest::manifest(&request.manifest_digest))
            .await
            .context("预签名 manifest 失败")?;
        let url = only_url(&reply, "manifest")?.url.clone();
        let public_key = reply.public_key_base64.clone();
        if public_key.trim().is_empty() {
            bail!("presign 响应缺少 public_key_base64");
        }

        let raw = self
            .fetcher
            .fetch(&url)
            .await
            .with_context(|| format!("下载 manifest 失败: {url}"))?;
        let actual = sha256_hex(&raw);
        if actual != request.manifest_digest {
            bail!(
                "manifest 内容 digest 不符（期望 {}，实际 {actual}）",
                request.manifest_digest
            );
        }
        let manifest: Manifest = serde_json::from_slice(&raw).with_context(|| {
            format!(
                "manifest JSON 解析失败（digest={}）",
                request.manifest_digest
            )
        })?;
        manifest.validate().context("manifest 结构校验失败")?;

        // 规范化一致性：反序列化后再序列化必须仍得到同一个 digest。
        let canonical = manifest.digest_hex()?;
        if canonical != request.manifest_digest {
            bail!(
                "manifest 规范化序列化后的 digest 与请求不符（期望 {}，实际 {canonical}），\
                 可能是 agent 与服务端字段顺序/类型漂移",
                request.manifest_digest
            );
        }

        let reply = self
            .presign
            .presign(&PresignRequest::signature(&request.manifest_digest))
            .await
            .context("预签名 manifest 签名失败")?;
        let signature_url = only_url(&reply, "signature")?.url.clone();
        let signature_raw = self
            .fetcher
            .fetch(&signature_url)
            .await
            .with_context(|| format!("下载 manifest 签名失败: {signature_url}"))?;
        let signature = decode_signature(&signature_raw)?;
        manifest
            .verify_signature_bytes(&public_key, &raw, &signature)
            .context("manifest Ed25519 验签失败，拒绝使用该镜像")?;

        tracing::debug!(
            digest = %request.manifest_digest,
            chunks = manifest.chunks.len(),
            "manifest 已验签"
        );
        Ok(manifest)
    }

    fn check_request_matches(&self, request: &PullRequest, manifest: &Manifest) -> Result<()> {
        if manifest.kind != request.kind {
            bail!(
                "下行 kind={} 与 manifest kind={} 不一致",
                request.kind.as_str(),
                manifest.kind.as_str()
            );
        }
        if manifest.format != request.format {
            bail!(
                "下行 format={} 与 manifest format={} 不一致",
                request.format.as_str(),
                manifest.format.as_str()
            );
        }
        if manifest.total_size != request.total_size {
            bail!(
                "下行 total_size={} 与 manifest total_size={} 不一致",
                request.total_size,
                manifest.total_size
            );
        }
        if manifest.chunks.len() != request.chunk_count {
            bail!(
                "下行 chunk_count={} 与 manifest 分片数={} 不一致",
                request.chunk_count,
                manifest.chunks.len()
            );
        }
        if manifest.name != request.name || manifest.version != request.version {
            tracing::warn!(
                payload = format!("{}:{}", request.name, request.version),
                manifest = format!("{}:{}", manifest.name, manifest.version),
                "下行 name/version 与 manifest 不一致，以 manifest 为准"
            );
        }
        Ok(())
    }

    /// 确保全部分片在缓存中，返回按 index 排列的本地路径。
    async fn ensure_chunks(
        &self,
        request: &PullRequest,
        manifest: &Manifest,
        progress: &(dyn Fn(ProgressEvent) + Send + Sync),
    ) -> Result<Vec<PathBuf>> {
        let mut paths: Vec<Option<PathBuf>> = vec![None; manifest.chunks.len()];
        let mut missing: Vec<usize> = Vec::new();
        for (index, chunk) in manifest.chunks.iter().enumerate() {
            match self.cache.get(&chunk.sha256, Some(chunk.size)).await? {
                Some(path) => {
                    tracing::debug!(index, sha256 = %chunk.sha256, "分片缓存命中");
                    paths[index] = Some(path);
                }
                None => missing.push(index),
            }
        }

        if !missing.is_empty() {
            let urls = self
                .presign_chunks(&request.manifest_digest, manifest, &missing)
                .await?;
            let done_bytes = AtomicI64::new(
                manifest
                    .chunks
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| paths[*index].is_some())
                    .map(|(_, chunk)| chunk.size)
                    .sum(),
            );
            let total = manifest.total_size.max(1);
            let futures = missing.iter().copied().map(|index| {
                let chunk = &manifest.chunks[index];
                let url = urls.get(&index).cloned();
                let fetcher = self.fetcher.as_ref();
                let cache = self.cache.as_ref();
                let done_bytes = &done_bytes;
                async move {
                    let url = url.with_context(|| format!("缺少分片 {index} 的预签名 URL"))?;
                    let bytes = fetcher
                        .fetch(&url)
                        .await
                        .with_context(|| format!("下载分片 {index} 失败: {url}"))?;
                    if bytes.len() as i64 != chunk.size {
                        bail!(
                            "分片 {index} size 不符（期望 {}，实际 {}）",
                            chunk.size,
                            bytes.len()
                        );
                    }
                    let path = cache
                        .put_bytes(&chunk.sha256, Some(chunk.size), &bytes)
                        .await
                        .with_context(|| format!("分片 {index} 校验/入缓存失败"))?;
                    let done = done_bytes.fetch_add(chunk.size, Ordering::Relaxed) + chunk.size;
                    progress(ProgressEvent::new(
                        Phase::Download,
                        download_progress(done, total),
                        done,
                    ));
                    Ok::<(usize, PathBuf), anyhow::Error>((index, path))
                }
            });
            let results: Vec<Result<(usize, PathBuf)>> = futures_util::stream::iter(futures)
                .buffer_unordered(self.concurrency)
                .collect()
                .await;
            for result in results {
                let (index, path) = result?;
                paths[index] = Some(path);
            }
        }

        let mut ordered = Vec::with_capacity(paths.len());
        for (index, path) in paths.into_iter().enumerate() {
            ordered.push(path.with_context(|| format!("分片 {index} 缺失"))?);
        }
        Ok(ordered)
    }

    /// 为缺失分片换取预签名 URL，按批（<=64）请求。
    async fn presign_chunks(
        &self,
        manifest_digest: &str,
        manifest: &Manifest,
        missing: &[usize],
    ) -> Result<HashMap<usize, String>> {
        let mut urls = HashMap::new();
        let mut start = 0usize;
        while start < missing.len() {
            let first = missing[start];
            let mut end = start + 1;
            while end < missing.len()
                && missing[end] == missing[end - 1] + 1
                && (end - start) < crate::presign::MAX_CHUNKS_PER_PRESIGN as usize
            {
                end += 1;
            }
            let batch = &missing[start..end];
            let reply = self
                .presign
                .presign(&PresignRequest::chunk_batch(
                    manifest_digest,
                    i32::try_from(first).context("分片 index 溢出 i32")?,
                    i32::try_from(batch.len()).context("批量大小溢出 i32")?,
                ))
                .await
                .with_context(|| format!("预签名分片批次失败（起始 index={first}）"))?;
            for (position, &index) in batch.iter().enumerate() {
                let chunk = &manifest.chunks[index];
                let expected_key = format!("objects/{}", chunk.sha256);
                let url = reply
                    .urls
                    .iter()
                    .find(|item| item.key == expected_key)
                    .or_else(|| reply.urls.get(position))
                    .map(|item| item.url.clone())
                    .with_context(|| {
                        format!("预签名响应缺少分片 {index} 的 URL（期望 key={expected_key}）")
                    })?;
                urls.insert(index, url);
            }
            start = end;
        }
        Ok(urls)
    }

    /// 按顺序组装并逐片流式校验 sha256；返回产物路径。
    async fn assemble(
        &self,
        manifest: &Manifest,
        chunk_paths: &[PathBuf],
        staging_dir: &Path,
        progress: &(dyn Fn(ProgressEvent) + Send + Sync),
    ) -> Result<PathBuf> {
        fs::create_dir_all(staging_dir)
            .await
            .with_context(|| format!("创建 staging 目录失败: {}", staging_dir.display()))?;
        let artifact_path = staging_dir.join(artifact_filename(manifest));
        let mut output = fs::File::create(&artifact_path)
            .await
            .with_context(|| format!("创建产物文件失败: {}", artifact_path.display()))?;

        let total = manifest.total_size.max(1);
        let mut verified: i64 = 0;
        let mut position: i64 = 0;
        let mut buffer = vec![0u8; COPY_BUFFER];

        for (index, chunk) in manifest.chunks.iter().enumerate() {
            if chunk.offset != position {
                bail!(
                    "分片 {index} 的 offset={} 与已写入位置 {position} 不一致",
                    chunk.offset
                );
            }
            let source = chunk_paths
                .get(index)
                .with_context(|| format!("分片 {index} 没有本地路径"))?;
            let mut file = fs::File::open(source)
                .await
                .with_context(|| format!("打开缓存分片失败: {}", source.display()))?;
            let mut hasher = Sha256::new();
            let mut copied: i64 = 0;
            loop {
                let read = file
                    .read(&mut buffer)
                    .await
                    .with_context(|| format!("读取缓存分片失败: {}", source.display()))?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
                output
                    .write_all(&buffer[..read])
                    .await
                    .with_context(|| format!("写产物失败: {}", artifact_path.display()))?;
                copied += read as i64;
            }
            let actual = hex::encode(hasher.finalize());
            if copied != chunk.size {
                let _ = fs::remove_file(&artifact_path).await;
                bail!(
                    "分片 {index} 缓存内容 size 不符（期望 {}，实际 {copied}）",
                    chunk.size
                );
            }
            if actual != chunk.sha256 {
                let _ = fs::remove_file(&artifact_path).await;
                // 缓存内容坏了：删掉，下一次 pull 会重新下载。
                let _ = fs::remove_file(source).await;
                bail!(
                    "分片 {index} 缓存内容被篡改（期望 {}，实际 {actual}），已删除缓存条目",
                    chunk.sha256
                );
            }
            position += copied;
            verified += copied;
            progress(ProgressEvent::new(
                Phase::Verify,
                verify_progress(verified, total),
                verified,
            ));
        }

        output
            .flush()
            .await
            .with_context(|| format!("刷产物失败: {}", artifact_path.display()))?;
        drop(output);

        let metadata = fs::metadata(&artifact_path)
            .await
            .with_context(|| format!("读取产物元数据失败: {}", artifact_path.display()))?;
        if metadata.len() != u64::try_from(manifest.total_size).context("total_size 非法")? {
            let _ = fs::remove_file(&artifact_path).await;
            bail!(
                "产物总大小不符（期望 {}，实际 {}）",
                manifest.total_size,
                metadata.len()
            );
        }
        if verified != manifest.total_size {
            let _ = fs::remove_file(&artifact_path).await;
            bail!(
                "组装字节数不符（期望 {}，实际 {verified}）",
                manifest.total_size
            );
        }
        Ok(artifact_path)
    }
}

/// 下载阶段百分比（0-80）。
fn download_progress(done: i64, total: i64) -> i32 {
    if total <= 0 {
        return 0;
    }
    ((done.max(0).saturating_mul(80)) / total).clamp(0, 80) as i32
}

/// 校验/组装阶段百分比（80-90）。
fn verify_progress(done: i64, total: i64) -> i32 {
    if total <= 0 {
        return 80;
    }
    80 + ((done.max(0).saturating_mul(10)) / total).clamp(0, 10) as i32
}

/// 产物文件名：`<name>-<version>.<ext>`，非法字符替换为 `_`。
pub fn artifact_filename(manifest: &Manifest) -> String {
    format!(
        "{}-{}.{}",
        sanitize_component(&manifest.name),
        sanitize_component(&manifest.version),
        manifest.format.file_extension()
    )
}

fn sanitize_component(raw: &str) -> String {
    let sanitized: String = raw
        .trim()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "image".to_owned()
    } else {
        sanitized
    }
}

/// 校验格式 magic：qcow2 头 4 字节、tar offset 257 的 `ustar`；raw 跳过。
async fn check_magic(path: &Path, format: ImageFormat) -> Result<()> {
    match format {
        ImageFormat::Qcow2 => {
            let mut file = fs::File::open(path)
                .await
                .with_context(|| format!("打开产物失败: {}", path.display()))?;
            let mut magic = [0u8; 4];
            file.read_exact(&mut magic)
                .await
                .with_context(|| format!("qcow2 文件过短，读不到 magic: {}", path.display()))?;
            if magic != QCOW2_MAGIC {
                bail!(
                    "qcow2 magic 不符（期望 QFI\\xfb，实际 {:02x?}）: {}",
                    magic,
                    path.display()
                );
            }
        }
        ImageFormat::Tar => {
            let mut file = fs::File::open(path)
                .await
                .with_context(|| format!("打开产物失败: {}", path.display()))?;
            file.seek(std::io::SeekFrom::Start(TAR_MAGIC_OFFSET))
                .await
                .with_context(|| format!("tar 文件过短，无法 seek: {}", path.display()))?;
            let mut magic = [0u8; 5];
            file.read_exact(&mut magic)
                .await
                .with_context(|| format!("tar 文件过短，读不到 magic: {}", path.display()))?;
            if &magic != TAR_MAGIC {
                bail!(
                    "tar magic 不符（offset 257 期望 ustar，实际 {:?}）: {}",
                    String::from_utf8_lossy(&magic),
                    path.display()
                );
            }
        }
        // raw 不做 magic；docker_archive / oci_layout 的容器格式由 importer 判断。
        ImageFormat::Raw | ImageFormat::DockerArchive | ImageFormat::OciLayout => {}
    }
    Ok(())
}

/// 签名对象可能是 64 字节原始签名，也可能是 base64/hex 文本（不同部署的写法）。
fn decode_signature(raw: &[u8]) -> Result<Vec<u8>> {
    if raw.len() == 64 {
        return Ok(raw.to_vec());
    }
    let text = std::str::from_utf8(raw)
        .context("签名对象既不是 64 字节原始签名，也不是文本")?
        .trim();
    if let Ok(decoded) = STANDARD.decode(text)
        && decoded.len() == 64
    {
        return Ok(decoded);
    }
    if let Ok(decoded) = hex::decode(text)
        && decoded.len() == 64
    {
        return Ok(decoded);
    }
    bail!("签名对象格式不合法（期望 64 字节原始签名或 base64/hex 文本）");
}

fn only_url<'a>(reply: &'a PresignReply, what: &str) -> Result<&'a crate::presign::UrlItem> {
    match reply.urls.as_slice() {
        [one] => Ok(one),
        [] => bail!("预签名响应没有 {what} URL"),
        many => bail!("预签名响应返回了 {} 个 {what} URL，期望 1 个", many.len()),
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    //! pull / job 测试共用的 manifest 夹具与 mock。
    use super::*;
    use crate::manifest::{Compression, ManifestChunk, ManifestRuntime};
    use base64::engine::general_purpose::STANDARD;
    use ed25519_dalek::{Signer as _, SigningKey};
    use std::collections::HashMap;
    use std::sync::Mutex;

    pub(crate) const TEST_KEY_SEED: [u8; 32] = [7u8; 32];
    pub(crate) const OTHER_KEY_SEED: [u8; 32] = [9u8; 32];

    pub(crate) fn signing_key(seed: [u8; 32]) -> SigningKey {
        SigningKey::from_bytes(&seed)
    }

    pub(crate) fn manifest_url(digest: &str) -> String {
        format!("http://mock.test/manifests/{digest}.json")
    }

    pub(crate) fn signature_url(digest: &str) -> String {
        format!("http://mock.test/signatures/{digest}.sig")
    }

    pub(crate) fn object_url(sha256: &str) -> String {
        format!("http://mock.test/objects/{sha256}")
    }

    /// 一份已签名、digest 已知的 manifest 及其对象字节。
    #[derive(Clone)]
    pub(crate) struct Fixture {
        pub manifest: Manifest,
        pub raw: Vec<u8>,
        pub digest: String,
        pub signature: Vec<u8>,
        pub public_key_base64: String,
        pub chunk_data: Vec<Vec<u8>>,
    }

    pub(crate) fn fixture(
        kind: ImageKind,
        format: ImageFormat,
        chunk_data: Vec<Vec<u8>>,
    ) -> Fixture {
        let mut offset = 0i64;
        let chunks: Vec<ManifestChunk> = chunk_data
            .iter()
            .enumerate()
            .map(|(index, data)| {
                let chunk = ManifestChunk {
                    index: i32::try_from(index).unwrap(),
                    object_key: format!("objects/{}", sha256_hex(data)),
                    sha256: sha256_hex(data),
                    size: data.len() as i64,
                    offset,
                };
                offset += data.len() as i64;
                chunk
            })
            .collect();

        let manifest = Manifest {
            schema: 1,
            kind,
            name: "demo".into(),
            version: "1.0".into(),
            platform: "linux/amd64".into(),
            format,
            compression: Compression::None,
            total_size: offset,
            chunks,
            runtime: ManifestRuntime {
                docker_image_ref: Some("demo:1.0".into()),
                ..ManifestRuntime::default()
            },
            annotations: serde_json::json!({}),
            created_at: "2024-01-02T03:04:05Z".parse().unwrap(),
            created_by: None,
        };
        let raw = manifest.to_json().unwrap();
        let digest = sha256_hex(&raw);
        let key = signing_key(TEST_KEY_SEED);
        let signature = key.sign(&raw).to_bytes().to_vec();
        let public_key_base64 = STANDARD.encode(key.verifying_key().as_bytes());
        Fixture {
            manifest,
            raw,
            digest,
            signature,
            public_key_base64,
            chunk_data,
        }
    }

    /// job 测试用的小镜像：`vm_disk` / `raw`，两个分片。
    pub(crate) fn raw_fixture_for_job() -> Fixture {
        fixture(
            ImageKind::VmDisk,
            ImageFormat::Raw,
            vec![b"first-chunk".to_vec(), b"second-chunk".to_vec()],
        )
    }

    /// 合法的 tar 头（512 字节，offset 257 为 `ustar`）。
    pub(crate) fn tar_bytes(payload: &[u8]) -> Vec<u8> {
        let mut tar = vec![0u8; 512];
        tar[..payload.len().min(512)].copy_from_slice(&payload[..payload.len().min(512)]);
        tar[257..262].copy_from_slice(b"ustar");
        tar
    }

    /// 合法的 qcow2 头（`QFI\xfb` + 填充）。
    pub(crate) fn qcow2_bytes(payload: &[u8]) -> Vec<u8> {
        let mut data = vec![0x51, 0x46, 0x49, 0xfb];
        data.extend_from_slice(payload);
        data
    }

    /// mock 预签名服务：按 kind/index/count 生成固定 URL。
    pub(crate) struct MockPresign {
        pub fixture: Arc<Fixture>,
        /// 覆盖下发的公钥（用于"换钥失败"场景）。
        pub public_key_base64: Option<String>,
        /// 记录收到的请求，便于断言。
        pub requests: Mutex<Vec<PresignRequest>>,
    }

    impl MockPresign {
        pub(crate) fn new(fixture: Arc<Fixture>) -> Self {
            Self {
                fixture,
                public_key_base64: None,
                requests: Mutex::new(Vec::new()),
            }
        }

        pub(crate) fn with_public_key(mut self, public_key_base64: impl Into<String>) -> Self {
            self.public_key_base64 = Some(public_key_base64.into());
            self
        }

        pub(crate) fn requests(&self) -> Vec<PresignRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl PresignClient for MockPresign {
        fn presign<'a>(
            &'a self,
            request: &'a PresignRequest,
        ) -> BoxFuture<'a, Result<PresignReply>> {
            Box::pin(async move {
                self.requests.lock().unwrap().push(request.clone());
                let digest = &self.fixture.digest;
                let mut urls = Vec::new();
                match request.kind {
                    crate::presign::PresignKind::Manifest => urls.push(crate::presign::UrlItem {
                        key: format!("manifests/{digest}.json"),
                        url: manifest_url(digest),
                        expires_at: chrono::Utc::now(),
                    }),
                    crate::presign::PresignKind::Signature => urls.push(crate::presign::UrlItem {
                        key: format!("signatures/{digest}.sig"),
                        url: signature_url(digest),
                        expires_at: chrono::Utc::now(),
                    }),
                    crate::presign::PresignKind::Chunk => {
                        let first = request.index.max(0) as usize;
                        let count = request.count.clamp(1, 64) as usize;
                        for index in first..first.saturating_add(count) {
                            let Some(chunk) = self.fixture.manifest.chunks.get(index) else {
                                break;
                            };
                            urls.push(crate::presign::UrlItem {
                                key: format!("objects/{}", chunk.sha256),
                                url: object_url(&chunk.sha256),
                                expires_at: chrono::Utc::now(),
                            });
                        }
                    }
                }
                Ok(PresignReply {
                    urls,
                    public_key_base64: self
                        .public_key_base64
                        .clone()
                        .unwrap_or_else(|| self.fixture.public_key_base64.clone()),
                })
            })
        }
    }

    /// mock 对象下载：URL → 字节，并统计每个 URL 的下载次数。
    pub(crate) struct MockFetcher {
        pub objects: HashMap<String, Vec<u8>>,
        pub fetch_counts: Mutex<HashMap<String, usize>>,
    }

    impl MockFetcher {
        pub(crate) fn new(fixture: &Fixture) -> Self {
            let mut objects = HashMap::new();
            objects.insert(manifest_url(&fixture.digest), fixture.raw.clone());
            objects.insert(signature_url(&fixture.digest), fixture.signature.clone());
            for (chunk, data) in fixture.manifest.chunks.iter().zip(&fixture.chunk_data) {
                objects.insert(object_url(&chunk.sha256), data.clone());
            }
            Self {
                objects,
                fetch_counts: Mutex::new(HashMap::new()),
            }
        }

        pub(crate) fn fetch_count(&self, url: &str) -> usize {
            self.fetch_counts
                .lock()
                .unwrap()
                .get(url)
                .copied()
                .unwrap_or(0)
        }

        pub(crate) fn replace(&mut self, url: &str, data: Vec<u8>) {
            self.objects.insert(url.to_owned(), data);
        }
    }

    impl ObjectFetcher for MockFetcher {
        fn fetch<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Vec<u8>>> {
            Box::pin(async move {
                *self
                    .fetch_counts
                    .lock()
                    .unwrap()
                    .entry(url.to_owned())
                    .or_insert(0) += 1;
                self.objects
                    .get(url)
                    .cloned()
                    .with_context(|| format!("mock 没有这个 URL: {url}"))
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::*;
    use super::*;
    use std::sync::Mutex;

    fn raw_fixture() -> Fixture {
        raw_fixture_for_job()
    }

    fn request_for(fixture: &Fixture, staging: PathBuf) -> PullRequest {
        PullRequest {
            transfer_id: "transfer-1".into(),
            manifest_digest: fixture.digest.clone(),
            kind: fixture.manifest.kind,
            format: fixture.manifest.format,
            name: fixture.manifest.name.clone(),
            version: fixture.manifest.version.clone(),
            total_size: fixture.manifest.total_size,
            chunk_count: fixture.manifest.chunks.len(),
            staging_dir: staging,
        }
    }

    struct Harness {
        _dir: tempfile::TempDir,
        fixture: Arc<Fixture>,
        cache: Arc<ObjectCache>,
        presign: Arc<MockPresign>,
        fetcher: Arc<MockFetcher>,
        puller: Puller,
        staging: PathBuf,
        events: Arc<Mutex<Vec<ProgressEvent>>>,
    }

    impl Harness {
        fn new(fixture: Fixture) -> Self {
            let fixture = Arc::new(fixture);
            let dir = tempfile::tempdir().unwrap();
            let cache = Arc::new(ObjectCache::new(dir.path().join("cache"), 1 << 30));
            let presign = Arc::new(MockPresign::new(fixture.clone()));
            let fetcher = Arc::new(MockFetcher::new(&fixture));
            let puller = Puller::new(presign.clone(), fetcher.clone(), cache.clone(), 2);
            let staging = dir.path().join("staging");
            Self {
                _dir: dir,
                fixture,
                cache,
                presign,
                fetcher,
                puller,
                staging,
                events: Arc::new(Mutex::new(Vec::new())),
            }
        }

        async fn pull(&self, fixture: &Fixture) -> Result<PulledImage> {
            let request = request_for(fixture, self.staging.clone());
            let events = self.events.clone();
            self.puller
                .pull(&request, &move |event| {
                    events.lock().unwrap().push(event);
                })
                .await
        }

        fn events(&self) -> Vec<ProgressEvent> {
            self.events.lock().unwrap().clone()
        }
    }

    #[tokio::test]
    async fn pulls_verifies_and_assembles() {
        let harness = Harness::new(raw_fixture());
        let fixture = harness.fixture.clone();
        let pulled = harness.pull(&fixture).await.unwrap();

        let mut expected = Vec::new();
        for data in &fixture.chunk_data {
            expected.extend_from_slice(data);
        }
        assert_eq!(
            tokio::fs::read(&pulled.artifact_path).await.unwrap(),
            expected
        );
        assert_eq!(pulled.bytes, fixture.manifest.total_size);
        assert_eq!(pulled.artifact_path, harness.staging.join("demo-1.0.raw"));
        assert_eq!(pulled.manifest.digest_hex().unwrap(), fixture.digest);

        // 分片全部进缓存
        for chunk in &fixture.manifest.chunks {
            assert!(
                harness
                    .cache
                    .get_verified(&chunk.sha256)
                    .await
                    .unwrap()
                    .is_some(),
                "分片 {} 应已缓存",
                chunk.sha256
            );
        }

        let events = harness.events();
        assert_eq!(
            events.first(),
            Some(&ProgressEvent::new(Phase::Download, 0, 0))
        );
        assert!(
            events
                .iter()
                .any(|event| event.phase == Phase::Download && event.progress == 80),
            "下载阶段应推进到 80%: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|event| event.phase == Phase::Verify && event.progress == 90),
            "校验阶段应推进到 90%: {events:?}"
        );
        assert_eq!(events.last().unwrap().phase, Phase::Verify);
        let download: Vec<i32> = events
            .iter()
            .filter(|event| event.phase == Phase::Download)
            .map(|event| event.progress)
            .collect();
        assert!(
            download.windows(2).all(|pair| pair[0] <= pair[1]),
            "{download:?}"
        );

        // 预签名请求：manifest + signature + 1 个分片批次
        let requests = harness.presign.requests();
        assert!(
            requests
                .iter()
                .any(|request| request.kind == crate::presign::PresignKind::Manifest)
        );
        assert!(
            requests
                .iter()
                .any(|request| request.kind == crate::presign::PresignKind::Signature)
        );
        let chunk_requests: Vec<_> = requests
            .iter()
            .filter(|request| request.kind == crate::presign::PresignKind::Chunk)
            .collect();
        assert_eq!(chunk_requests.len(), 1);
        assert_eq!((chunk_requests[0].index, chunk_requests[0].count), (0, 2));
    }

    #[tokio::test]
    async fn second_pull_reuses_cached_chunks() {
        let harness = Harness::new(raw_fixture());
        let fixture = harness.fixture.clone();
        harness.pull(&fixture).await.unwrap();

        let object_urls: Vec<String> = fixture
            .manifest
            .chunks
            .iter()
            .map(|chunk| object_url(&chunk.sha256))
            .collect();
        for url in &object_urls {
            assert_eq!(harness.fetcher.fetch_count(url), 1);
        }

        harness.pull(&fixture).await.unwrap();
        for url in &object_urls {
            assert_eq!(harness.fetcher.fetch_count(url), 1, "分片不应重复下载");
        }
        assert!(harness.fetcher.fetch_count(&manifest_url(&fixture.digest)) >= 2);
        assert!(harness.fetcher.fetch_count(&signature_url(&fixture.digest)) >= 2);
    }

    #[tokio::test]
    async fn rejects_wrong_public_key() {
        let fixture = Arc::new(raw_fixture());
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(ObjectCache::new(dir.path(), 1 << 30));
        let other = ed25519_dalek::SigningKey::from_bytes(&OTHER_KEY_SEED);
        let presign = Arc::new(
            MockPresign::new(fixture.clone())
                .with_public_key(STANDARD.encode(other.verifying_key().as_bytes())),
        );
        let fetcher = Arc::new(MockFetcher::new(&fixture));
        let puller = Puller::new(presign, fetcher, cache, 1);
        let request = request_for(&fixture, dir.path().join("staging"));
        let error = puller
            .pull(&request, &|_| {})
            .await
            .expect_err("换钥必须失败");
        let message = format!("{error:#}");
        assert!(message.contains("验签"), "{message}");
    }

    #[tokio::test]
    async fn rejects_tampered_manifest_bytes() {
        let harness = Harness::new(raw_fixture());
        let fixture = harness.fixture.clone();
        let mut fetcher = MockFetcher::new(&fixture);
        fetcher.replace(&manifest_url(&fixture.digest), b"{\"schema\":1}".to_vec());
        let puller = Puller::new(
            Arc::new(MockPresign::new(fixture.clone())),
            Arc::new(fetcher),
            harness.cache.clone(),
            1,
        );
        let request = request_for(&fixture, harness.staging.clone());
        let error = puller.pull(&request, &|_| {}).await.unwrap_err();
        assert!(format!("{error:#}").contains("digest"), "{error:#}");
    }

    #[tokio::test]
    async fn rejects_tampered_chunk_bytes() {
        let harness = Harness::new(raw_fixture());
        let fixture = harness.fixture.clone();
        let mut fetcher = MockFetcher::new(&fixture);
        let url = object_url(&fixture.manifest.chunks[0].sha256);
        fetcher.replace(&url, b"tampered".to_vec());
        let puller = Puller::new(
            Arc::new(MockPresign::new(fixture.clone())),
            Arc::new(fetcher),
            harness.cache.clone(),
            1,
        );
        let request = request_for(&fixture, harness.staging.clone());
        let error = puller.pull(&request, &|_| {}).await.unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("size") || message.contains("sha256"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn rejects_corrupt_tar_magic() {
        let harness = Harness::new(fixture(
            ImageKind::LxcRootfs,
            ImageFormat::Tar,
            vec![b"not-a-tar".to_vec()],
        ));
        let fixture = harness.fixture.clone();
        let error = harness.pull(&fixture).await.unwrap_err();
        assert!(format!("{error:#}").contains("tar"), "{error:#}");
        assert!(
            !harness.staging.join("demo-1.0.tar").exists(),
            "magic 校验失败不应留下产物"
        );
    }

    #[tokio::test]
    async fn accepts_valid_tar_magic() {
        let harness = Harness::new(fixture(
            ImageKind::LxcRootfs,
            ImageFormat::Tar,
            vec![tar_bytes(b"rootfs")],
        ));
        let fixture = harness.fixture.clone();
        let pulled = harness.pull(&fixture).await.unwrap();
        assert!(pulled.artifact_path.exists());
    }

    #[tokio::test]
    async fn rejects_corrupt_qcow2_magic() {
        let harness = Harness::new(fixture(
            ImageKind::VmDisk,
            ImageFormat::Qcow2,
            vec![b"not-qcow2".to_vec()],
        ));
        let fixture = harness.fixture.clone();
        let error = harness.pull(&fixture).await.unwrap_err();
        assert!(format!("{error:#}").contains("qcow2"), "{error:#}");
    }

    #[tokio::test]
    async fn accepts_valid_qcow2_magic() {
        let harness = Harness::new(fixture(
            ImageKind::VmDisk,
            ImageFormat::Qcow2,
            vec![qcow2_bytes(b"disk")],
        ));
        let fixture = harness.fixture.clone();
        let pulled = harness.pull(&fixture).await.unwrap();
        assert_eq!(
            tokio::fs::read(&pulled.artifact_path).await.unwrap()[..4],
            [0x51, 0x46, 0x49, 0xfb]
        );
    }

    #[tokio::test]
    async fn rejects_payload_manifest_mismatch() {
        let harness = Harness::new(raw_fixture());
        let fixture = harness.fixture.clone();
        let mut request = request_for(&fixture, harness.staging.clone());
        request.total_size += 1;
        let error = harness.puller.pull(&request, &|_| {}).await.unwrap_err();
        assert!(format!("{error:#}").contains("total_size"), "{error:#}");
    }

    #[tokio::test]
    async fn rejects_bad_digest_format() {
        let harness = Harness::new(raw_fixture());
        let fixture = harness.fixture.clone();
        let mut request = request_for(&fixture, harness.staging.clone());
        request.manifest_digest = "not-a-digest".into();
        let error = harness.puller.pull(&request, &|_| {}).await.unwrap_err();
        assert!(
            format!("{error:#}").contains("manifest_digest"),
            "{error:#}"
        );
    }

    #[test]
    fn progress_helpers_cover_ranges() {
        assert_eq!(download_progress(0, 100), 0);
        assert_eq!(download_progress(50, 100), 40);
        assert_eq!(download_progress(100, 100), 80);
        assert_eq!(download_progress(200, 100), 80);
        assert_eq!(verify_progress(0, 100), 80);
        assert_eq!(verify_progress(50, 100), 85);
        assert_eq!(verify_progress(100, 100), 90);
    }

    #[test]
    fn artifact_filename_sanitizes_components() {
        let mut manifest = raw_fixture().manifest;
        manifest.name = "demo image".into();
        manifest.version = "1.0/beta".into();
        assert_eq!(artifact_filename(&manifest), "demo_image-1.0_beta.raw");
        manifest.name = "  ".into();
        manifest.version = "v1".into();
        assert_eq!(artifact_filename(&manifest), "image-v1.raw");
    }

    #[test]
    fn signature_decoding_accepts_raw_base64_and_hex() {
        let raw = vec![7u8; 64];
        assert_eq!(decode_signature(&raw).unwrap(), raw);
        assert_eq!(
            decode_signature(STANDARD.encode(&raw).as_bytes()).unwrap(),
            raw
        );
        assert_eq!(decode_signature(hex::encode(&raw).as_bytes()).unwrap(), raw);
        assert!(decode_signature(b"short").is_err());
    }
}
