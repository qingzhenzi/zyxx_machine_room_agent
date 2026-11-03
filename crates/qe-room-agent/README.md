> **已并入统一入口（P4 退场）**：新部署请用 [`qe-agent`](../agent/README.md) 的
> `qe-agent run --role machine`（或 `--role both`）；本 crate 作为库实现继续维护。

# qe-room-agent（房间机器遥测 agent，v1.1）

房间内「子机器端」agent：周期性采集主机清单、内置指标、健康结论与能力采集
（安全基线 qeaudit / 容器 qe-container / 虚机 virt-libvirt —— **进程内库直调**；
platform-health / platform-diagnose 为可选外部工具）的结果，
组装成契约 JSON 后上报给服务端。

契约以 [`machine-telemetry.md`](../../../room_node/room_node_backend/docs/machine-telemetry.md) **v1.1** 为准：

```text
room-agent（每个房间机器）
   ├─ fast 档（默认 10s）：内置采集 /proc、/sys、df（不做能力采集）
   ├─ full 档（默认 300s）：内置采集 + 安全 / 容器 / 虚机 / 平台工具
   ├─ 安全扫描独立规则（默认 300s，0 = 跟随 full 档）：刷新即经 mTLS 实时回传 full 快照
   ├─ 默认上报：POST {DEVICE_PKI_MTLS_URL}/machine/report（设备证书 mTLS，device-pki）
   └─ 兼容上报：POST {MACHINE_API_BASE}/machine/report + Authorization: Bearer（旧部署）
```

两档间隔可由服务端下发：启动时（以及运行期每 30 个 fast 周期）通过 mTLS
`GET /agent/config` 拉取 `global → area → room → device` 合并后的
`telemetry_fast_interval_secs` / `telemetry_full_interval_secs`。

设计原则：

- **默认 mTLS**：身份来自设备证书（`qe-agent enroll` 写入
  `<DEVICE_DATA_DIR>/creds/<DEVICE_SLOT>`），不再依赖 fleet token；
  `REPORT_MODE=bearer` 保留旧上报路径；
- **两档采集**：fast 只跑内置采集，full 才做能力采集（安全/容器/虚机/平台工具），
  既保证实时性又避免每 10s 跑重活；
- **内置采集只依赖 std**：不引入平台重依赖，容器/虚拟机里也能跑；
- **能力采集进程内优先、全部可选**：qeaudit / qe-container / virt-libvirt 以库形式
  直调（不 spawn 子进程），docker / virsh CLI 仅作回退；单项缺失/失败/超时只记录到
  `sections.agent.tools`，不影响整体上报；
- **安全扫描即时回传**：qeaudit 按独立间隔扫描，扫描完成立即经 mTLS 回传 full 快照，
  安全发现不等下一个 full 周期；
- **采集与 payload 组装不触碰网络**：`--once --print` 可在本机直接排查；
- **退出码可断言**：`--once` 成功 0、失败非 0，方便 systemd/CI。

---

## 1. 构建与安装

```bash
# workspace 根目录
cargo build -p qe-room-agent --release
sudo install -m 0755 target/release/qe-room-agent /usr/local/bin/qe-room-agent
```

要求 Rust 1.88+（workspace `rust-version`；edition 2024）。

---

## 2. 采集内容

### 2.1 内置采集（总会有）

| 数据 | 来源 | 去向 |
| --- | --- | --- |
| hostname / OS / kernel / arch / uptime | `/proc/sys/kernel/hostname`、`/etc/os-release`、`/proc/sys/kernel/osrelease`、`/proc/uptime`、编译期 arch | 顶层 `hostname`/`os_name`/`kernel_version`、`metrics.uptime_secs`、`sections.agent.host` |
| CPU 使用率 | `/proc/stat` 间隔 500ms 双采样 | `metrics.cpu_usage_percent` |
| load / CPU 型号 / 核心数 | `/proc/loadavg`、`/proc/cpuinfo` | `sections.agent.host`，参与健康阈值 |
| 内存 | `/proc/meminfo`（优先 `MemAvailable`） | `metrics.memory_usage_percent` |
| 磁盘 | `df -B1`（15s 超时，失败则空） | `metrics.disk_usage_percent`（根分区，缺失时取最满挂载点）、`sections.disks` |
| 网络接口累计流量 | `/proc/net/dev` | `sections.network` |
| 温度 | `/sys/class/hwmon/*/temp*_input`（毫摄氏度） | `sections.thermal`，最高温参与健康阈值 |
| machine_id | `/etc/machine-id`，缺失时 hostname 的 FNV-1a 哈希 | `sections.agent.host.machine_id` |

