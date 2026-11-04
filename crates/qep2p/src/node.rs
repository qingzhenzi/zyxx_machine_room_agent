//! 节点生命周期、文件分享/下载/推送，以及控制协议。

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use iroh::endpoint::{Connection, RecvStream, SendStream, VarInt};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, SecretKey, endpoint::presets};
use iroh_blobs::api::blobs::{AddPathOptions, ImportMode};
use iroh_blobs::api::remote::GetProgressItem;
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::ticket::BlobTicket;
use iroh_blobs::{BlobFormat, BlobsProtocol, Hash};
use iroh_mdns_address_lookup::MdnsAddressLookup;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::{CONTROL_ALPN, PROTOCOL_VERSION, hostname, printable, short_id};

const MAX_CONTROL_FRAME: usize = 64 * 1024;
const DEFAULT_MAX_FILE_SIZE: u64 = 1024 * 1024 * 1024;
const KEY_FILE: &str = "node.key";

/// relay 的三种模式：纯内网、n0 公共 relay、自建 relay。
#[derive(Clone, Debug)]
pub enum RelayChoice {
    /// 完全关闭 relay，只走直连 / mDNS。
    Disabled,
    /// 使用 n0 的公共 relay（默认不启用，避免依赖公网）。
    Default,
    /// 自建 relay，例如 `http://10.0.0.5:3340`。
    Custom {
        urls: Vec<String>,
        token: Option<String>,
    },
}

impl RelayChoice {
    fn to_mode(&self) -> Result<RelayMode> {
        match self {
            RelayChoice::Disabled => Ok(RelayMode::Disabled),
            RelayChoice::Default => Ok(RelayMode::Default),
            RelayChoice::Custom { urls, token } => {
                if urls.is_empty() {
                    bail!("自定义 relay 至少需要一个 URL");
                }
                let map = RelayMap::try_from_iter(urls.iter().map(String::as_str))
                    .context("relay URL 非法（需要 http(s)://host:port）")?;
                let map = match token {
                    Some(token) => map.with_auth_token(token.clone()),
                    None => map,
                };
                Ok(RelayMode::Custom(map))
            }
        }
    }
}

/// 节点配置。`state_dir` 里保存身份私钥和 blob 库。
#[derive(Clone, Debug)]
pub struct NodeConfig {
    pub state_dir: PathBuf,
    pub relay: RelayChoice,
    pub mdns: bool,
    pub inbox: Option<PathBuf>,
    pub allow: Vec<EndpointId>,
    pub max_file_size: u64,
    /// 分享时只把 relay 地址写进 ticket（适合双方无法直连的网段）。
    pub relay_only: bool,
}

impl NodeConfig {
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            relay: RelayChoice::Disabled,
            mdns: true,
            inbox: None,
            allow: Vec::new(),
            max_file_size: DEFAULT_MAX_FILE_SIZE,
            relay_only: false,
        }
    }
}

/// 节点事件，CLI 和宿主程序都可以订阅。
#[derive(Clone, Debug)]
pub enum Event {
    Offer {
        from: EndpointId,
        name: String,
        size: u64,
    },
    Progress {
        received: u64,
    },
    Downloaded {
        from: Option<EndpointId>,
        path: PathBuf,
        bytes: u64,
    },
    Sent {
        to: EndpointId,
        name: String,
        bytes: u64,
    },
    Rejected {
        from: EndpointId,
        reason: String,
    },
}

#[derive(Debug)]
pub struct SharedBlob {
    pub ticket: BlobTicket,
    pub size: u64,
}

#[derive(Debug)]
pub struct DownloadOutcome {
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug)]
pub struct SendOutcome {
    pub to: EndpointId,
    pub name: String,
    pub bytes: u64,
    pub remote_path: Option<String>,
}

#[derive(Debug)]
pub struct PingOutcome {
    pub to: EndpointId,
    pub rtts: Vec<Duration>,
}

struct Inner {
    endpoint: Endpoint,
    store: FsStore,
    inbox: Option<PathBuf>,
    allow: Vec<EndpointId>,
    max_file_size: u64,
    events: broadcast::Sender<Event>,
}

