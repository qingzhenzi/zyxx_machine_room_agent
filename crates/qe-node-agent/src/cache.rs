//! 内容寻址对象缓存：`CACHE_DIR/objects/{sha256}`，按 LRU 淘汰到 `CACHE_MAX_BYTES`。
//!
//! 设计要点：
//! - 文件名就是内容摘要，**写入前**必须校验 sha256 与 size，避免把损坏数据写进缓存；
//! - 命中时只做 size 校验（`get`）；需要强校验时用 [`ObjectCache::get_verified`]
//!   （重算 sha256，发现篡改会删除条目并报错）；
//! - LRU 依据文件访问/修改时间：命中时刷新 atime（`FileTimes`，不依赖额外 crate）；
//! - 淘汰可传 `keep` 集合保留当前版本，契约要求"保留当前版本"。
//!
//! 注意：`put` 不会自动淘汰——一次 pull 的多个分片必须全部写入后才能组装，
//! 若写入即淘汰会把刚下好的前序分片删掉。淘汰由 [`ObjectCache::evict_to_limit`]
//! 在合适时机（pull 开始前/组装完成后）显式触发。

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::manifest::{is_sha256_hex, sha256_hex};

/// 临时文件序号，避免并发写入同名临时文件。
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);
/// 流式计算/拷贝的缓冲区大小。
const BUFFER_SIZE: usize = 1024 * 1024;
/// 临时文件前缀；淘汰时跳过，避免误删正在写入的对象。
const TEMP_PREFIX: &str = ".tmp-";

/// 内容寻址缓存。
#[derive(Debug, Clone)]
pub struct ObjectCache {
    objects_dir: PathBuf,
    max_bytes: u64,
}

impl ObjectCache {
    /// `cache_dir/objects` 为对象目录；`max_bytes == 0` 表示不限制。
    pub fn new(cache_dir: impl Into<PathBuf>, max_bytes: u64) -> Self {
        Self {
            objects_dir: cache_dir.into().join("objects"),
            max_bytes,
        }
    }