### 2.2 能力采集（进程内优先，CLI 兜底）

> **仅 full 档做能力采集**：fast 档（默认 10s）不做任何能力采集，
> `sections.agent.tools` 里五项均为 `{"available": false, "skipped": true}`。
> full 档（默认 300s）才执行；其中 `platform-diagnose`（默认每 60s）与
> `qeaudit`（默认每 300s）的结果会缓存复用，避免重复跑重活；
> 复用项在 `sections.agent.tools.<name>` 带 `cached: true` 与 `age_secs`。


| 能力 | 调用方式 | 输出位置 | 关闭开关 |
| --- | --- | --- | --- |
| qeaudit 安全基线 | **进程内** qeaudit 库（默认）；显式定制 `QEAUDIT_BIN`/`QEAUDIT_ARGS` 时回退外部二进制 | `sections.security = {summary, raw}` | `ROOM_AGENT_DISABLE_SECURITY` |
| 容器清单 | **进程内** qe-container（Docker Engine API）；失败回退 `docker ps --format '{{json .}}'` | `sections.containers[]`（name/image/state/status/id） | `ROOM_AGENT_DISABLE_DOCKER` |
| 虚拟机清单 | **进程内** virt-libvirt（libvirt API）；失败回退 `virsh list --all` | `sections.vms[]`（name/state/id） | `ROOM_AGENT_DISABLE_VMS` |
| platform-health | 外部命令 `collect --processes 10` | `sections.platform_health`（原样 JSON） | `ROOM_AGENT_DISABLE_HEALTH` |
| platform-diagnose | 外部命令 `-s generic -d 3 --json` | `sections.diagnose`（原样 JSON） | `ROOM_AGENT_DISABLE_DIAGNOSE` |

**超时与容错**：外部命令默认 45s 硬超时（`ROOM_AGENT_TOOL_TIMEOUT_SECS` 可调），
进程内采集不受该超时约束（库内自带超时）；启动失败、超时、解析失败都只写进
`sections.agent.tools.<name>`：

```json
{
  "platform_health": { "available": true, "duration_ms": 2032, "version": "platform-health/v2" },
  "qeaudit": { "available": true, "duration_ms": 22256, "exit_code": 2, "version": "0.1.0" },
  "platform_diagnose": { "available": true, "duration_ms": 4582, "exit_code": 2 },
  "docker": { "available": false, "duration_ms": 12, "error": "启动 `docker` 失败: No such file or directory (os error 2)" },
  "virsh": { "available": false, "disabled": true }
}
```

> **退出码语义**：`platform-diagnose` / `qeaudit` 用非 0 退出码表达「发现告警/故障」
> （qeaudit 默认 `--fail-on fail`，有 findings 时返回 2；进程内扫描用
> `Report::exit_code` 映射同一语义）。JSON 类工具只要 stdout 能解析成
> JSON 就采信数据并记录 `exit_code`，不把「有 findings」误判成「工具不可用」。
> docker / virsh 的 CLI 回退路径仍按非 0 退出码判失败。

**安全扫描独立规则 + 实时回传**：qeaudit 不跟 full 档同频死等——按
`ROOM_AGENT_SECURITY_INTERVAL_SECS`（默认 300s，**0 = 跟随 full 档**）独立调度；
扫描结果一旦刷新，立即组装 full 快照经 mTLS 回传（事件驱动），安全发现不等下一个
full 周期。缓存与 full 档共享：full 档刚刷新过则该次规则触发自然跳过
（`cached: true`），不重复扫描/上报。

---

## 3. 快速开始