/// 一个已启动的 qep2p 节点。
///
/// 内部同时注册了两个 ALPN：
/// - `iroh_blobs::ALPN`：内容寻址的 BLAKE3 校验流；
/// - `qep2p/control/1`：推送 Offer、Ping 等控制消息。
pub struct Node {
    endpoint: Endpoint,
    store: FsStore,
    router: Router,
    inner: Arc<Inner>,
    events: broadcast::Sender<Event>,
    relay_only: bool,
    _lock: std::fs::File,
}

impl Node {
    pub async fn start(config: NodeConfig) -> Result<Self> {
        prepare_state_dir(&config.state_dir)?;
        let lock = lock_state_dir(&config.state_dir)?;
        let key = load_or_create_key(&config.state_dir)?;
        let relay_mode = config.relay.to_mode()?;
        let has_relay = !matches!(config.relay, RelayChoice::Disabled);

        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(key)
            .relay_mode(relay_mode);
        if config.mdns {
            builder = builder.address_lookup(MdnsAddressLookup::builder());
        }
        let endpoint = builder.bind().await.context("启动 iroh endpoint 失败")?;

        if has_relay {
            // relay 可用性不决定启动成败；拿不到就继续，直连仍然工作。
            let _ = tokio::time::timeout(Duration::from_secs(8), endpoint.online()).await;
        }

        let relay_only = config.relay_only;
        let store = FsStore::load(config.state_dir.join("blobs"))
            .await
            .context("打开 blob 库失败")?;
        let (events, _) = broadcast::channel(256);
        let inner = Arc::new(Inner {
            endpoint: endpoint.clone(),
            store: store.clone(),
            inbox: config.inbox,
            allow: config.allow,
            max_file_size: config.max_file_size,
            events: events.clone(),
        });

        let blobs = BlobsProtocol::new(&store, None);
        let router = Router::builder(endpoint.clone())
            .accept(iroh_blobs::ALPN, blobs)
            .accept(
                CONTROL_ALPN,
                ControlHandler {
                    inner: inner.clone(),
                },
            )
            .spawn();

        Ok(Self {
            endpoint,
            store,
            router,
            inner,
            events,
            relay_only,
            _lock: lock,
        })
    }

    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    pub fn addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// 把文件加入 blob 库并返回 ticket。ticket 里包含当前 relay / 直连地址。
    pub async fn share_path(&self, path: &Path) -> Result<SharedBlob> {
        let abs = std::path::absolute(path)
            .with_context(|| format!("解析路径失败：{}", path.display()))?;
        let meta = tokio::fs::metadata(&abs)
            .await
            .with_context(|| format!("读取文件失败：{}", abs.display()))?;
        if !meta.is_file() {
            bail!("只支持单个普通文件：{}", abs.display());
        }

        let tag = self
            .store
            .blobs()
            .add_path_with_opts(AddPathOptions {
                path: abs,
                format: BlobFormat::Raw,
                // 大文件优先硬链接/引用，避免复制一份；分享期间不要改文件。
                mode: ImportMode::TryReference,
            })
            .await
            .context("导入文件到 blob 库失败")?;

        let mut addr = self.endpoint.addr();
        if self.relay_only {
            addr.addrs.retain(|a| a.is_relay());
            if addr.addrs.is_empty() {
                bail!("relay 尚未就绪，无法生成 relay-only ticket");
            }
        }
        Ok(SharedBlob {
            ticket: BlobTicket::new(addr, tag.hash, tag.format),
            size: meta.len(),
        })
    }

    /// 从 ticket 下载到 `out_dir`。本地已有分片会被复用（断点续传）。
    pub async fn download(&self, ticket: &BlobTicket, out_dir: &Path) -> Result<DownloadOutcome> {
        self.download_named(ticket, out_dir, None).await
    }

    /// 同 [`Node::download`]，但可以指定保存的文件名。
    pub async fn download_named(
        &self,
        ticket: &BlobTicket,
        out_dir: &Path,
        name: Option<&str>,
    ) -> Result<DownloadOutcome> {
        fetch_ticket(&self.inner, ticket, out_dir, name).await
    }

