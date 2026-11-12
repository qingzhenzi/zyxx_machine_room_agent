# platform-health

一次调用，拿到一台设备的**清单 + 健康结论**。Rust 库 + CLI，Linux / Windows / macOS 通用（Linux 覆盖最全）。

```rust
let snap = platform_health::snapshot();
println!("{} score {}", snap.health.status, snap.health.score);
```

```console
$ platform-health check
critical score  8  6 issue(s)
  [critical] thermal.temperature  k10temp Tctl  k10temp Tctl at 91°C
  [warning ] disk.usage           /             Disk / usage at 88%
```

一次快照采集 **20 个分段**：核心指标（CPU/内存/磁盘/网络/温度/进程）+ 可选清单域（显示器、电池、固件、RAM 槽位、Wi-Fi、时钟、GPU、SMART、容器、包、安全态势）。每个分段独立计时并记录失败原因（见 `sections`），便于线上定位"为什么这台机器这块是空的"。

## 为什么不用现成的

现成方案要么许可证有坑，要么只给指标、不给结论：

| 方案 | 许可证 | 问题 |
|---|---|---|
| Zabbix agent（7.0 起） | **AGPLv3** | 打包分发即触发开源义务 |
| Netdata | GPL-3.0 | 同上，且是"采集+展示"整套 |
| Grafana / Uptrace / OpenObserve | **AGPLv3** | 平台层，不适合嵌入 |
| osquery | Apache-2.0 / GPL-2.0 双许可 | 二进制几十 MB，指标是通用表，无健康判定 |
| Telegraf | MIT | Go 单二进制 100MB+，同样没有判定层 |
| sysinfo / procfs / os_info | MIT | 只有原始指标，需自己拼装（本 crate 干的活） |

结论：**采集用 sysinfo（MIT）+ 自研 sysfs/shell-out，判定自己写**。默认构建只依赖 `sysinfo`（MIT）和 `serde`（MIT/Apache-2.0），没有 GPL/AGPL 代码，可闭源分发、静态链接、塞进 Tauri 安装包。可选 feature 也只增 MIT/Apache-2.0 依赖（GPU），或 shell out 到独立进程（smartctl/docker 等，独立进程调用不传染本 crate）。

## Feature 开关

默认构建已含所有"读 sysfs/sysinfo"的免费域，零新依赖。需要额外依赖或外部工具的域按需开启：

| Feature | 默认 | 依赖/工具 | 采集内容 |
|---|---|---|---|
| `users` `display` `battery` `firmware` `ram` `edac` `wifi` `clock` `cpu-ext` | ✅ | 无（sysfs/sysinfo/`who`/`iw`） | 用户、显示器、电池、固件/SecureBoot/TPM、RAM 槽位、ECC、Wi-Fi、时钟偏移、CPU 缓存/虚拟化 |
| `gpu` | ❌ | `nvml-wrapper` (MIT/Apache) | NVIDIA 利用率/显存/温度/功耗/风扇 |
| `smart` | ❌ | `smartctl` | 磁盘 SMART 健康/磨损/重映射扇区 |
| `security` | ❌ | `ss`/`systemctl`/`getenforce` | 监听端口、自启项、MAC 状态、失败单元 |
| `containers` | ❌ | `docker`/`podman`/`virsh` | 容器/VM 数量 |
| `packages` | ❌ | `dpkg`/`rpm`/`pacman`/`apk` | 已装包数量、待定安全更新 |
| `full` | ❌ | 以上全部 | — |

```console
cargo build --features full      # 全开
cargo build                      # 只默认（免费域）
```

未开启或平台不支持的域，对应 section 记为 `disabled`/`unsupported`/`missing_tool`，字段为 `null`，**不影响其余采集**。

## 用法

### 库

```rust
use platform_health::{Options, Privacy, Thresholds, snapshot_with};

let opts = Options {
    privacy: Privacy::Redacted,       // 脱敏：主机名、MAC、序列号、exe 路径
    include_processes: true,          // 进程表，默认关闭
    top_processes: 10,
    include_optional: true,           // false = 只要核心指标（快照）
    thresholds: Thresholds::default(), // 阈值可整体替换
};
let snap = snapshot_with(&opts);
let json = serde_json::to_string(&snap)?;
```

采集**不会失败**：没有温度传感器 → `components: []`；虚拟机没有 DMI → `machine` 为 `null`；桌面无电池 → `battery.present=false`；未支持系统 → 字段稀疏但结构合法。

### CLI