```bash
# 只采集并打印 payload（不发请求；--print 模式下可省略 token）
./target/debug/qe-room-agent --once --print --device-slot room-101

# 本机指向外部工具的 release 二进制（platform-health / diagnose 仍是外部命令；
# qeaudit 默认进程内库直调，这里设 QEAUDIT_BIN 只为验证 CLI 回退路径）
PLATFORM_HEALTH_BIN=/home/qingzhenzi/Projects/QEStudio/common/platform-health/target/release/platform-health \
PLATFORM_DIAGNOSE_BIN=/home/qingzhenzi/Projects/QEStudio/common/platform-diagnose/target/release/platform-diagnose \
QEAUDIT_BIN=/home/qingzhenzi/Projects/QEStudio/room_agent/target/release/qeaudit \
./target/debug/qe-room-agent --once --print --device-slot room-101 | jq '.sections.agent.tools'

# 真实上报一轮（mTLS，需先 qe-agent enroll 拿证书；失败非 0）
DEVICE_PKI_MTLS_URL=https://qe.example.com:4441 \
DEVICE_SLOT=room-101 DEVICE_DATA_DIR=/var/lib/qe-agent \
./target/debug/qe-room-agent --once

# 兼容 bearer 模式
REPORT_MODE=bearer \
MACHINE_API_BASE=http://127.0.0.1:8080/api/v1 \
MACHINE_AGENT_TOKEN=<fleet-token> DEVICE_SLOT=room-101 \
./target/debug/qe-room-agent --once

# daemon（systemd 用；不要在前台验证时乱启动常驻循环）
DEVICE_PKI_MTLS_URL=https://qe.example.com:4441 \
DEVICE_SLOT=room-101 DEVICE_DATA_DIR=/var/lib/qe-agent \
./target/debug/qe-room-agent
```

`--print` 的 JSON 固定写 stdout；日志固定写 stderr，因此
`... --print | jq .` 不会被日志污染。

`sections.agent.tools` 是排查能力采集问题的第一入口：哪一项缺失，看对应
`error` 是「启动失败 / 超时 / 退出码 / 不是合法 JSON」即可；fast 档里看到
`{"skipped": true}` 属于预期（该档不做能力采集）。

---

## 4. 配置

优先级：**显式 CLI（`--interval` / `--device-slot`）> 环境变量 > 内置默认值**。
空白环境变量视同未设置（systemd `Environment=FOO=` 不会静默覆盖默认值）。