    /// 把本地文件推给对端：先 share，再通过控制协议发 Offer，
    /// 对端用自己的 blob 库回连下载。
    pub async fn send_file(
        &self,
        target: EndpointAddr,
        path: &Path,
        timeout: Duration,
    ) -> Result<SendOutcome> {
        let shared = self.share_path(path).await?;
        let name = sanitize_name(
            &path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
        )?;
        let target_id = target.id;
        let request = ControlRequest::Offer {
            v: PROTOCOL_VERSION,
            ticket: shared.ticket.to_string(),
            name: name.clone(),
            size: shared.size,
            host: hostname(),
        };

        let started = Instant::now();
        let fut = async {
            let conn = self
                .endpoint
                .connect(target, CONTROL_ALPN)
                .await
                .context("连接对端控制通道失败")?;
            let (mut send, mut recv) = conn.open_bi().await.context("打开控制流失败")?;
            write_frame(&mut send, &request).await?;
            send.finish().context("关闭控制流失败")?;

            match read_frame::<ControlResponse>(&mut recv).await? {
                ControlResponse::Offer {
                    ok: true,
                    path,
                    bytes,
                    ..
                } => Ok(SendOutcome {
                    to: target_id,
                    name: name.clone(),
                    bytes,
                    remote_path: path,
                }),
                ControlResponse::Offer {
                    ok: false, error, ..
                } => bail!(
                    "对端拒绝接收：{}",
                    error.unwrap_or_else(|| "未知原因".into())
                ),
                other => bail!("意外的控制响应：{other:?}"),
            }
        };

        let outcome = match tokio::time::timeout(timeout, fut).await {
            Ok(result) => result?,
            Err(_) => bail!("发送超时（{} 秒）", timeout.as_secs()),
        };
        let _ = self.events.send(Event::Sent {
            to: target_id,
            name: outcome.name.clone(),
            bytes: outcome.bytes,
        });
        eprintln!(
            "[qep2p] 已推送 {}（{} 字节）给 {}，用时 {:.0} ms",
            outcome.name,
            outcome.bytes,
            short_id(&target_id),
            started.elapsed().as_secs_f64() * 1000.0
        );
        Ok(outcome)
    }

    /// 通过控制通道测 RTT。
    pub async fn ping(
        &self,
        target: EndpointAddr,
        count: u32,
        interval: Duration,
        timeout: Duration,
    ) -> Result<PingOutcome> {
        let target_id = target.id;
        let fut = async {
            let conn = self
                .endpoint
                .connect(target, CONTROL_ALPN)
                .await
                .context("连接失败")?;
            let (mut send, mut recv) = conn.open_bi().await.context("打开控制流失败")?;
            let mut rtts = Vec::with_capacity(count as usize);
            for i in 0..count {
                let nonce = nanos() as u64 ^ i as u64;
                let started = Instant::now();
                write_frame(&mut send, &ControlRequest::Ping { nonce }).await?;
                match read_frame::<ControlResponse>(&mut recv).await? {
                    ControlResponse::Pong { nonce: n } if n == nonce => {}
                    other => bail!("意外的 Ping 响应：{other:?}"),
                }
                rtts.push(started.elapsed());
                if i + 1 < count {
                    tokio::time::sleep(interval).await;
                }
            }
            send.finish().ok();
            Ok::<_, anyhow::Error>(rtts)
        };

        let rtts = match tokio::time::timeout(timeout, fut).await {
            Ok(result) => result?,
            Err(_) => bail!("Ping 超时（{} 秒）", timeout.as_secs()),
        };
        Ok(PingOutcome {
            to: target_id,
            rtts,
        })
    }

    pub async fn shutdown(self) -> Result<()> {
        self.router.shutdown().await.context("关闭 router 失败")?;
        self.endpoint.close().await;
        // FsStore 的 actor 可能已随 router 关闭而退出；数据已经通过 redb 提交，
        // 这里只做尽力清理，不把关闭错误当成失败。
        let _ = self.store.shutdown().await;
        Ok(())
    }
}

/// 只读取/生成身份，不启动网络，供 `qep2p id` 使用。
pub fn load_or_create_identity(state_dir: &Path) -> Result<EndpointId> {
    prepare_state_dir(state_dir)?;
    let key = load_or_create_key(state_dir)?;
    Ok(key.public())
}

