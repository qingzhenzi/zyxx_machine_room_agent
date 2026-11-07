//! P2P 镜像分发（agent 侧）：产物分享/拉取、manifest 分片校验、seed 登记抽象。
//!
//! 设计要点（契约见 `docs/p2p-image-distribution.md`）：
//!
//! - **manifest 是唯一校验根**：`Manifest` 没有产物级 sha256，只有 `total_size` +
//!   `chunks[]`（offset/size/sha256）。P2P 下载完成后必须按 chunk 切分逐片重算
//!   sha256，并校验总大小，全部通过才接受；不匹配要删除产物并回退 HTTP。
//! - **ticket 即能力**：ticket 由 `qep2p::Node::share_path` 生成，包含节点地址与
//!   blob 哈希；拿到它的内网设备可以直接下载。
//! - **seed 登记是可选的**：`QE_P2P_ENABLED=1` 才启用；未启动 P2P 节点或 peer
//!   查询失败一律回退现有 HTTP 分片下载，"回退必须无感"。
//! - 抽象 [`BlobExchange`] / [`SeedRegistry`] 便于单测注入 mock，不依赖真实网络。

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use qe_device_pki_client::transport::TransportConfig;
use sha2::{Digest, Sha256};

use crate::BoxFuture;
use crate::manifest::Manifest;

/// P2P 开关环境变量。
pub const P2P_ENABLED_ENV: &str = "QE_P2P_ENABLED";
/// 单个 peer 下载超时（契约：每个 ticket 60s）。
pub const PEER_FETCH_TIMEOUT: Duration = Duration::from_secs(60);
/// 单次 pull 最多尝试的 peer 数（服务端 peers 接口最多 20 条）。
pub const MAX_PEER_ATTEMPTS: usize = 5;

/// 一条 peer seed（与 `qe_device_pki_client::p2p::PeerSeed` 对齐）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSeed {
    pub serial: String,
    pub blob_ticket: String,
    pub size_bytes: u64,
    pub updated_at: String,
}

/// seed 登记/查询抽象；生产实现走 device-pki mTLS，测试可 mock。
pub trait SeedRegistry: Send + Sync {
    fn peers<'a>(&'a self, manifest_digest: &'a str) -> BoxFuture<'a, Result<Vec<PeerSeed>>>;
    fn seed<'a>(
        &'a self,
        manifest_digest: &'a str,
        blob_ticket: &'a str,
        size_bytes: u64,
    ) -> BoxFuture<'a, Result<()>>;
}

/// 生产实现：`qe_device_pki_client::p2p`（设备证书 mTLS）。
pub struct MtlsSeedRegistry {
    transport: TransportConfig,
}

impl MtlsSeedRegistry {
    pub fn new(transport: TransportConfig) -> Self {
        Self { transport }
    }
}

impl SeedRegistry for MtlsSeedRegistry {
    fn peers<'a>(&'a self, manifest_digest: &'a str) -> BoxFuture<'a, Result<Vec<PeerSeed>>> {
        Box::pin(async move {
            let peers =
                qe_device_pki_client::p2p::fetch_peers(&self.transport, manifest_digest).await?;
            Ok(peers
                .into_iter()
                .map(|peer| PeerSeed {
                    serial: peer.serial,
                    blob_ticket: peer.blob_ticket,
                    size_bytes: peer.size_bytes,
                    updated_at: peer.updated_at,
                })
                .collect())
        })
    }

    fn seed<'a>(
        &'a self,
        manifest_digest: &'a str,
        blob_ticket: &'a str,
        size_bytes: u64,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let reply = qe_device_pki_client::p2p::post_seed(
                &self.transport,
                manifest_digest,
                blob_ticket,
                size_bytes,
            )
            .await?;
            if !reply.ok {
                bail!(
                    "device-pki 未确认 seed 登记（expires_at={}）",
                    reply.expires_at
                );
            }
            Ok(())
        })
    }
}

/// blob 分享/下载抽象；生产实现是 [`qep2p::Node`]，测试可 mock。
pub trait BlobExchange: Send + Sync {
    fn share<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<(String, u64)>>;
    fn fetch<'a>(
        &'a self,
        ticket: &'a str,
        dest_dir: &'a Path,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<PathBuf>>;
}