| 环境变量 | 默认 | 说明 |
| --- | --- | --- |
| `DEVICE_SLOT` | **必填** | 设备槽位，1–128 字符（也可用 `--device-slot`） |
| `ROOM_AGENT_TOOL_TIMEOUT_SECS` | `45` | 外部命令硬超时（进程内采集不受此限），1..=300 |
| `ROOM_AGENT_DIAGNOSE_INTERVAL_SECS` | `60` | diagnose 结果缓存/刷新间隔，0 = 每轮；重命令不占每轮周期 |
| `ROOM_AGENT_SECURITY_INTERVAL_SECS` | `300` | qeaudit 独立调度间隔（刷新后即时回传 full 快照），0 = 跟随 full 档 |
| `PLATFORM_HEALTH_BIN` | `platform-health` | 可执行文件路径/命令名 |
| `PLATFORM_HEALTH_ARGS` | `collect --processes 10` | 按空白切分的参数 |
| `PLATFORM_DIAGNOSE_BIN` | `platform-diagnose` | 同上 |
| `PLATFORM_DIAGNOSE_ARGS` | `-s generic -d 3 --json` | 同上 |
| `QEAUDIT_BIN` | `qeaudit` | 外部二进制路径；**显式设置即回退子进程模式**（默认进程内库直调） |
| `QEAUDIT_ARGS` | `scan --format json` | 同上（定制后走外部命令，可借此缩小检查范围） |
| `DOCKER_BIN` | `docker` | CLI 回退路径（进程内 qe-container 失败时用；参数固定 `ps --format '{{json .}}'`） |
| `VIRSH_BIN` | `virsh` | CLI 回退路径（进程内 virt-libvirt 失败时用；参数固定 `list --all`） |
| `ROOM_AGENT_DISABLE_HEALTH` | `false` | 关闭 platform-health 采集 |
| `ROOM_AGENT_DISABLE_DIAGNOSE` | `false` | 关闭 platform-diagnose 采集 |
| `ROOM_AGENT_DISABLE_SECURITY` | `false` | 关闭 qeaudit 安全扫描 |
| `ROOM_AGENT_DISABLE_DOCKER` | `false` | 关闭 docker 容器列表 |
| `ROOM_AGENT_DISABLE_VMS` | `false` | 关闭 virsh 虚拟机列表 |
| `REPORT_MODE` | `mtls` | 上报模式：`mtls`（默认）/ `bearer`（兼容旧部署） |
| `DEVICE_PKI_MTLS_URL` | **mTLS 模式必填** | device-pki mTLS 端点，如 `https://node:4441` |
| `DEVICE_DATA_DIR` | `./data` | 数据目录；设备证书在 `<data>/creds/<slot>` |
| `ROOM_AGENT_FAST_INTERVAL_SECS` | `10` | fast 档间隔，5..=60（远端策略可覆盖） |
| `ROOM_AGENT_FULL_INTERVAL_SECS` | `300` | full 档间隔，60..=3600（远端策略可覆盖） |
| `MACHINE_REPORT_INTERVAL_SECS` | — | 旧变量，等价于 `ROOM_AGENT_FAST_INTERVAL_SECS` |
| `MACHINE_API_BASE` | `http://127.0.0.1:8080/api/v1` | 仅 bearer 模式：qe-api 基础地址 |
| `MACHINE_AGENT_TOKEN` | bearer 模式必填 | 仅 bearer 模式：fleet token |

> 优先级：**远端策略 > 显式 CLI/env > 内置默认**。远端拉取失败只 `warn!`，
> 继续用本地的 env/默认值，绝不阻塞上报。

布尔值接受 `1/true/yes/on/y` 与 `0/false/no/off/n`（大小写不敏感）。

| CLI 参数 | 说明 |
| --- | --- |
| `--once` | 采集/上报**一份 full** 后退出；成功 0，失败 1，配置/参数错误 2 |
| `--print` | 把 payload 打印到 stdout，不发送（daemon 下 fast/full 分别打印） |
| `--fast-interval <秒>` | 覆盖 fast 档间隔（5..=60） |
| `--full-interval <秒>` | 覆盖 full 档间隔（60..=3600） |
| `--interval <秒>` | 旧参数，等价于 `--fast-interval` |
| `--report-mode <模式>` | `mtls`（默认）/ `bearer` |
| `--device-slot <槽位>` | 覆盖 `DEVICE_SLOT` |
| `--data-dir <目录>` | 覆盖 `DEVICE_DATA_DIR` |
| `-h, --help` | 打印用法 |

### 指向外部工具

platform-health / platform-diagnose 两个 sibling 项目默认不在 `PATH` 里；
生产建议软链/拷贝到 `/opt/qe/bin/`，开发机可直接用 `target/release/` 绝对路径。
qeaudit / 容器 / 虚机默认进程内直调，无需配置；只有要验证 CLI 回退路径时
才需要 `QEAUDIT_BIN` / `DOCKER_BIN` / `VIRSH_BIN`：

```bash
PLATFORM_HEALTH_BIN=/home/qingzhenzi/Projects/QEStudio/common/platform-health/target/release/platform-health
PLATFORM_DIAGNOSE_BIN=/home/qingzhenzi/Projects/QEStudio/common/platform-diagnose/target/release/platform-diagnose
QEAUDIT_BIN=/home/qingzhenzi/Projects/QEStudio/room_agent/target/release/qeaudit
DOCKER_BIN=/usr/bin/docker
VIRSH_BIN=/usr/bin/virsh
```

小技巧：