pub fn parse_ticket(s: &str) -> Result<BlobTicket> {
    s.trim().parse().context("ticket 解析失败")
}

async fn fetch_ticket(
    inner: &Inner,
    ticket: &BlobTicket,
    out_dir: &Path,
    name: Option<&str>,
) -> Result<DownloadOutcome> {
    let hash_and_format = ticket.hash_and_format();
    let local = inner
        .store
        .remote()
        .local(hash_and_format)
        .await
        .context("检查本地分片失败")?;

    if !local.is_complete() {
        let conn = inner
            .endpoint
            .connect(ticket.addr().clone(), iroh_blobs::ALPN)
            .await
            .with_context(|| format!("连接提供方 {} 失败", short_id(&ticket.addr().id)))?;
        let progress = inner.store.remote().execute_get(conn, local.missing());
        let mut stream = progress.stream();
        let mut done = false;
        while let Some(item) = stream.next().await {
            match item {
                GetProgressItem::Progress(received) => {
                    let _ = inner.events.send(Event::Progress { received });
                }
                GetProgressItem::Done(_) => {
                    done = true;
                    break;
                }
                GetProgressItem::Error(e) => bail!("下载失败：{e:?}"),
            }
        }
        if !done {
            bail!("下载流提前结束（未收到完成确认）");
        }
    }

    let name = match name {
        Some(name) => sanitize_name(name)?,
        None => format!("blob-{}", &ticket.hash().to_hex()[..12]),
    };
    let path = place_export(&inner.store, ticket.hash(), out_dir, &name).await?;
    let bytes = tokio::fs::metadata(&path).await?.len();
    let _ = inner.events.send(Event::Downloaded {
        from: None,
        path: path.clone(),
        bytes,
    });
    Ok(DownloadOutcome { path, bytes })
}

/// 先把 blob 导出到临时文件，再硬链接到不冲突的目标名，避免覆盖已有文件。
async fn place_export(store: &FsStore, hash: Hash, dir: &Path, name: &str) -> Result<PathBuf> {
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("创建目录失败：{}", dir.display()))?;
    let tmp = dir.join(format!(".qep2p-{}-{}.part", &hash.to_hex()[..12], nanos()));
    let _ = tokio::fs::remove_file(&tmp).await;
    store
        .blobs()
        .export(hash, &tmp)
        .await
        .with_context(|| format!("导出到 {} 失败", tmp.display()))?;

    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let ext = path.extension().map(|s| s.to_string_lossy().to_string());

    for n in 0..1000 {
        let filename = if n == 0 {
            name.to_string()
        } else {
            match &ext {
                Some(ext) => format!("{stem} ({n}).{ext}"),
                None => format!("{stem} ({n})"),
            }
        };
        let candidate = dir.join(&filename);
        match tokio::fs::hard_link(&tmp, &candidate).await {
            Ok(()) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                if tokio::fs::try_exists(&candidate).await.unwrap_or(true) {
                    continue;
                }
                match tokio::fs::rename(&tmp, &candidate).await {
                    Ok(()) => return Ok(candidate),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => {
                        let _ = tokio::fs::remove_file(&tmp).await;
                        return Err(e).context("落盘失败");
                    }
                }
            }
        }
    }
    let _ = tokio::fs::remove_file(&tmp).await;
    bail!("同名文件过多，无法生成不冲突的文件名")
}

fn sanitize_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("文件名为空");
    }
    if name.len() > 200 {
        bail!("文件名过长");
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        bail!("文件名包含路径分隔符");
    }
    if name == "." || name == ".." || name.starts_with('.') {
        bail!("拒绝隐藏文件或点目录名");
    }
    if name.chars().any(char::is_control) {
        bail!("文件名包含控制字符");
    }
    Ok(name.to_string())
}

fn prepare_state_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("创建 state 目录失败：{}", dir.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("设置目录权限失败：{}", dir.display()))?;
    Ok(())
}