    pub fn objects_dir(&self) -> &Path {
        &self.objects_dir
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// 对象路径（校验摘要格式，防止路径穿越）。
    pub fn path_for(&self, sha256: &str) -> Result<PathBuf> {
        if !is_sha256_hex(sha256) {
            bail!("缓存键必须是 64 位小写 sha256 hex，实际 `{sha256}`");
        }
        Ok(self.objects_dir.join(sha256))
    }

    /// 确保对象目录存在。
    pub async fn ensure_dir(&self) -> Result<()> {
        fs::create_dir_all(&self.objects_dir)
            .await
            .with_context(|| format!("创建缓存目录失败: {}", self.objects_dir.display()))
    }

    /// 写入内存中的对象内容。校验通过后原子落盘（先写临时文件再 rename）。
    pub async fn put_bytes(
        &self,
        sha256: &str,
        expected_size: Option<i64>,
        data: &[u8],
    ) -> Result<PathBuf> {
        let path = self.path_for(sha256)?;
        if let Some(expected) = expected_size {
            let expected = usize::try_from(expected).context("期望 size 必须 >= 0")?;
            if data.len() != expected {
                bail!(
                    "缓存写入拒绝：分片 size 不符（期望 {expected}，实际 {}）",
                    data.len()
                );
            }
        }
        let actual = sha256_hex(data);
        if actual != sha256 {
            bail!("缓存写入拒绝：内容 sha256 不符（期望 {sha256}，实际 {actual}）");
        }

        if let Some(existing) = self.existing_matching(&path, sha256).await? {
            self.touch(&existing)?;
            return Ok(existing);
        }

        self.ensure_dir().await?;
        let temp = self.temp_path(sha256);
        fs::write(&temp, data)
            .await
            .with_context(|| format!("写缓存临时文件失败: {}", temp.display()))?;
        if let Err(error) = fs::rename(&temp, &path).await {
            let _ = fs::remove_file(&temp).await;
            return Err(error).with_context(|| format!("缓存原子落盘失败: {}", path.display()));
        }
        self.touch(&path)?;
        Ok(path)
    }

    /// 把一个已存在的文件收进缓存（流式校验，不整文件读入内存）。
    ///
    /// 源文件保持不动；缓存里是独立副本（rename 只发生在缓存目录内部）。
    pub async fn commit_file(
        &self,
        sha256: &str,
        expected_size: Option<i64>,
        source: &Path,
    ) -> Result<PathBuf> {
        let path = self.path_for(sha256)?;
        let metadata = fs::metadata(source)
            .await
            .with_context(|| format!("读取待入缓存文件失败: {}", source.display()))?;
        if !metadata.is_file() {
            bail!("待入缓存路径不是文件: {}", source.display());
        }
        if let Some(expected) = expected_size
            && metadata.len() != u64::try_from(expected).context("期望 size 必须 >= 0")?
        {
            bail!(
                "缓存写入拒绝：文件 size 不符（期望 {expected}，实际 {}）",
                metadata.len()
            );
        }

        self.ensure_dir().await?;
        let temp = self.temp_path(sha256);
        let mut reader = fs::File::open(source)
            .await
            .with_context(|| format!("打开待入缓存文件失败: {}", source.display()))?;
        let mut writer = fs::File::create(&temp)
            .await
            .with_context(|| format!("创建缓存临时文件失败: {}", temp.display()))?;
        let mut hasher = Sha256::new();
        let mut total: u64 = 0;
        let mut buffer = vec![0u8; BUFFER_SIZE];
        loop {
            let read = reader
                .read(&mut buffer)
                .await
                .with_context(|| format!("读取失败: {}", source.display()))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            writer
                .write_all(&buffer[..read])
                .await
                .with_context(|| format!("写缓存临时文件失败: {}", temp.display()))?;
            total += read as u64;
        }
        writer
            .flush()
            .await
            .with_context(|| format!("刷缓存临时文件失败: {}", temp.display()))?;
        drop(writer);
        drop(reader);

        let actual = hex::encode(hasher.finalize());
        if actual != sha256 {
            let _ = fs::remove_file(&temp).await;
            bail!("缓存写入拒绝：内容 sha256 不符（期望 {sha256}，实际 {actual}）");
        }
        if let Some(expected) = expected_size
            && total != u64::try_from(expected).context("期望 size 必须 >= 0")?
        {
            let _ = fs::remove_file(&temp).await;
            bail!("缓存写入拒绝：拷贝字节数不符（期望 {expected}，实际 {total}）");
        }
        if let Err(error) = fs::rename(&temp, &path).await {
            let _ = fs::remove_file(&temp).await;
            return Err(error).with_context(|| format!("缓存原子落盘失败: {}", path.display()));
        }
        self.touch(&path)?;
        Ok(path)
    }

    /// 普通命中：校验存在性与 size（若给出），并刷新访问时间。
    ///
    /// size 不符视为缓存损坏：删除条目并返回 `None`（调用方会重新下载）。
    pub async fn get(&self, sha256: &str, expected_size: Option<i64>) -> Result<Option<PathBuf>> {
        let path = self.path_for(sha256)?;
        let metadata = match fs::metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("读取缓存元数据失败: {}", path.display()));
            }
        };
        if !metadata.is_file() {
            return Ok(None);
        }
        if let Some(expected) = expected_size
            && metadata.len() != u64::try_from(expected).context("期望 size 必须 >= 0")?
        {
            tracing::warn!(
                path = %path.display(),
                actual = metadata.len(),
                expected,
                "缓存对象 size 不符，删除后重新下载"
            );
            let _ = fs::remove_file(&path).await;
            return Ok(None);
        }
        self.touch(&path)?;
        Ok(Some(path))
    }

    /// 强命中：重算 sha256，发现内容被篡改则删除条目并报错。
    pub async fn get_verified(&self, sha256: &str) -> Result<Option<PathBuf>> {
        let Some(path) = self.get(sha256, None).await? else {
            return Ok(None);
        };
        let actual = hash_file(&path).await?;
        if actual != sha256 {
            let _ = fs::remove_file(&path).await;
            bail!(
                "缓存对象被篡改（期望 {sha256}，实际 {actual}），已删除: {}",
                path.display()
            );
        }
        self.touch(&path)?;
        Ok(Some(path))
    }

    /// 缓存中所有对象的总字节数（不含临时文件）。
    pub async fn total_bytes(&self) -> Result<u64> {
        let mut total = 0u64;
        let mut entries = match fs::read_dir(&self.objects_dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error).context("读取缓存目录失败"),
        };
        while let Some(entry) = entries.next_entry().await.context("遍历缓存目录失败")? {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(TEMP_PREFIX) {
                continue;
            }
            let Ok(metadata) = entry.metadata().await else {
                continue;
            };
            if metadata.is_file() {
                total += metadata.len();
            }
        }
        Ok(total)
    }

    /// 按最近使用时间淘汰，直到总量不超过上限；`keep` 中的对象永不淘汰。
    ///
    /// 返回释放的字节数；`max_bytes == 0` 视为不限制，直接返回 0。
    pub async fn evict_to_limit(&self, keep: &HashSet<String>) -> Result<u64> {
        if self.max_bytes == 0 {
            return Ok(0);
        }
        let mut entries = match fs::read_dir(&self.objects_dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error).context("读取缓存目录失败"),
        };

        struct Entry {
            name: String,
            path: PathBuf,
            len: u64,
            last_used: SystemTime,
        }

        let mut total: u64 = 0;
        let mut candidates: Vec<Entry> = Vec::new();
        while let Some(entry) = entries.next_entry().await.context("遍历缓存目录失败")? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(TEMP_PREFIX) {
                continue;
            }
            let Ok(metadata) = entry.metadata().await else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let len = metadata.len();
            total += len;
            if !keep.contains(&name) {
                candidates.push(Entry {
                    name,
                    path: entry.path(),
                    len,
                    last_used: last_used(&metadata),
                });
            }
        }

        if total <= self.max_bytes {
            return Ok(0);
        }

        candidates.sort_by(|left, right| {
            left.last_used
                .cmp(&right.last_used)
                .then_with(|| left.name.cmp(&right.name))
        });

        let mut freed = 0u64;
        for entry in candidates {
            if total <= self.max_bytes {
                break;
            }
            match fs::remove_file(&entry.path).await {
                Ok(()) => {
                    total -= entry.len;
                    freed += entry.len;
                    tracing::debug!(
                        object = %entry.name,
                        bytes = entry.len,
                        "缓存 LRU 淘汰"
                    );
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    total = total.saturating_sub(entry.len);
                    freed += entry.len;
                }
                Err(error) => {
                    tracing::warn!(
                        path = %entry.path.display(),
                        error = %error,
                        "缓存淘汰失败，跳过该对象"
                    );
                }
            }
        }
        Ok(freed)
    }

    /// 刷新对象访问时间（LRU 的"最近使用"依据）。
    pub fn touch(&self, path: &Path) -> io::Result<()> {
        let file = std::fs::File::open(path)?;
        file.set_times(
            std::fs::FileTimes::new()
                .set_accessed(SystemTime::now())
                .set_modified(SystemTime::now()),
        )
    }

    fn temp_path(&self, sha256: &str) -> PathBuf {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        self.objects_dir.join(format!(
            "{TEMP_PREFIX}{sha256}.{}-{seq}",
            std::process::id()
        ))
    }

    /// 已有同摘要对象时直接复用（先校验内容，避免"文件名对、内容错"）。
    async fn existing_matching(&self, path: &Path, sha256: &str) -> Result<Option<PathBuf>> {
        let metadata = match fs::metadata(path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("读取缓存元数据失败: {}", path.display()));
            }
        };
        if !metadata.is_file() {
            return Ok(None);
        }
        match hash_file(path).await {
            Ok(actual) if actual == sha256 => Ok(Some(path.to_path_buf())),
            Ok(_) => {
                tracing::warn!(path = %path.display(), "缓存对象内容不符，覆盖重写");
                Ok(None)
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), error = %error, "缓存对象不可读，覆盖重写");
                Ok(None)
            }
        }
    }
}