- 外部模式下部分机器的 `qeaudit scan` 较慢（本机实测 ~22s，网络/进程检查占大头）。
  可按需调大 `ROOM_AGENT_TOOL_TIMEOUT_SECS=40`，或用 `QEAUDIT_ARGS` 缩小范围（仅外部模式生效），
  例如 `QEAUDIT_ARGS="scan --format json --fail-on never --only grub,kernel,space"`；
  进程内模式走默认全量扫描，结果由 `ROOM_AGENT_SECURITY_INTERVAL_SECS` 缓存复用；
- 只想要内置指标时，把五个 `ROOM_AGENT_DISABLE_*` 全打开，payload 会很小、采集一次约 0.5–1s。

---

## 5. 健康结论

1. **优先采纳 platform-health**：其 JSON 里存在 `health.status` 时，直接采用
   `health.status/score/issues`（issues 转成契约形状 `severity/code/message/detail`）；
2. **兜底内置阈值**（platform-health 不可用时）：

| 检查项 | warning | critical |
| --- | --- | --- |
| CPU 使用率 | ≥ 90% | ≥ 97% |
| 内存使用率 | ≥ 90% | ≥ 97% |
| 根分区（缺失时取最满挂载点）使用率 | ≥ 90% | ≥ 95% |
| 最高温度 | ≥ 85°C | ≥ 95°C |
| 每核 1 分钟 load（load1 / 逻辑核心数） | ≥ 1.5 | ≥ 3.0 |

评分规则：100 分起，warning 扣 10、critical 扣 30、info 不扣，下限 0；
任一 critical → `critical`，否则任一 warning → `warning`，否则 `ok`。

---

## 6. payload 结构摘要

```json
{
  "schema": 1,
  "device_slot": "room-101",
  "agent_version": "0.1.0",
  "collected_at": "2026-10-31T14:53:01Z",
  "hostname": "room-101-node",
  "os_name": "CachyOS Linux rolling",
  "kernel_version": "6.18.42-1-cachyos-lts",
  "health": { "status": "ok", "score": 96, "issues": [] },
  "metrics": {
    "cpu_usage_percent": 14.7,
    "memory_usage_percent": 42.1,
    "disk_usage_percent": 61.2,
    "uptime_secs": 3280
  },
  "tier": "full",
  "sections": {
    "platform_health": { "...": "platform-health 原样 JSON" },
    "diagnose": { "...": "platform-diagnose 原样 JSON" },
    "security": { "summary": { "pass": 39, "warn": 4, "fail": 1 }, "raw": { "...": "qeaudit 原样 JSON" } },
    "containers": [ { "name": "py-api", "image": "...", "state": "running", "status": "Up 2 hours", "id": "..." } ],
    "vms": [ { "name": "debian12", "state": "running", "id": 1 } ],
    "disks": [ { "mount_point": "/", "filesystem": "/dev/nvme0n1p6", "usage_percent": 61.2 } ],
    "network": [ { "interface": "wlan0", "rx_bytes": 1, "tx_bytes": 2 } ],
    "thermal": [ { "sensor": "nvme/temp2", "temp_c": 86.85 } ],
    "agent": {
      "agent_version": "0.1.0",
      "tier": "full",
      "report_mode": "mtls",
      "interval_secs": 300,
      "fast_interval_secs": 10,
      "full_interval_secs": 300,
      "builtin_collect_duration_ms": 541,
      "tools": { "platform_health": { "available": true, "duration_ms": 2032 } },
      "host": { "machine_id": "...", "cpu_model": "...", "logical_cores": 14, "load1": 1.92 }
    }
  }
}
```

> 服务端限制请求体 ≤ 1 MiB；本机全量（含 platform-health 20KB 左右）约 50–70KB。

---

## 7. systemd

示例 unit 见 [`deploy/room-agent.service.example`](deploy/room-agent.service.example)：