fn lock_state_dir(dir: &Path) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    let path = dir.join("lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("打开 {} 失败", path.display()))?;
    // SAFETY: fd 来自仍然存活的 File。
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            bail!("state 目录已被另一个 qep2p 进程占用：{}", dir.display());
        }
        return Err(err).with_context(|| format!("锁定 {} 失败", path.display()));
    }
    Ok(file)
}

fn load_or_create_key(dir: &Path) -> Result<SecretKey> {
    let path = dir.join(KEY_FILE);
    if path.exists() {
        return load_key(&path);
    }
    let key = SecretKey::generate();
    match write_new(&path, hex::encode(key.to_bytes()).as_bytes()) {
        Ok(()) => Ok(key),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => load_key(&path),
        Err(e) => Err(e).context("写入身份私钥失败"),
    }
}

fn load_key(path: &Path) -> Result<SecretKey> {
    use std::os::unix::fs::PermissionsExt;
    let meta =
        std::fs::symlink_metadata(path).with_context(|| format!("读取 {} 失败", path.display()))?;
    if !meta.file_type().is_file() {
        bail!("{} 不是普通文件，拒绝使用", path.display());
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        bail!(
            "私钥 {} 权限过宽（{:o}），请执行 chmod 600 后重试",
            path.display(),
            mode
        );
    }
    let text = std::fs::read_to_string(path)?;
    let raw = hex::decode(text.trim()).context("私钥不是合法十六进制")?;
    let arr: [u8; 32] = raw
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("私钥长度必须是 32 字节"))?;
    Ok(SecretKey::from_bytes(&arr))
}

fn write_new(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)?;
    f.sync_all()?;
    Ok(())
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ControlRequest {
    Offer {
        v: u32,
        ticket: String,
        name: String,
        size: u64,
        host: String,
    },
    Ping {
        nonce: u64,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ControlResponse {
    Offer {
        ok: bool,
        path: Option<String>,
        bytes: u64,
        error: Option<String>,
    },
    Pong {
        nonce: u64,
    },
}

#[derive(Clone)]
struct ControlHandler {
    inner: Arc<Inner>,
}

impl fmt::Debug for ControlHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ControlHandler")
    }
}

impl ProtocolHandler for ControlHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let remote = conn.remote_id();
        if !self.inner.allow.is_empty() && !self.inner.allow.contains(&remote) {
            conn.close(VarInt::from_u32(2), b"not allowed");
            return Ok(());
        }
        if let Err(e) = self.handle(conn).await {
            let msg = format!("{e:#}");
            // 对端读完响应后正常断开时，QUIC 可能报 connection lost，不算错误。
            if !msg.contains("closed by peer") && !msg.contains("connection lost") {
                eprintln!("[qep2p] 控制连接 {} 失败：{msg}", short_id(&remote));
            }
        }
        Ok(())
    }
}

impl ControlHandler {
    async fn handle(&self, conn: Connection) -> Result<()> {
        let remote = conn.remote_id();
        let (mut send, mut recv) = conn.accept_bi().await.context("接受控制流失败")?;
        loop {
            let req = match read_frame_opt::<ControlRequest>(&mut recv).await? {
                Some(req) => req,
                None => break,
            };
            match req {
                ControlRequest::Ping { nonce } => {
                    write_frame(&mut send, &ControlResponse::Pong { nonce }).await?;
                }
                ControlRequest::Offer {
                    v,
                    ticket,
                    name,
                    size,
                    host,
                } => {
                    if v != PROTOCOL_VERSION {
                        let resp = ControlResponse::Offer {
                            ok: false,
                            path: None,
                            bytes: 0,
                            error: Some(format!("协议版本不一致：{v}")),
                        };
                        write_frame(&mut send, &resp).await?;
                        continue;
                    }
                    let resp = self.offer(remote, ticket, name, size, host).await;
                    write_frame(&mut send, &resp).await?;
                }
            }
        }
        send.finish().ok();
        // 等发送端读完响应再关闭连接，避免响应在 QUIC 关闭时被丢掉。
        let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
        Ok(())
    }