impl BlobExchange for qep2p::Node {
    fn share<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<(String, u64)>> {
        Box::pin(share_artifact(self, path))
    }

    fn fetch<'a>(
        &'a self,
        ticket: &'a str,
        dest_dir: &'a Path,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<PathBuf>> {
        Box::pin(fetch_artifact(self, ticket, dest_dir, timeout))
    }
}

/// 一次启用中的 P2P 分发上下文（node 句柄 + seed 登记客户端）。
#[derive(Clone)]
pub struct P2pDistribution {
    blobs: Arc<dyn BlobExchange>,
    seeds: Arc<dyn SeedRegistry>,
}

impl P2pDistribution {
    pub fn new(blobs: Arc<dyn BlobExchange>, seeds: Arc<dyn SeedRegistry>) -> Self {
        Self { blobs, seeds }
    }

    /// 查询该 digest 的其它设备 seed。
    pub async fn peers(&self, manifest_digest: &str) -> Result<Vec<PeerSeed>> {
        self.seeds.peers(manifest_digest).await
    }

    /// 分享一个已组装产物，返回 `(ticket, size)`。
    pub async fn share(&self, path: &Path) -> Result<(String, u64)> {
        self.blobs.share(path).await
    }

    /// 从 ticket 下载产物到 `dest_dir`（含超时）。
    pub async fn fetch(&self, ticket: &str, dest_dir: &Path, timeout: Duration) -> Result<PathBuf> {
        self.blobs.fetch(ticket, dest_dir, timeout).await
    }

    /// 登记 seed（best-effort，调用方只记日志）。
    pub async fn seed(
        &self,
        manifest_digest: &str,
        blob_ticket: &str,
        size_bytes: u64,
    ) -> Result<()> {
        self.seeds
            .seed(manifest_digest, blob_ticket, size_bytes)
            .await
    }
}

/// 解析 `QE_P2P_ENABLED` 的值（默认关；1/true/yes/on/y 开）。
pub fn parse_switch(raw: Option<&str>) -> bool {
    matches!(
        raw.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("1" | "true" | "yes" | "on" | "y")
    )
}

/// 从环境变量读取 P2P 开关（每次调用重新读取，便于运行期调试）。
pub fn p2p_enabled_from_env() -> bool {
    let raw = std::env::var(P2P_ENABLED_ENV).ok();
    parse_switch(raw.as_deref())
}

/// 把一个普通文件加入 qep2p blob 库并返回 `(ticket, 文件大小)`。
pub async fn share_artifact(node: &qep2p::Node, path: &Path) -> Result<(String, u64)> {
    let shared = node
        .share_path(path)
        .await
        .with_context(|| format!("qep2p 分享产物失败: {}", path.display()))?;
    Ok((shared.ticket.to_string(), shared.size))
}

/// 从 ticket 下载到 `dest_dir`，返回本地路径（超时视为失败）。
pub async fn fetch_artifact(
    node: &qep2p::Node,
    ticket: &str,
    dest_dir: &Path,
    timeout: Duration,
) -> Result<PathBuf> {
    let parsed = qep2p::parse_ticket(ticket)?;
    let outcome = tokio::time::timeout(timeout, node.download(&parsed, dest_dir))
        .await
        .map_err(|_| anyhow::anyhow!("P2P 下载超时（{}s）", timeout.as_secs()))?
        .with_context(|| format!("P2P 下载失败（ticket={}）", short_ticket(ticket)))?;
    if !outcome.path.is_file() {
        bail!("P2P 下载完成但产物不存在: {}", outcome.path.display());
    }
    if outcome.bytes == 0 {
        bail!("P2P 下载产物为空: {}", outcome.path.display());
    }
    Ok(outcome.path)
}

fn short_ticket(ticket: &str) -> String {
    let trimmed = ticket.trim();
    if trimmed.len() <= 24 {
        trimmed.to_owned()
    } else {
        format!("{}…", &trimmed[..24])
    }
}

