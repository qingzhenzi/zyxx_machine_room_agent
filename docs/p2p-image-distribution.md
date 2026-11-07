# P2P 镜像分发（P2P over 内网）

状态：**已接入 image.pull 链路，可开关（`QE_P2P_ENABLED`，默认关）**。
实现：`crates/qep2p`（iroh/QUIC + BLAKE3）、`crates/qe-agent-node/src/p2p.rs`（产物
分享/下载 + manifest 分片校验）、`room_node/crates/device-pki/src/p2p.rs`（seed 登记）。
本机 P2P 分享→下载→分片校验有真跑的单测（见 §6）。

## 1. 数据流

```text
控制台 / qe-api                         device-pki (mTLS, 4441)
      │ image.pull 下行                        ▲   │
      ▼                                        │   │ GET /machine/p2p/peers?digest=
  qe-agent (node 角色)                          │   │ POST /machine/p2p/seed
      │                                        │   ▼
      ├──1. 先查 peers（QE_P2P_ENABLED=1）──────┘  image_p2p_seeds 表
      │
      ├──2. 对每个 peer ticket：qep2p 下载 → 按 manifest chunks 逐片
      │      校验 sha256 + total_size ── 通过 ──► 跳过 HTTP 分片下载
      │                                        （transfer.completed.source = "p2p"）
      │
      ├──3. 无 peer / 下载失败 / 校验失败 ──► 回退既有 HTTP 预签名分片下载
      │                                        （source = "http"，回退无感）
      │
      ├──4. import（docker load / incus image import / virsh vol-*）
      │
      └──5. best-effort：把组装产物 share 成 qep2p ticket 并 POST seed
              （失败只记日志，不影响 pull 结果；TTL 默认 3600s）
```

关键点：**manifest 是唯一校验根**。`Manifest` 里没有产物级 sha256，只有
`total_size` + `chunks[]`（每个 chunk 有 `object_key` / `sha256` / `size` / `offset`）。
P2P 下载完成后必须按 chunk 的 offset/size 切分、逐片重算 sha256 并与
`chunk.sha256` 比对，同时校验文件大小 == `total_size`；任何一项不符都删除下载
产物并回退 HTTP。P2P 只负责省掉大文件分片下载与跨机带宽，**不降低**校验强度。

## 2. 契约

### POST /machine/p2p/seed（mTLS 设备入口）

请求：

```json
{
  "manifest_digest": "sha256:<64 hex> 或 64 位 hex",
  "blob_ticket": "qep2p BlobTicket 字符串",
  "size_bytes": 1073741824
}
```

- 服务端把 digest 归一化为 64 位小写 hex 存储；
- 按 `(manifest_digest, serial)` upsert（serial 来自设备证书）；
- TTL：环境变量 `P2P_SEED_TTL_SECS`，默认 3600s，范围 60..=86400（越界回退默认）。

响应：

```json
{ "ok": true, "expires_at": "2026-11-07T12:00:00+00:00" }
```

### GET /machine/p2p/peers?digest=sha256:...

- 排除请求者自己、跳过已过期 seed、按 `updated_at` 倒序、最多 20 条；
- `digest` 缺失或非法 → 400。

响应：

```json
{
  "peers": [
    {
      "serial": "…",
      "blob_ticket": "…",
      "size_bytes": 1073741824,
      "updated_at": "2026-11-07T11:00:00+00:00"
    }
  ]
}
```

### DB（room_node/migrations/0029_image_p2p_seeds.sql）

```sql
image_p2p_seeds(
  id bigserial pk,
  manifest_digest text not null,
  serial text not null references device_enrollment(serial) on delete cascade,
  blob_ticket text not null,
  size_bytes bigint not null,
  updated_at timestamptz not null default now(),
  expires_at timestamptz not null,
  unique(manifest_digest, serial)
)
+ index(manifest_digest, expires_at)
```

## 3. agent 侧实现

`crates/qe-agent-node/src/p2p.rs`：