```console
platform-health                     # 输出 JSON（默认 collect，含所有默认域）
platform-health --pretty            # 格式化
platform-health --redact            # 脱敏输出
platform-health --core-only         # 跳过可选清单域，只要核心指标
platform-health --processes 5       # 附带 top5 进程
platform-health --watch 30 -o /var/lib/qe/health.jsonl   # 每 30s 追加一行
platform-health check               # 人类可读结论，退出码 0/1/2
platform-health check --thresholds ops.json   # 用配置文件覆盖阈值
```

阈值外置（JSON，允许只写要改的字段）：

```json
{ "disk_usage_warn": 75, "memory_usage_crit": 90, "temperature_warn": 85, "clock_offset_warn_ms": 3000 }
```

`check` 退出码：`0` 健康、`1` 降级、`2` 严重，适合 cron / systemd timer / 告警脚本。

## 输出结构（v2，节选）

```json
{
  "schema": "platform-health/v2",
  "collected_at": "2026-10-30T14:41:02Z",
  "identity": { "fingerprint": "9f2c1ab34d5e6f70", "hostname": "ws-042" },
  "host": { "os_name": "CachyOS Linux", "kernel_version": "6.18.42", "arch": "x86_64", "uptime_secs": 183422 },
  "machine": { "vendor": "WOOKING", "model": "X15", "serial": "PF0ABCDE", "bios_version": "V1.67" },
  "sections": [
    { "name": "cpu", "status": "ok", "duration_ms": 0 },
    { "name": "ram", "status": "denied", "duration_ms": 0, "detail": "read SMBIOS table (run as root)" },
    { "name": "gpu", "status": "unsupported", "duration_ms": 0 }
  ],
  "cpu": { "brand": "AMD Ryzen 9 6900HX", "logical_cores": 14, "usage_percent": 8.0,
           "virtualization": { "supported": true, "running_hypervisor": false } },
  "cpu_ext": { "l1_kib": 64, "l2_kib": 512, "l3_kib": 16384, "stepping": "1", "flags": ["avx2","aes","svm"] },
  "memory": { "total_bytes": 15934545920, "usage_percent": 44.0 },
  "disks": [ { "mount_point": "/", "file_system": "btrfs", "kind": "ssd", "usage_percent": 84.6, "read_only": false } ],
  "networks": [ { "name": "wlan0", "mac": "bc:2b:02:**:**:**", "state": "up", "link_speed_mbps": null, "mtu": 1500 } ],
  "components": [ { "label": "k10temp Tctl", "temperature_c": 91.0, "critical_c": 100.0 } ],
  "displays": [ { "width": 2560, "height": 1440, "physical_width_mm": 340, "is_primary": true } ],
  "battery": { "charge_percent": 86.0, "status": "charging", "health_percent": 100.0, "technology": "Li-ion", "present": true },
  "firmware": { "bios_version": "V1.67", "firmware_type": "uefi", "secure_boot": false, "tpm_present": true, "tpm_version": "2.0", "disk_encryption": false },
  "wifi": [ { "interface": "wlan0", "ssid": "R", "signal_dbm": -28, "band": "2.4GHz", "channel": 1, "connected": true } ],
  "clock": { "synchronised": true, "method": "timedatectl" },
  "smart": [ { "device": "/dev/nvme0n1", "health": "PASSED", "temperature_c": 38.0, "percentage_used": 3.0 } ],
  "containers": { "docker_running": 1, "docker_images": 12 },
  "packages": { "manager": "pacman", "count": 1809 },
  "processes": null,
  "health": { "status": "critical", "score": 8, "issues": [ { "code": "thermal.temperature", "severity": "critical", "subject": "k10temp Tctl", "observed": 91.0, "threshold": 90.0, "unit": "°C" } ] }
}
```

要点：
- **磁盘按物理设备去重**：btrfs 子卷 `@rootfs`/`@home` 只报一条（优先 `/`），否则同一块盘算八条、健康分清零。
- **分段自证**：`sections` 记录每个域的状态与耗时。`denied`=权限不足（如 SMBIOS 需 root）、`missing_tool`=外部工具不在 PATH、`disabled`=feature 未开、`unsupported`=平台不支持。
- **指纹**：`identity.fingerprint` = UUID（优先）或主机名+MAC 的 FNV-1a 哈希。换网卡/改主机名会变新设备，重装系统不会——够做资产绑定，但**不是**密码学指纹。

## 健康规则

默认阈值（`Thresholds`，可整体替换）：