/// 按 manifest 校验 P2P 下载的产物。
///
/// 校验项（全部通过才接受）：
/// 1. 文件大小 == `manifest.total_size`；
/// 2. 分片 offset 从 0 连续覆盖整个文件；
/// 3. 逐片重算 sha256 == `chunk.sha256`。
pub fn verify_artifact(path: &Path, manifest: &Manifest) -> Result<()> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("读取 P2P 产物元数据失败: {}", path.display()))?;
    if !metadata.is_file() {
        bail!("P2P 产物不是普通文件: {}", path.display());
    }
    let total_size = u64::try_from(manifest.total_size).context("manifest.total_size 非法")?;
    if metadata.len() != total_size {
        bail!(
            "P2P 产物大小不符：期望 {} 字节，实际 {}",
            total_size,
            metadata.len()
        );
    }

    let mut file = std::fs::File::open(path)
        .with_context(|| format!("打开 P2P 产物失败: {}", path.display()))?;
    let mut covered: u64 = 0;
    for chunk in &manifest.chunks {
        let offset = u64::try_from(chunk.offset).context("chunk.offset 非法")?;
        let size = u64::try_from(chunk.size).context("chunk.size 非法")?;
        if offset != covered {
            bail!(
                "manifest 分片 offset 不连续：分片 {} 期望 {covered}，实际 {offset}",
                chunk.index
            );
        }
        file.seek(SeekFrom::Start(offset))
            .with_context(|| format!("seek 分片 {} 失败", chunk.index))?;
        let actual = sha256_reader(&mut file, size)
            .with_context(|| format!("读取分片 {} 失败", chunk.index))?;
        if actual != chunk.sha256 {
            bail!(
                "P2P 产物分片 {} sha256 不符：期望 {}，实际 {actual}",
                chunk.index,
                chunk.sha256
            );
        }
        covered = covered.checked_add(size).context("分片大小累计溢出")?;
    }
    if covered != total_size {
        bail!("manifest 分片未覆盖完整产物：分片合计 {covered}，total_size {total_size}");
    }
    Ok(())
}