| 项 | 说明 |
| --- | --- |
| `QE_P2P_ENABLED` | `1/true/yes/on/y` 开启，默认关；每次 `image.pull` 重新读取 |
| `share_artifact` | `qep2p::Node::share_path` → `(ticket, size)` |
| `fetch_artifact` | `parse_ticket` + `download`，单 peer 超时 60s |
| `verify_artifact` | 总大小 + 分片连续覆盖 + 逐片 sha256 |
| `P2pDistribution` | 组合 `BlobExchange`（生产 = `qep2p::Node`）与 `SeedRegistry`（生产 = `MtlsSeedRegistry`，走 device-pki） |
| `MTlsSeedRegistry` | `qe_device_pki_client::p2p::{fetch_peers, post_seed}` |

`job.rs` 编排：

1. `QE_P2P_ENABLED` 且 boot 阶段 P2P 节点已启动（`apps/qe-agent/src/boot/services.rs`
   的 `P2P_HANDLE`）→ 先 `GET peers`；
2. 对 peer 依次 `fetch_artifact`（最多 5 个，单个 60s），校验通过即用该产物；
3. 失败/无 peer 回退 `Puller::pull`（HTTP 预签名分片）；
4. `transfer.completed` 增加可选字段 `source: "p2p" | "http"`（旧服务端忽略未知字段）；
5. 成功后 `share_artifact` + `POST seed`（best-effort）。

`QE_P2P_ENABLED=1` 但 P2P 节点未启动（boot services 未就绪/失败）时：记录 warning，
本次 pull 回退 HTTP，不 panic、不阻塞启动。

## 4. 开关 / 回退 / 安全

开关：

```bash
# /etc/qe-agent.env
QE_P2P_ENABLED=1
# 可选：device-pki 侧 seed TTL
P2P_SEED_TTL_SECS=3600
```

回退矩阵：

| 场景 | 行为 |
| --- | --- |
| `QE_P2P_ENABLED` 未开 | 完全不查 peers，走 HTTP |
| P2P 节点未启动/启动失败 | warning + HTTP |
| peers 查询失败 / 无 peer | HTTP |
| peer 下载超时/失败 | 尝试下一个 peer，全部失败回退 HTTP |
| 下载产物大小或分片 sha256 不符 | 删除产物、尝试下一个 peer，全部失败回退 HTTP |
| 成功 pull 后 share/seed 失败 | 只记日志，pull 结果不变 |

安全：

- **ticket 即能力**：blob ticket 包含节点地址与 blob 哈希，拿到 ticket 的内网设备
  可直接下载；因此 P2P 只跑在内网设备之间，peers/seed 接口挂在 device-pki 的
  **mTLS listener**（设备证书鉴权），不暴露在公网；
- seed 有 TTL（默认 1h），过期自动不可见；`serial` 外键级联删除；
- 校验不降级：即使 ticket 被恶意投毒，下载后也会因 manifest 分片 sha256 不符被拒绝；
- ticket 长度/大小上限校验（ticket ≤ 8 KiB、size ≤ 1 TiB）防滥用。

## 5. 已知限制

- **无 swarm 调度**：没有 tracker/DHT 选源，peers 按 `updated_at` 倒序取前 20，
  agent 顺序尝试最多 5 个；源少时命中率取决于内网在线设备数；
- **单 provider 即够用**：当前每台成功拉取的设备都登记自己的 ticket，本质是
  "多 seed 列表"，但没有分片级并行下载/多源聚合；
- **未接入多机实测**：本机单测验证了 share→download→分片校验链路；真实机房
  多机（跨交换机/多网卡/mDNS 不可用）仍需现场验证，必要时配置自建 relay；
- 成功率指标尚未上报（只有 `source` 字段与日志），后续可在 completed payload
  增加 `p2p_bytes` / `p2p_peer_serial` 做统计；
- iroh/QUIC 需要内网 UDP 可达；纯 TCP 网络下会回退 HTTP。

## 6. 测试与验证

```bash
# agent 侧（含真跑 P2P：两个本地 qep2p Node share→fetch 1MiB 并逐片校验）
cargo test -p qe-agent-node -j4
# device-pki seed/peers 参数校验单测
cd ../room_node && cargo test -p qe-device-pki --lib -j4
# 客户端响应解析单测
cargo test -p qe-device-pki-client -j4
```

手工验证（多机演示）：

```bash
# 设备 A：正常 image.pull（QE_P2P_ENABLED=1）→ 日志出现 "P2P seed 已登记"
# 设备 B：同 digest 的 image.pull → 日志出现 "P2P 命中"，transfer.completed.source=p2p
journalctl -u qe-agent -f | grep -E 'P2P|seed'
```