    async fn offer(
        &self,
        from: EndpointId,
        ticket: String,
        name: String,
        size: u64,
        _host: String,
    ) -> ControlResponse {
        let result = async {
            if size > self.inner.max_file_size {
                bail!("文件超过上限（{size} 字节）");
            }
            let ticket: BlobTicket = ticket.parse().context("ticket 解析失败")?;
            if ticket.addr().id != from {
                bail!("ticket 的提供者与连接身份不一致");
            }
            let inbox = self
                .inner
                .inbox
                .clone()
                .context("本端未开启接收目录（--inbox）")?;
            let name = sanitize_name(&name)?;
            let _ = self.inner.events.send(Event::Offer {
                from,
                name: name.clone(),
                size,
            });
            let outcome = fetch_ticket(&self.inner, &ticket, &inbox, Some(&name)).await?;
            Ok::<_, anyhow::Error>(outcome)
        }
        .await;

        match result {
            Ok(outcome) => {
                eprintln!(
                    "[qep2p] 收到 {} 的 {}（{} 字节），保存到 {}",
                    short_id(&from),
                    printable(&name, 80),
                    outcome.bytes,
                    outcome.path.display()
                );
                ControlResponse::Offer {
                    ok: true,
                    path: Some(outcome.path.display().to_string()),
                    bytes: outcome.bytes,
                    error: None,
                }
            }
            Err(e) => {
                let reason = format!("{e:#}");
                let _ = self.inner.events.send(Event::Rejected {
                    from,
                    reason: reason.clone(),
                });
                eprintln!(
                    "[qep2p] 拒绝 {} 的 {}：{}",
                    short_id(&from),
                    printable(&name, 80),
                    printable(&reason, 200)
                );
                ControlResponse::Offer {
                    ok: false,
                    path: None,
                    bytes: 0,
                    error: Some(reason),
                }
            }
        }
    }
}

async fn write_frame<T: Serialize>(send: &mut SendStream, msg: &T) -> Result<()> {
    let data = serde_json::to_vec(msg).context("序列化控制消息失败")?;
    if data.len() > MAX_CONTROL_FRAME {
        bail!("控制消息过大：{} 字节", data.len());
    }
    send.write_all(&(data.len() as u32).to_be_bytes())
        .await
        .context("写控制帧头失败")?;
    send.write_all(&data).await.context("写控制帧失败")?;
    Ok(())
}

async fn read_frame<T: DeserializeOwned>(recv: &mut RecvStream) -> Result<T> {
    read_frame_opt(recv)
        .await?
        .context("对端在发送控制消息前关闭了流")
}

async fn read_frame_opt<T: DeserializeOwned>(recv: &mut RecvStream) -> Result<Option<T>> {
    let mut header = [0u8; 4];
    if !read_exact_opt(recv, &mut header).await? {
        return Ok(None);
    }
    let len = u32::from_be_bytes(header) as usize;
    if len == 0 || len > MAX_CONTROL_FRAME {
        bail!("非法的控制帧长度：{len}");
    }
    let mut buf = vec![0u8; len];
    if !read_exact_opt(recv, &mut buf).await? {
        bail!("控制帧在读取过程中中断");
    }
    let msg = serde_json::from_slice(&buf).context("解析控制消息失败")?;
    Ok(Some(msg))
}

async fn read_exact_opt(recv: &mut RecvStream, buf: &mut [u8]) -> Result<bool> {
    let mut off = 0;
    while off < buf.len() {
        match recv.read(&mut buf[off..]).await? {
            Some(0) => return Ok(false),
            Some(n) => off += n,
            None => {
                if off == 0 {
                    return Ok(false);
                }
                bail!("流在数据中途结束（已读 {off}/{}）", buf.len());
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rejects_escape_attempts() {
        for bad in [
            "", "..", ".", "../x", "a/b", "a\\b", ".hidden", "a\0b", "a\nb",
        ] {
            assert!(sanitize_name(bad).is_err(), "{bad:?} 应被拒绝");
        }
        assert_eq!(sanitize_name("报告 2026.pdf").unwrap(), "报告 2026.pdf");
    }

    #[test]
    fn relay_choice_rejects_empty_custom() {
        let choice = RelayChoice::Custom {
            urls: vec![],
            token: None,
        };
        assert!(choice.to_mode().is_err());
    }
}