/// 最近使用时间：取 atime 与 mtime 的较新者。
///
/// 部分文件系统不维护 atime（或挂载 noatime 时返回纪元时间），fallback 到 mtime。
fn last_used(metadata: &std::fs::Metadata) -> SystemTime {
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    metadata
        .accessed()
        .map(|accessed| accessed.max(modified))
        .unwrap_or(modified)
}

async fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)
        .await
        .with_context(|| format!("打开缓存对象失败: {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; BUFFER_SIZE];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .with_context(|| format!("读取缓存对象失败: {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_cache(max_bytes: u64) -> (tempfile::TempDir, ObjectCache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = ObjectCache::new(dir.path(), max_bytes);
        (dir, cache)
    }

    fn sha(data: &[u8]) -> String {
        sha256_hex(data)
    }

    /// 直接设置对象的 atime/mtime，让 LRU 测试不依赖 sleep（避免 CI 抖动）。
    fn set_last_used(path: &Path, seconds_ago: u64) {
        let time = SystemTime::now() - std::time::Duration::from_secs(seconds_ago);
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_accessed(time)
                .set_modified(time),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn put_then_hit_and_miss() {
        let (_dir, cache) = new_cache(1024);
        let data = b"chunk-bytes";
        let key = sha(data);

        let path = cache
            .put_bytes(&key, Some(data.len() as i64), data)
            .await
            .unwrap();
        assert_eq!(path, cache.objects_dir().join(&key));
        assert_eq!(tokio::fs::read(&path).await.unwrap(), data);

        // 命中
        let hit = cache.get(&key, Some(data.len() as i64)).await.unwrap();
        assert_eq!(hit.as_deref(), Some(path.as_path()));
        // 强校验命中
        assert!(cache.get_verified(&key).await.unwrap().is_some());

        // 未命中
        let missing = sha(b"missing");
        assert!(cache.get(&missing, None).await.unwrap().is_none());
        assert!(cache.get_verified(&missing).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn put_rejects_digest_and_size_mismatch() {
        let (_dir, cache) = new_cache(1024);
        let error = cache
            .put_bytes(&sha(b"expected"), None, b"actual")
            .await
            .expect_err("sha 不符必须拒绝");
        assert!(error.to_string().contains("sha256"), "{error}");

        let data = b"payload";
        let error = cache
            .put_bytes(&sha(data), Some(999), data)
            .await
            .expect_err("size 不符必须拒绝");
        assert!(error.to_string().contains("size"), "{error}");

        // 被拒绝的写入不应留下文件
        assert_eq!(cache.total_bytes().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn tampered_cache_is_detected_and_removed() {
        let (_dir, cache) = new_cache(1024);
        let data = b"good-data";
        let key = sha(data);
        let path = cache
            .put_bytes(&key, Some(data.len() as i64), data)
            .await
            .unwrap();

        // 内容被替换但长度相同：size 校验发现不了，强校验必须发现
        tokio::fs::write(&path, b"evil-data").await.unwrap();
        let error = cache.get_verified(&key).await.expect_err("篡改必须被发现");
        assert!(error.to_string().contains("篡改"), "{error}");
        assert!(!path.exists(), "损坏对象应被删除");

        // 长度不同：普通 get 直接拒绝并删除
        let path = cache
            .put_bytes(&key, Some(data.len() as i64), data)
            .await
            .unwrap();
        tokio::fs::write(&path, b"much-longer-content")
            .await
            .unwrap();
        assert!(
            cache
                .get(&key, Some(data.len() as i64))
                .await
                .unwrap()
                .is_none()
        );
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn commit_file_verifies_hash_and_copies() {
        let (dir, cache) = new_cache(1024);
        let source = dir.path().join("chunk.part");
        let data = b"downloaded-to-disk";
        tokio::fs::write(&source, data).await.unwrap();

        let key = sha(data);
        let path = cache
            .commit_file(&key, Some(data.len() as i64), &source)
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), data);
        assert!(source.exists(), "源文件不应被移动/删除");

        let wrong = sha(b"other");
        let error = cache
            .commit_file(&wrong, None, &source)
            .await
            .expect_err("sha 不符必须拒绝");
        assert!(error.to_string().contains("sha256"), "{error}");
    }

    #[tokio::test]
    async fn lru_evicts_oldest_and_keeps_recently_used() {
        let (_dir, cache) = new_cache(25);
        let a = b"aaaaaaaaaa";
        let b = b"bbbbbbbbbb";
        let c = b"cccccccccc";
        let (ka, kb, kc) = (sha(a), sha(b), sha(c));

        let pa = cache.put_bytes(&ka, Some(10), a).await.unwrap();
        let pb = cache.put_bytes(&kb, Some(10), b).await.unwrap();
        let pc = cache.put_bytes(&kc, Some(10), c).await.unwrap();

        // 最近使用顺序：b（最旧） < c < a（刚被命中）
        set_last_used(&pb, 300);
        set_last_used(&pc, 200);
        set_last_used(&pa, 100);

        assert_eq!(cache.total_bytes().await.unwrap(), 30);
        let freed = cache.evict_to_limit(&HashSet::new()).await.unwrap();
        assert_eq!(freed, 10, "只需淘汰一个对象即可降到 20 <= 25");
        assert!(pa.exists(), "刚使用过的对象保留");
        assert!(pc.exists(), "较新的对象保留");
        assert!(!pb.exists(), "最久未使用的对象被淘汰");
        assert_eq!(cache.total_bytes().await.unwrap(), 20);
    }

    #[tokio::test]
    async fn keep_set_protects_current_version() {
        let (_dir, cache) = new_cache(10);
        let a = b"aaaaaaaaaa";
        let b = b"bbbbbbbbbb";
        let (ka, kb) = (sha(a), sha(b));
        let pa = cache.put_bytes(&ka, Some(10), a).await.unwrap();
        let pb = cache.put_bytes(&kb, Some(10), b).await.unwrap();
        set_last_used(&pa, 300); // a 更旧，但属于当前版本

        let keep: HashSet<String> = [ka.clone()].into_iter().collect();
        let freed = cache.evict_to_limit(&keep).await.unwrap();
        assert_eq!(freed, 10);
        assert!(pa.exists(), "当前版本必须保留");
        assert!(!pb.exists());
    }

    #[tokio::test]
    async fn eviction_is_noop_below_limit_and_unlimited() {
        let (_dir, cache) = new_cache(1024);
        let data = b"small";
        let path = cache
            .put_bytes(&sha(data), Some(data.len() as i64), data)
            .await
            .unwrap();
        assert_eq!(cache.evict_to_limit(&HashSet::new()).await.unwrap(), 0);
        assert!(path.exists());

        let (_dir, unlimited) = new_cache(0);
        let data = b"whatever";
        let path = unlimited
            .put_bytes(&sha(data), Some(data.len() as i64), data)
            .await
            .unwrap();
        assert_eq!(unlimited.evict_to_limit(&HashSet::new()).await.unwrap(), 0);
        assert!(path.exists());
    }

    #[tokio::test]
    async fn path_for_rejects_non_digest_keys() {
        let (_dir, cache) = new_cache(1024);
        assert!(cache.path_for("../etc/passwd").is_err());
        assert!(cache.path_for("ABC").is_err());
        assert!(cache.path_for(&sha(b"x")).is_ok());
    }
}