```bash
sudo install -d -m 0755 /etc/systemd/system
sudo install -m 0644 deploy/room-agent.service.example /etc/systemd/system/qe-room-agent.service
sudo install -m 0600 /dev/null /etc/qe-room-agent.env
sudo tee /etc/qe-room-agent.env >/dev/null <<'EOF'
REPORT_MODE=mtls
DEVICE_PKI_MTLS_URL=https://qe.example.com:4441
DEVICE_SLOT=room-101
DEVICE_DATA_DIR=/var/lib/qe-agent
ROOM_AGENT_FAST_INTERVAL_SECS=10
ROOM_AGENT_FULL_INTERVAL_SECS=300
PLATFORM_HEALTH_BIN=/opt/qe/bin/platform-health
PLATFORM_DIAGNOSE_BIN=/opt/qe/bin/platform-diagnose
EOF
sudo systemctl daemon-reload
sudo systemctl enable --now qe-room-agent
journalctl -u qe-room-agent -f
```

`--print` 不需要证书/token。mTLS 模式下证书由 `qe-agent enroll` 写入
`DEVICE_DATA_DIR`，unit 只需 `EnvironmentFile` 注入端点与槽位；
bearer 模式仍用 `EnvironmentFile` 注入 token，避免 token 出现在
`systemctl cat` / 进程列表里。

---

## 8. 安全说明

- **默认 mTLS（v1.1）**：每台机器持有自己的设备证书（`qe-agent enroll` 签发），
  服务端 device-pki 从证书 CN/serial 解析身份；证书文件 `0600`，私钥不出机器。
- **bearer 是兼容路径**：`REPORT_MODE=bearer` 时每台机器持有同一个
  `MACHINE_AGENT_TOKEN`，仅适合内网/过渡期；token 通过 `0600` 的 EnvironmentFile 下发。
  上报走 HTTP 时 token 会明文传输，**生产必须放到 HTTPS 反向代理/内网专线之后**。
- agent 采集 docker/virsh/温度时通常需要 root（docker.sock、`qemu:///system`、
  部分 hwmon）；只采内置指标时可降权为专用用户并加入 `docker` 组，
  并用 `ROOM_AGENT_DISABLE_*` 关闭用不到的工具（最小权限）。
- agent 不写入磁盘、不做持久化队列；SIGTERM 由 systemd 直接终止，重启后重新采集。
- mTLS 复用了 `qe-device-pki-client` 的信任配置：设备证书作为客户端身份，
  `ca-chain.pem` 作为服务端校验根，与 `qe-node-agent` 完全一致。

---

## 9. 测试

```bash
cargo build -p qe-room-agent
cargo test -p qe-room-agent
cargo clippy -p qe-room-agent --all-targets -- -D warnings
```

测试覆盖：`/proc`/`df`/`hwmon` 解析、CPU 使用率差分、内存/磁盘/温度/load 阈值、
platform-health 健康采纳与回退、外部命令失败/超时/非 0 退出码容错、
docker/virsh/qeaudit JSON 解析（含进程内 → CLI 回退的键形映射）、配置校验
（两档间隔/tier/优先级）、mTLS 凭证加载与客户端构建、远端策略解析与应用
（非法值拒绝）、fast/full payload 形状（tier、skipped 工具）、HTTP 上报
（本地 mock server 校验 Bearer/body/202/非 2xx），以及二进制级
`--once --print` 契约 JSON / 退出码集成测试。

---

## 10. 已知限制

- `sections.vms[]` 只含 `name/state/id`：避免对每台 VM 额外跑 `virsh dominfo`
  拉 `vcpus/memory_bytes`（契约里这些字段可选；需要时后续加 `virsh dumpxml` 解析）；
- `sections.disks` 来自 `df -B1`，btrfs 子卷会重复出现；`metrics.disk_usage_percent`
  固定取根分区；
- 单轮最坏耗时约 `内置 ~0.5s + 外部工具超时`（仅 full 档；进程内 qeaudit/容器/虚机
  采集通常远快于外部命令，慢点主要在 platform-health / platform-diagnose）；
  调大工具超时/机器很慢时，full 单轮可能超过 full 间隔（daemon 串行执行，
  下轮从当前时刻重新起算，不突发补跑）；
- fast 与 full 共用主循环：full 采集期间不会并发跑 fast，fast 快照会顺延；
- 不做本地持久化队列/指数退避：daemon 上报失败只 warn 并等下一轮；
- 不处理 SIGTERM/SIGINT：由 systemd 管理生命周期。
