# qe-agent（统一机器 agent 客户端）

一个二进制，覆盖房间主节点与子机上的三种能力：

| 能力 | 子命令 | 说明 |
| --- | --- | --- |
| **注册/身份** | `qe-agent enroll <链接>` | 走 device-pki 一次性 token，拿 mTLS 设备证书与 CA 链 |
| **镜像/策略（主节点）** | `qe-agent run --role node` | 下行 `image.pull` 拉取/验签/校验/导入；`GET /agent/config` 拉策略；`config.update` 热更新 |
| **机器遥测（子机/主节点）** | `qe-agent run --role machine` | 两档采集（fast/full）→ 默认 mTLS 上报 `/machine/report` |
| **单服务双角色** | `qe-agent run --role both` | 同一进程同时跑 node + machine（推荐；单 IPC socket） |
| **本机 IPC（GUI）** | `qe-agent run ... --ipc-socket <PATH>` | Unix socket HTTP：status / enroll / logs / policy，见 `docs/agent-ipc.md` |
| **本地自检** | `qe-agent status` | 查看凭证目录、slot、serial、mTLS 端点、证书自检与守护进程能力快照 |

它把原先三个可执行文件收口为一个入口：

- `qe-device-pki-client` 的 enroll/凭证能力 → `qe-agent enroll` / `qe-agent status`；
- `qe-node-agent` 的镜像与控制面逻辑 → `qe-agent run --role node`（复用 `qe-node-agent` 库）；
- `qe-room-agent` 的遥测逻辑 → `qe-agent run --role machine`（复用 `qe-room-agent` 库）。

守护进程能力层（库内直调，不 spawn 子进程）：`qeaudit`（安全基线）、
`qe-container`（容器）、`virt-*`（虚拟化）、`qep2p`（P2P 传输）全部以库形式链入，
由遥测/后续功能按规则调用；`qe-agent status` 末尾给出各能力就绪快照（见 `src/caps.rs`）。

旧二进制暂时保留（P4 退场），行为与统一入口一致。

---

## 1. 构建与安装

```bash
# workspace 根目录
cargo build -p qe-agent --release
sudo install -m 0755 target/release/qe-agent /usr/local/bin/qe-agent
```

要求 Rust 1.88+（workspace `rust-version`；edition 2024）。

systemd 单元与 env 示例（推荐单服务 `--role both`）：

- [`deploy/qe-agent.service.example`](deploy/qe-agent.service.example) —— 单服务，`--role both` + `/run/qe-agent/agent.sock`；
- [`deploy/qe-agent.env.example`](deploy/qe-agent.env.example) —— `/etc/qe-agent.env`（0600）。

安装要点：`Group=qe-agent` + `RuntimeDirectory=qe-agent`，GUI 用户加入 `qe-agent` 组后
即可访问 IPC socket（0660）。

---

## 2. 身份与数据目录

设备证书由 **device-pki** 签发（唯一签发体系）：

```bash
# 管理端房间页/接口生成一次性注册链接后：
qe-agent enroll "<https://node:4440/enroll?token=…&slot=room-101>" --trust
```

- 凭证落在 `<data_dir>/creds/<slot>/`（`cert.pem` / `key.pem` / `ca-chain.pem` / `meta.json`，0600）；
- 数据目录优先级：`--data-dir` > `$DEVICE_DATA_DIR` > `./data`；
- 注册与运行必须使用同一个数据目录，否则运行时会报"加载设备凭证失败"。

---

## 3. 运行角色

### 3.1 node（房间主节点：镜像/策略）

```bash
DEVICE_PKI_MTLS_URL=https://node:4441 \
DEVICE_SLOT=room-101 \
DEVICE_DATA_DIR=/var/lib/qe-agent \
qe-agent run --role node
```

- 启动：加载本地策略（内置默认 < `AGENT_CONFIG_FILE` < 显式 env）→ mTLS `confirm`
  → `GET /agent/config` 拉远端策略（失败只 warn）→ 注册 `image.pull` / `config.update`
  → 按 `poll_interval_secs` 轮询下行；Ctrl-C 优雅退出；
- 上行 `transfer.progress` / `transfer.completed` 走 device-pki outbox（发送成功才删）；
- 导入目标：Docker/OCI、Incus/plain LXC、libvirt VM；相关可执行文件路径/池名见 node-agent 文档；
- 本角色是**常驻下行循环**，不支持 `--once/--print`（调试用旧二进制或日志）。

### 3.2 machine（机器遥测）

```bash
DEVICE_PKI_MTLS_URL=https://node:4441 \
DEVICE_SLOT=room-101 \
DEVICE_DATA_DIR=/var/lib/qe-agent \
REPORT_MODE=mtls \
qe-agent run --role machine
```

- 默认 **mTLS 上报**（`POST {DEVICE_PKI_MTLS_URL}/machine/report`）；`REPORT_MODE=bearer`
  可回退到旧的 `MACHINE_API_BASE` + `MACHINE_AGENT_TOKEN` 路径；
- CLI 覆盖：`--fast-interval <5..=60>` / `--full-interval <60..=3600>` /
  `--report-mode mtls|bearer` / `--data-dir <DIR>`（`--interval` 为旧参数，等价 fast）；
