# qep2p — 内网优先的 P2P 文件传输

一个可嵌入 Rust 程序、也可直接当 CLI 用的文件传输节点：**iroh/QUIC + TLS 认证 + BLAKE3 校验 + mDNS 发现 + 自建 relay 兜底**。默认只走内网，不依赖任何公共服务。

和上一版手写 QUIC/Noise 协议相比，0.2 把传输、身份、NAT 穿透、断点续传都交给了成熟组件，本仓库只保留节点生命周期、推送控制协议和自建 relay 的接线。

## 组成

| 层 | 组件 | 提供什么 |
| --- | --- | --- |
| 连接 / 身份 | [iroh](https://docs.rs/iroh/latest/iroh/) 1.4.1 | Ed25519 公钥即节点 ID；QUIC + TLS 1.3；打洞、relay 回退、连接迁移 |
| 文件 | [iroh-blobs](https://docs.rs/iroh-blobs/latest/iroh_blobs/) 0.103.1 | BLAKE3 校验流、按 range 下载、**断点续传**、内容寻址 |
| 发现 | `iroh-mdns-address-lookup` 0.6.1 | 同一广播域内按节点 ID 自动解析地址 |
| 节点 / 推送 | 本仓库 `node.rs` | `Node` 库 API、Offer/Ping 控制协议、mDNS/relay/收件箱配置 |
| 自建 relay | `iroh-relay` 1.4.1 | 自托管 relay server + token/allowlist 接入控制 |

> 版本固定是有意为之：`iroh-blobs` 目前还是 0.x，0.104 的文档自己标注“not production quality”，所以这里锁 **0.103.1**（`sendme` 0.37.1 同款），并用自己的 `Node` API 把上游变动挡住。

## 快速开始

```bash
cargo build -p qep2p --release \
  --config 'source.crates-io.replace-with="rsproxy"' \
  --config 'source.rsproxy.registry="sparse+https://rsproxy.cn/index/"'

# 本机身份（首次运行生成 <state>/node.key，0600）
./target/release/qep2p id
```

### 拉取：share + download

```bash
# 主机 A：分享文件，打印 ticket，保持运行
qep2p share ./report.pdf

# 主机 B：拿 ticket 下载（默认保存为 blob-<hash 前 12 位>）
qep2p download '<ticket>' --out ~/Downloads --name report.pdf
```

ticket 里已经带了提供方的 relay URL 和直连地址，跨网段也能用；同一局域网会优先直连。

### 推送：serve + send

```bash
# 主机 A：常驻接收端
qep2p serve --inbox ~/Downloads/qep2p

# 主机 B：按 endpoint id 推送（mDNS 自动解析地址）
qep2p send <A 的 endpoint id> ./report.pdf
```

`send` 内部的流程是：A 先 `share` 得到 ticket → 通过控制通道把 ticket 推给 B → B 用自己的 blob 库回连 A 下载。所以断点续传、校验和 relay 回退都仍然生效。

### 探测

```bash
qep2p ping <endpoint id> --count 4
```

`ping` 走控制通道（不是 ICMP），能验证“认证 + 控制面”是否连通。

## 自建 relay

默认 `RelayChoice::Disabled`，只有 mDNS/直连。需要跨网段或直连失败时，起一个内网 relay：

```bash
# 1) 安装 relay server（需要 server feature）
cargo install iroh-relay --version 1.4.1 --features server

# 2) 配置（token 只给 relay 用户读）
useradd --system --no-create-home --shell /usr/sbin/nologin iroh-relay || true
install -D -m0640 -o root -g iroh-relay deploy/relay.toml /etc/qep2p/relay.toml
# 编辑 access.shared_token，换成足够长的随机串

# 3) 启动
iroh-relay --config-path /etc/qep2p/relay.toml

# 或直接用 systemd
install -m0644 deploy/iroh-relay.service /etc/systemd/system/
systemctl daemon-reload && systemctl enable --now iroh-relay
```

客户端：

```bash
export QEP2P_RELAY=http://relay.intra:3340
export QEP2P_RELAY_TOKEN='<token>'

qep2p serve --inbox ~/Downloads/qep2p
qep2p share ./big.iso --relay-only     # ticket 里只放 relay 地址
```

`deploy/relay.toml` 是最小内网配置：**明文 HTTP + shared token**。只适合可信内网；要给不可信网络用，请在 `relay.toml` 里加 `tls` 段（Manual / LetsEncrypt）或把 relay 放到 TLS 反向代理后面。relay 只转发密文，看不到文件内容，但 HTTP 模式下元数据和 token 是明文。

`deploy/iroh-relay.service` 是配套 systemd 单元（`DynamicUser` + 沙箱指令）。

## 作为库使用

`qep2p` 是普通 Rust 库，宿主程序只需要一个 tokio runtime：

```rust
use std::time::Duration;
use qep2p::{Node, NodeConfig, RelayChoice};

let mut cfg = NodeConfig::new("/var/lib/qep2p");
cfg.inbox = Some("/var/lib/qep2p/inbox".into());
cfg.allow = vec![trusted_peer_id];          // 可选：只允许这些节点
cfg.relay = RelayChoice::Custom {
    urls: vec!["http://relay.intra:3340".into()],
    token: std::env::var("QEP2P_RELAY_TOKEN").ok(),
};

let node = Node::start(cfg).await?;
println!("my id: {}", node.id());

// 分享 / 拉取
let shared = node.share_path("/data/report.pdf".as_ref()).await?;
println!("ticket: {}", shared.ticket);
let got = node.download(&shared.ticket, "/data/out".as_ref()).await?;
println!("saved to {}", got.path.display());

// 推送给在线节点
let outcome = node.send_file(
    peer_addr,                              // iroh::EndpointAddr
    "/data/report.pdf".as_ref(),
    Duration::from_secs(120),
).await?;

// 控制面 RTT
let pong = node.ping(peer_addr, 4, Duration::from_millis(200), Duration::from_secs(10)).await?;

node.shutdown().await?;
```

要点：

- `Node` 必须活在 tokio runtime 里；所有网络方法都是 `async`。
- `Node::start` 会在 `state_dir` 上加进程锁，同一个目录不能同时跑两个实例；退出前调用 `shutdown()`，否则 router 会在 Drop 时中止。
- `share_path` 默认用 `ImportMode::TryReference`（硬链接/引用，不复制大文件）；分享期间不要修改源文件，否则 BLAKE3 校验会失败。
- `download` 会复用 `<state>/blobs` 里已有的分片，进程重启后也能续传。
- `subscribe()` 可以拿 `Event::{Offer, Progress, Downloaded, Sent, Rejected}` 做进度/审计。
- `EndpointAddr` 来自 `iroh`：可以只给节点 ID（靠 mDNS 解析），也可以带 `with_ip_addr` / `with_relay_url`。

## 身份与信任

- 首次运行生成 `<state>/node.key`（32 字节 Ed25519 私钥，0600；目录 0700）。
- **endpoint id = 公钥**，64 位十六进制；连接由 TLS 1.3 双向认证，relay 只转发密文。
- `serve --allow <id>`（或库里的 `NodeConfig::allow`）限制**推送/控制连接**；不在名单里的节点无法 Offer 或 Ping。注意它不拦截“持有 ticket 的下载”——ticket 本身就是下载凭证，要像密码一样传递。
- 接收目录默认关闭：不传 `--inbox` 时所有 Offer 都会被拒绝，不会写盘。
- ticket 是 bearer token：谁拿到 ticket，谁就能在 provider 运行期间下载。ticket 要当秘密传递。
- 文件落盘前会校验 BLAKE3 + 大小；文件名只允许纯文件名（拒绝 `/`、`\`、`..`、隐藏文件、控制字符）；同名文件自动改成 `name (1).ext`，不覆盖。

## 协议细节

- 文件 ALPN：`iroh_blobs::ALPN`。一条 QUIC 流上请求 BLAKE3 hash/range，提供方回 BLAKE3 verified stream。
- 控制 ALPN：`qep2p/control/1`。每帧 `u32 大端长度 + JSON`，单帧上限 64 KiB：
  - `{"kind":"offer","v":1,"ticket":"...","name":"...","size":123,"host":"..."}`
  - `{"kind":"ping","nonce":...}`
  - 响应 `{"kind":"offer","ok":true,"path":"...","bytes":123,"error":null}` / `{"kind":"pong","nonce":...}`
- 接收端会校验 `ticket.addr().id == 控制连接身份`，防止让本端去下载第三方内容。
- 下载完成后先导出到 `.qep2p-<hash>-<nanos>.part`，再硬链接到不冲突的目标名；跨文件系统时退回 `rename`，仍然先做存在性检查。

## 和 qeaudit 的关系

`qep2p` 是独立 crate，不依赖 `qeaudit`；放在同一 workspace 只是便于一起构建。拆走时把 `qep2p/` 目录整体移走、删掉根 `Cargo.toml` 的 `[workspace]` 即可。

## 测试

```bash
cargo test -p qep2p -- --test-threads=1
```

覆盖：

- `share_path` → `download` 往返（300 KB，内容逐字节比对）
- `send_file` 推送 + 同名文件不覆盖（第二次落成 `report (1).bin`）
- allowlist 拒绝未知节点
- 控制通道 Ping

自建 relay 路径已手工验证：本地 `iroh-relay --config-path deploy/relay.toml`（HTTP + token），两端都用 `--relay-only` / `id@http://relay:3340`，share → download、ping、push 均通过。

## 已知限制 / 下一步

- CLI 一次只处理一个文件；目录传输还没接（iroh-blobs 本身支持 collection，后续可加）。
- 没有交互式“接受/拒绝”提示，`--inbox` 配置了就直接收。
- ticket 不能撤销；provider 停止后 ticket 失效。
- relay 配置里的 shared token 不能热更新，改完要重启 relay。
- mDNS 只在同一二层广播域；跨网段请用 relay，或 `--addr` 显式给地址。
- 没有做磁盘配额；接收端上限由 `--max-size-mb` 控制，存储会保留已下载 blob（这是续传的前提）。
- `iroh-blobs` 0.103.1 是刻意 pin 的；升级前先跑 `cargo test -p qep2p`，并检查上游 0.x 的 breaking change。

## 文件结构

```
qep2p/
  src/lib.rs       公开导出
  src/node.rs      Node / NodeConfig / Event / 控制协议 / 落盘
  src/main.rs      CLI
  tests/e2e.rs     库级端到端测试
  deploy/          自建 relay 配置与 systemd 单元
  README.md
```
