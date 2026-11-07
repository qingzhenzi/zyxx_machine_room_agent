> **已并入统一入口（P4 已完成）**：本 crate 已无独立二进制（`src/main.rs` 已移除），
> 仅作为库维护；部署运行统一用 [`qe-agent`](../../apps/qe-agent/README.md) 的
> `qe-agent serve --role node`（或 `--role both`），安装用 `sudo bash scripts/install-agent.sh`。

# qe-agent-node（房间节点 agent，P2）

经 **device-pki mTLS** 拉取、校验并导入镜像/资源；大文件走 S3 预签名直连，
device-pki 消息只承载控制信令与状态。设计契约见
[`docs/storage-design.md`](../../docs/storage-design.md) §15–§17。

## 流程

1. 复用 `qe-device-pki-client` 的证书/TOFU 信任与 mTLS 传输（不新增认证体系）；
2. 加载本地配置（env + `AGENT_CONFIG_FILE`）→ mTLS `confirm` →
   `GET {DEVICE_PKI_MTLS_URL}/agent/config` 拉取生效策略（失败仅 warn，继续用本地策略）；
3. 下行 `image.pull`（device-pki kind）触发任务；每次任务开始读取**当前策略**的
   `lxc_auto_start`；
4. 通过 mTLS `POST /resources/presign` 获取 manifest / 分片 / 签名的短 TTL 预签名 URL；
5. 拉取 → Ed25519 验签 manifest → 逐 chunk 校验 sha256/size → 格式 magic；
6. 按类型导入：
   - `docker_archive` → `docker load -i`；`oci_layout` → `ctr images import`
   - `tar`（LXC）→ 合成 metadata 后 `incus image import`；`LXC_AUTO_START=true`
     （或被远端/下行策略打开）时继续 `incus launch`；
     无 Incus 时 plain LXC 解包到 `LXC_ROOTFS_DIR/<name>/rootfs`
   - `qcow2`/`raw`（虚拟机）→ `virsh vol-create-as` + `vol-upload` 到 `LIBVIRT_POOL`
7. 上行 `transfer.progress` / `transfer.completed` 回写 `transfers` 与房间发布状态；
8. 下行 `config.update` 应用管理端二次下发的完整策略（见下）。


## 配置

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `DEVICE_PKI_MTLS_URL` | 必填 | 如 `https://node:4441` |
| `DEVICE_SLOT` | 必填 | 设备槽位（与证书 CN 一致） |
| `DEVICE_DATA_DIR` | `./data` | 客户端状态/凭证目录（复用 device-pki-client） |
| `AGENT_CONFIG_FILE` | `<DEVICE_DATA_DIR>/agent-config.json` | 本地策略配置文件（JSON，格式见下） |
| `CACHE_DIR` | `./cache` | 内容寻址分片缓存 |
| `CACHE_MAX_BYTES` | `10737418240` | 缓存上限（LRU），范围 0..=1 TiB |
| `DOWNLOAD_CONCURRENCY` | `4` | 并发分片数，范围 1..=32 |
| `POLL_INTERVAL_SECS` | `2` | 下行轮询间隔（秒），范围 1..=60 |
| `DOCKER_BIN` / `CTR_BIN` / `INCUS_BIN` / `VIRSH_BIN` | 同名命令 | 可指向 mock 脚本 |
| `LIBVIRT_URI` | `qemu:///system` | libvirt 连接 URI（非 root 必须显式，否则默认 session 连接找不到 pool） |
| `LIBVIRT_POOL` | `default` | libvirt storage pool |
| `LXC_AUTO_START` | `false` | Incus 导入成功后是否自动 `incus launch` 启动实例 |
| `LXC_ROOTFS_DIR` | `/var/lib/lxc` | plain LXC rootfs 目录 |

### 本地配置文件

```json
{ "policy": { "lxc_auto_start": true, "poll_interval_secs": 2 } }
```

`policy` 是**部分 patch**，只写需要覆盖的字段；文件不存在等同于空 patch。

### 策略优先级与动态生效

生效策略（低 → 高）：**内置默认 < 配置文件 patch < 显式 env < 远端/下行**。
只有显式设置的 env 才覆盖文件；`LXC_AUTO_START` / `DOWNLOAD_CONCURRENCY` /
`CACHE_MAX_BYTES` / `POLL_INTERVAL_SECS` 均参与优先级。

- 启动：`confirm` 成功后 mTLS `GET /agent/config`；成功则记录 revision 与各字段并应用，
  失败（网络/5xx/非法值）只 warn，继续用本地策略，不影响启动。
- 运行时：下行 `config.update`（payload `{"policy":{完整策略},"revision":n}`）校验后立即替换；
  `lxc_auto_start` / `poll_interval_secs` 下一次 `image.pull` / 轮询立即生效。
- `download_concurrency` / `cache_max_bytes` 在启动时固化进 Puller/缓存，变更只记录
  “需重启生效”的 warn，不重建、不重启。
- mTLS 端点、slot、路径、可执行文件、libvirt pool 等**本地专属字段永不接受**
  配置文件/远端/下行覆盖。


## 运行

本 crate 已无独立二进制，统一由 `qe-agent` 入口加载：

```bash
# workspace 根目录
cargo build -p qe-agent --release
DEVICE_PKI_MTLS_URL=https://node:4441 DEVICE_SLOT=room-101 \
  DEVICE_DATA_DIR=/var/lib/qe-agent RUST_LOG=info \
  qe-agent serve --role node
```

生产用 `sudo bash scripts/install-agent.sh` 安装 systemd 单元；单服务推荐
`qe-agent serve --role both --ipc-socket /run/qe-agent/agent.sock`（node + machine 同进程）。

> 生产环境必须为 device-pki 服务端与 qe-api 配置相同的 `STORAGE_SIGNING_KEY_BASE64`，
> 否则重启后 manifest 公钥变化，已签清单无法再验签。