/// 从 reader 读取恰好 `size` 字节并返回 sha256 小写 hex。
fn sha256_reader(reader: &mut impl Read, size: u64) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut remaining = size;
    let mut buffer = vec![0u8; 64 * 1024];
    while remaining > 0 {
        let want = remaining.min(buffer.len() as u64) as usize;
        let read = reader.read(&mut buffer[..want])?;
        if read == 0 {
            bail!("读取到文件末尾，仍缺 {remaining} 字节");
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{
        Compression, ImageFormat, ImageKind, MANIFEST_SCHEMA, ManifestChunk, ManifestRuntime,
        sha256_hex,
    };
    use std::io::Write;
    use std::time::Duration;

    fn test_manifest(data: &[u8], chunk_sizes: &[usize]) -> Manifest {
        let mut chunks = Vec::new();
        let mut offset = 0i64;
        for (index, size) in chunk_sizes.iter().copied().enumerate() {
            let start = usize::try_from(offset).unwrap();
            let part = &data[start..start + size];
            chunks.push(ManifestChunk {
                index: i32::try_from(index).unwrap(),
                object_key: format!("objects/{}", sha256_hex(part)),
                sha256: sha256_hex(part),
                size: i64::try_from(size).unwrap(),
                offset,
            });
            offset += i64::try_from(size).unwrap();
        }
        Manifest {
            schema: MANIFEST_SCHEMA,
            kind: ImageKind::VmDisk,
            name: "p2p-test".into(),
            version: "1.0".into(),
            platform: "linux/amd64".into(),
            format: ImageFormat::Raw,
            compression: Compression::None,
            total_size: i64::try_from(data.len()).unwrap(),
            chunks,
            runtime: ManifestRuntime::default(),
            annotations: serde_json::json!({}),
            created_at: chrono::Utc::now(),
            created_by: None,
        }
    }

    fn write_file(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(data).unwrap();
        path
    }

    fn sample_data() -> Vec<u8> {
        (0..1024 * 1024u64)
            .map(|i| u8::try_from((i.wrapping_mul(31).wrapping_add(7)) % 251).unwrap())
            .collect()
    }

    #[test]
    fn switch_parsing() {
        for enabled in ["1", "true", "TRUE", " yes ", "on", "y"] {
            assert!(parse_switch(Some(enabled)), "{enabled} 应为开");
        }
        for disabled in [None, Some("0"), Some("false"), Some("off"), Some("")] {
            assert!(!parse_switch(disabled), "{disabled:?} 应为关");
        }
    }

    #[test]
    fn manifest_chunk_verification_passes_for_2_and_3_chunks() {
        let data = sample_data();
        for sizes in [
            vec![512 * 1024, 512 * 1024],
            vec![300_000, 400_000, 348_576],
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = write_file(dir.path(), "artifact.raw", &data);
            let manifest = test_manifest(&data, &sizes);
            manifest.validate().unwrap();
            verify_artifact(&path, &manifest).unwrap();
        }
    }

    #[test]
    fn manifest_chunk_verification_detects_tampering() {
        let data = sample_data();
        let sizes = vec![300_000, 400_000, 348_576];
        let manifest = test_manifest(&data, &sizes);
        let dir = tempfile::tempdir().unwrap();

        // 篡改第一个 chunk 的 1 个字节 → sha256 不符
        let mut tampered = data.clone();
        tampered[0] ^= 0xff;
        let path = write_file(dir.path(), "tampered.raw", &tampered);
        let error = verify_artifact(&path, &manifest).unwrap_err();
        assert!(error.to_string().contains("sha256"), "{error}");

        // 截断文件 → 大小不符
        let truncated_path = write_file(dir.path(), "truncated.raw", &data[..data.len() - 1]);
        let error = verify_artifact(&truncated_path, &manifest).unwrap_err();
        assert!(error.to_string().contains("大小"), "{error}");
    }

    #[test]
    fn manifest_verification_rejects_extra_bytes() {
        let data = sample_data();
        let manifest = test_manifest(&data, &[512 * 1024, 512 * 1024]);
        let dir = tempfile::tempdir().unwrap();
        let mut extra = data.clone();
        extra.push(0);
        let path = write_file(dir.path(), "extra.raw", &extra);
        let error = verify_artifact(&path, &manifest).unwrap_err();
        assert!(error.to_string().contains("大小"), "{error}");
    }

    /// 真跑 P2P：两个本地 qep2p Node（独立 state 目录）share → fetch 1 MiB，
    /// 再按 3 个 chunk 的 manifest 逐片校验 sha256。
    ///
    /// 若本机网络环境导致 iroh endpoint 无法启动（例如无可用 UDP/回环），
    /// 测试会带着精确错误失败，而不是伪装通过。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn p2p_share_fetch_1mib_roundtrip_verifies_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let data = sample_data();
        let src = write_file(dir.path(), "artifact.raw", &data);
        let manifest = test_manifest(&data, &[300_000, 400_000, 348_576]);
        manifest.validate().unwrap();

        let mut cfg_a = qep2p::NodeConfig::new(dir.path().join("p2p-a"));
        cfg_a.mdns = false;
        let node_a = qep2p::Node::start(cfg_a)
            .await
            .unwrap_or_else(|error| panic!("启动 provider P2P 节点失败: {error:#}"));
        let mut cfg_b = qep2p::NodeConfig::new(dir.path().join("p2p-b"));
        cfg_b.mdns = false;
        let node_b = qep2p::Node::start(cfg_b)
            .await
            .unwrap_or_else(|error| panic!("启动 consumer P2P 节点失败: {error:#}"));

        let (ticket, size) = share_artifact(&node_a, &src)
            .await
            .expect("share_artifact 应成功");
        assert_eq!(size, data.len() as u64);
        assert!(!ticket.is_empty());

        let out_dir = dir.path().join("fetched");
        let fetched = fetch_artifact(&node_b, &ticket, &out_dir, Duration::from_secs(120))
            .await
            .expect("fetch_artifact 应成功");
        assert!(fetched.is_file());
        assert_eq!(std::fs::read(&fetched).unwrap(), data);
        verify_artifact(&fetched, &manifest).expect("下载产物应通过 manifest 分片校验");

        let bytes = std::fs::read(&fetched).unwrap();
        assert_eq!(sha256_hex(&bytes), sha256_hex(&data));

        node_a.shutdown().await.ok();
        node_b.shutdown().await.ok();
    }
}