| code | 判定 | 警告 | 严重 |
|---|---|---|---|
| `cpu.usage` | CPU 使用率 | ≥ 85% | ≥ 95% |
| `cpu.load` | 1 分钟负载 / 逻辑核 | ≥ 1.5 | ≥ 3.0 |
| `memory.usage` | 内存使用率 | ≥ 85% | ≥ 95% |
| `swap.usage` | 交换分区使用率（未配置跳过） | ≥ 50% | ≥ 80% |
| `disk.usage` | 各挂载点使用率 | ≥ 85% | ≥ 95% |
| `disk.free` | 剩余空间绝对值（<4GiB 盘跳过） | < 5 GiB | < 1 GiB |
| `disk.read_only` | 文件系统被 remount 只读 | — | 是 |
| `disk.smart` | SMART 自评估失败 | — | FAILED |
| `disk.wear` | SSD 磨损（percentage_used） | ≥ 90% | — |
| `thermal.temperature` | 传感器温度（固件 critical 优先） | ≥ 80°C | ≥ 90°C |
| `gpu.temperature` | GPU 温度 | ≥ 85°C | ≥ 95°C |
| `battery.low` | 放电中电量过低 | ≤ 15% | — |
| `battery.health` | 电池健康度（满充/设计容量） | ≤ 60% | — |
| `memory.ecc` | EDAC/ECC 内存错误 | 可纠 > 1000 | 不可纠 > 0 |
| `clock.skew` | 与时间源偏移 | ≥ 5000 ms | — |
| `firmware.secure_boot` | Secure Boot 关闭（合规） | 信息 | — |
| `process.count` | 进程总数 | > 1000 | — |
| `process.zombie` | 僵尸进程 | > 10 | — |

评分：满分 100，严重 −30、警告 −10、信息 −2，下限 0。状态取所有问题中的最高级别（信息不降级）。每条 `Issue` 带 `code`/`subject`/`observed`/`threshold`/`unit`，前端按 `code` 做文案，不必解析英文 `message`。

## 隐私

- `Privacy::Full`（默认）：原样上报，适合内网受管设备。
- `Privacy::Redacted`：主机名/用户名→`device-<hash>`、MAC 保留厂商 OUI 其余打码、序列号/资产标签只留后 4 位、进程 exe 只留文件名、Wi-Fi SSID/BSSID 打码。**指纹仍按原始值计算**，脱敏前后同一台设备能对上。

出内网（遥测、工单附件）记得开 `--redact`。`security`/`users` 域较敏感，默认 feature 已含但可按部署关闭。

## 已知边界

- **Windows/macOS 覆盖较浅**：核心指标（CPU/内存/磁盘/网络/温度/进程）三平台都有；sysfs 类清单域（电池/固件/RAM 槽位/EDAC/Wi-Fi/SecureBoot）目前 Linux-only，非 Linux 返回 `unsupported` 并在 `sections` 标注。WinAPI/IOKit 接线可按需补。
- **GPU 仅 NVIDIA**：AMD/Intel 需另接 sysfs（`amdgpu-sysfs` 等），当前 `gpu` feature 走 nvml。
- **RAM 槽位需 root**：直接解析 `/sys/firmware/dmi/tables/DMI`，普通用户 `denied`（刻意不依赖 GPL 的 dmidecode）。
- **SMART 需 root**：smartctl 读盘需权限，否则该盘 `supported=false` + error 字符串。
- **网络只有累计字节**：速率需两次采样求差，交给调用方按 `--watch` 频率算。
- **虚拟机**里 DMI/温度/电池通常为空或占位，属正常降级。

## 与 QEStudio 集成

- **client-app / Tauri**：加依赖后包一层 `#[tauri::command] fn device_health() -> Snapshot`，前端按 `health.issues[].code` 渲染。桌面端建议 `--core-only` 或关掉 `security`/`containers` 等 shell-out 域，避免不必要的外部进程。
- **backend**：设备侧 `platform-health --watch 60 -o health.jsonl`，按 `identity.fingerprint` 归并、按 `health.status` 建告警；`sections` 可用来监控采集质量（哪台机器哪块采集失败率高）。

## 开发

```console
cargo test --features full     # 单元 + 集成测试（25 个）
cargo clippy --features full --all-targets -- -D warnings
cargo run --features full -- check
make check                     # fmt + clippy + test
```

## 许可证

MIT。依赖许可证见 [LICENSES.md](LICENSES.md)：`sysinfo` (MIT)、`serde`/`serde_json` (MIT OR Apache-2.0)、`nvml-wrapper` (MIT/Apache-2.0，可选)，**无 GPL/AGPL**。shell-out 工具（smartctl/dmidecode 等）为独立进程调用，不构成衍生作品。CI 建议加 `cargo deny check licenses`（配置见 [deny.toml](deny.toml)）。
