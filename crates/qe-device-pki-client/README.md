# qe-device-pki-client

设备 PKI 客户端：一次性注册、TOFU/pin 信任、mTLS 防抖通讯。

> **临时验证用 crate**。注册与通讯逻辑验证通过后，可原样移植进真实客户端项目。

## CLI

```bash
# 1. 用注册链接注册（auto 型直接拿证书）
qe-device-pki-client enroll "https://node:4440/enroll?token=T&slot=S" --trust

# approval 型：等待管理员批准（默认 5 分钟）
qe-device-pki-client enroll "https://...&token=T&slot=S" --trust --wait

# 1b. 等待审批期间中断/超时后续跑（私钥与 enroll_id 已落盘，不需要原链接）
qe-device-pki-client poll --slot S --wait-secs 300

# 2. 建立 WebSocket 隧道（**建连即完成注册激活**，双向消息走这一条连接）
qe-device-pki-client channel

# 2b. 或者走 HTTP 兜底：mTLS 回连确认
qe-device-pki-client confirm

# 3. 心跳 + 防抖重连演示
qe-device-pki-client ping --count 5

# 4. 查看本地凭证
qe-device-pki-client status
```

`--data-dir` 可覆盖数据目录（默认 `~/.local/share/qe-device-pki-client`）。

链接里含一次性 token，走 argv 会留在 shell history / `ps` 里 —— 传 `-` 表示
从 stdin 读：

```bash
echo "$LINK" | qe-device-pki-client enroll - --trust
```

## 服务端身份校验（三优先级）

1. **pin**：`...&pin=sha256/<hex>` 或 `pin=sha256spki/<hex>`，只接受该指纹
2. **已信任记录**：本地 trust store 里确认过的指纹
3. **TOFU**：首次连接**打印指纹**并要求人工核对（交互确认，或显式 `--trust`）后写入 trust store

判出来的指纹会被**钉进真正发送 token/CSR 的那条 TLS 连接**（自定义
`ServerCertVerifier` + `use_preconfigured_tls`）——否则校验的是探测连接、传凭证
的是另一条连接，中间人可以两条连接出示不同证书把 token 骗走。

拿不到服务端证书时**硬失败**，不会带着空指纹继续。

mTLS 阶段同理：用服务端下发的 `ca_chain_pem` 作为信任根做链验证 + 主机名校验，
**不再** `danger_accept_invalid_certs`。

无论哪条路径，客户端都会把「实际看到的证书指纹」作为 `tls_cert_seen_fp` 上报，
服务端据此做中间人检测（不一致返回 `fp_mismatch`）。

## 本地凭证

```
<data-dir>/
├── creds/<slot>/
│   ├── cert.pem        # 0600
│   ├── key.pem         # 0600
│   ├── ca-chain.pem    # 0600
│   └── meta.json       # 0600
├── pending/<slot>.json # 进行中的 approval 注册：私钥 + enroll_id（0600）
├── trust-store.json
└── outbox/             # sled：未 ACK 消息队列
```

## 防抖通讯

双向，且两侧都是**至少一次 + 确认成功才推进水位/游标**：

**上行（设备 → 服务端）**

- **指数退避**：100ms → 30s 上限，jitter 20%；**成功一轮后按 5s 心跳间隔**再探
- **消息可靠性**：出站消息单调 `seq`，写 sled 后才经 `POST /messages`（mTLS）
  发送，**收到服务端 ACK 才删除**；重连后按 seq 逐条真正重发，失败即停止本轮保序。
  服务端按 `(serial, seq)` 幂等去重（重复也是 2xx），所以重试是安全的
- **心跳**：5s（`ping` 每次心跳入队一条 heartbeat 消息，剩余待 ACK 条数会实时打印）

**下行（服务端 → 设备）**

```rust
let d = downlink::Dispatcher::new();
d.subscribe("config", |m| async move { /* m.id / m.kind / m.payload */ Ok(()) });
d.subscribe_all(|m| async move { println!("{}: {}", m.kind, m.payload); Ok(()) });
downlink::poll_once(&cfg, &d, &state_path, 50).await?;
```

`ping` 每个心跳拉一批下行消息、本地分发、**处理成功才 `ack`**；游标落
`<data-dir>/downlink-<slot>.json`（0600），重启后从上次位置继续。
handler 必须**幂等**（服务端是至少一次，处理失败会重投）。

## 测试

```bash
cargo test -p qe-device-pki-client   # 53 passed（49 单元 + 4 跨 crate 契约）
```