- 两档采集：`fast`（内置 /proc、df、hwmon，默认 10s，5..=60）与 `full`（含 platform-health /
  diagnose / qeaudit / docker / virsh，默认 300s，60..=3600）；频率默认取服务端策略
  （global → area → room → device，room 默认 + 单机覆盖），启动时与运行期经
  `GET /agent/config` 拉取；本机 env `ROOM_AGENT_FAST_INTERVAL_SECS` /
  `ROOM_AGENT_FULL_INTERVAL_SECS` 可覆盖内置默认；
- `--once`：立即采集并上报一份 full 后退出（成功 0 / 失败非 0）；
- `--print`：只打印契约 JSON，不发请求（不需要证书/ token）。

---

### 3.3 both（单服务，推荐）

```bash
DEVICE_PKI_MTLS_URL=https://node:4441 DEVICE_SLOT=room-101 DEVICE_DATA_DIR=/var/lib/qe-agent \
  qe-agent run --role both --ipc-socket /run/qe-agent/agent.sock
```

- node 在 tokio 主线程、machine 在独立线程；共用同一证书/数据目录；
- 只有一个 IPC socket，GUI 连它即可看到两个角色；
- 只有 node 角色轮询下行并维护 outbox（machine 不碰下行）。

### 3.4 本机 IPC（GUI）

`--ipc-socket` 启用 Unix socket HTTP 服务（只监听本机、0660）：

| 端点 | 用途 |
| --- | --- |
| `GET /v1/healthz` | GUI 判断 daemon 是否在跑 |
| `GET /v1/status` | 版本/角色/注册状态/serial/端点/运行时长/间隔 |
| `GET /v1/logs?limit=` | 最近日志（内存 2000 行环形缓冲） |
| `POST /v1/enroll` | 用注册链接注册（`trust`/`wait_secs`） |
| `GET /v1/policy` | 当前采集间隔（本地值） |

调试时只起 IPC（不跑角色、便于 GUI 联调）：

```bash
qe-agent serve --ipc-socket /tmp/qe-agent-dev.sock
QE_AGENT_IPC_SOCKET=/tmp/qe-agent-dev.sock pnpm tauri dev   # 在 agent-client 仓库
```

完整字段与错误语义见 [`docs/agent-ipc.md`](../../docs/agent-ipc.md)；
机器端完整部署步骤见 [`docs/agent-deploy.md`](../../docs/agent-deploy.md)。
注意：注册成功后 `/v1/status` 立即反映，但已启动角色需重启才会用新证书。

## 4. 配置优先级

- **身份/路径类**（`DEVICE_PKI_MTLS_URL`、`DEVICE_SLOT`、`DEVICE_DATA_DIR`、可执行文件、
  libvirt pool 等）只来自本机 env/CLI，**永不接受服务端下发**；
- **策略类**（镜像：`lxc_auto_start` / `poll_interval_secs` / `download_concurrency` /
  `cache_max_bytes`；遥测：`telemetry_fast_interval_secs` / `telemetry_full_interval_secs`）
  优先级：内置默认 < 本机配置文件 < 显式 env < 服务端
  （global → area → room → device，device 覆盖最后生效）；
- 具体字段与范围见 [qe-node-agent 配置](../qe-node-agent/README.md#配置) 与
  [machine-telemetry.md](../../../room_node/room_node_backend/docs/machine-telemetry.md)。

---

## 5. 验证

```bash
cargo build -p qe-agent
cargo test -p qe-agent -p qe-node-agent -p qe-room-agent
cargo clippy -p qe-agent --all-targets -- -D warnings
```

本机联调（需要 device-pki + 已注册设备）：

```bash
# 只打印一份 full payload
DEVICE_SLOT=room-101 qe-agent run --role machine --once --print | jq '.health'
# 上报一轮（mTLS）
DEVICE_PKI_MTLS_URL=https://127.0.0.1:4441 DEVICE_SLOT=room-101 \
DEVICE_DATA_DIR=/var/lib/qe-agent qe-agent run --role machine --once
```

管理端验收：控制台「机器总览」应看到该 serial 的新快照；`machine_snapshots.tier`
区分 `fast` / `full`。

---

## 6. 迁移说明（P4 计划）

| 旧入口 | 新入口 | 兼容期 |
| --- | --- | --- |
| `qe-node-agent` | `qe-agent run --role node` | 旧二进制保留，P4 标 deprecated 后移除 |
| `qe-room-agent` | `qe-agent run --role machine` | 同上 |
| `qe-device-pki-client enroll/status` | `qe-agent enroll/status` | CLI 保留用于联调 |

systemd 迁移：`qe-room-agent.service` → `qe-agent.service`（`--role both`）；
node-agent 目前无 unit（手工启动），迁移时一并并到这个单元。

### 退场时间表（P4）

| 版本 | 状态 | 动作 |
| --- | --- | --- |
| v0.1（当前） | 过渡 | 默认 mTLS / `--role both`；`qe-node-agent`、`qe-room-agent` 保留为**弃用 shim**（启动打印提示）；Bearer fleet token 仅作回退 |
| v0.2 | 删除旧入口 | 移除 `qe-node-agent`/`qe-room-agent` 二进制（保留库 crate），部署/CI/文档只认 `qe-agent` |
| v0.2+ | 删除旧认证 | 所有设备迁移到 mTLS 后，移除 `MACHINE_AGENT_TOKEN` 与 qe-api 的 Bearer `/api/v1/machine/report` |

删除条件：管理端 `/admin/devices` 没有 Bearer 设备流量；所有 systemd/脚本已指向
`qe-agent`；现场完成一次 mTLS 上报验收。
