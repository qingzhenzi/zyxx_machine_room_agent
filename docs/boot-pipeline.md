# Boot 流水线设计

`qe-agent serve` 的阶段化初始化。目标是把 `crates/` 下的能力按顺序接进一条
可观测的启动流水线，每个阶段产出状态，IPC `/v1/state` 全程可查。

## 阶段与 crate 映射

| # | 阶段       | 使用的 crate                        | 失败行为           |
|---|-----------|-------------------------------------|--------------------|
| 1 | `ipc`      | （serve 内建）                      | 不适用（前置条件） |
| 2 | `firewall` | `qe-nftables`（nftables-rs 适配）   | failed，不阻塞     |
| 3 | `display`  | `apps/boot-display`（spawn 可选）   | skipped/failed     |
| 4 | `checks`   | `platform-health` + `qeaudit` + `platform-diagnose` | failed，不阻塞 |
| 5 | `enroll`   | `qe-device-pki-client`              | skipped/failed，不阻塞 |
| 6 | `services` | `virt-crate`（KVM 探测）+ `qe-container` + `qep2p` | failed，不阻塞 |

长驻服务（node agent / room agent）仍由 serve 原有路径启动，流水线只做
检测与就绪标记，两者解耦。

## 语义约定

* **配置不足**：无证书且无注册链接 → `enroll=skipped`，
  `config_sufficient=false`。依赖注册的步骤（下行通道等）不执行，
  其余初始化照常。后续 provisioning 写入 `QE_ENROLL_LINK` 环境变量或
  `<data_dir>/enroll-link` 文件即可。
* **注册失败**：`enroll=failed`，同样不阻塞。
* **overall**：全部阶段有终态且无 failed → `ready`；任一 failed →
  `degraded`；否则 `booting`。

## 配置

配置文件 `<data_dir>/agent-config.json` 的 `boot` 段（可省略，全默认）：

```json
{
  "boot": {
    "display": { "enabled": true, "bin": "/usr/bin/boot-display" },
    "checks_interval_secs": 600,
    "firewall_enabled": true
  }
}
```

环境变量覆盖（优先级更高）：

| 环境变量                       | 说明                                   |
|--------------------------------|----------------------------------------|
| `QE_BOOT_DISPLAY`              | `0`/`false` 关闭展示层                 |
| `QE_BOOT_DISPLAY_BIN`          | boot-display 二进制路径（默认查 PATH） |
| `QE_BOOT_CHECKS_INTERVAL_SECS` | 周期检查间隔（30~86400s，越界回退默认）|
| `QE_ENROLL_LINK`               | 注册链接（或写入 `<data_dir>/enroll-link`）|

## 周期检查

* 首次必检（流水线阶段 4）；之后 `boot/monitor.rs` 的线程按间隔复检，
  只更新 `/v1/state` 里 `checks` 阶段的状态，不重启流水线。
* 远端覆盖间隔：

  ```
  POST /v1/checks-interval   {"interval_secs": 300}
  → {"ok": true, "interval_secs": 300}
  ```

* **远端联动**：控制服务器通过下行 `config.update` 更新策略后，
  `qe-node-agent` 触发 `on_remote_policy_applied(full_interval_secs)` 钩子，
  serve 里把它接到 `boot.set_checks_interval` —— 服务端只需调
  `telemetry_full_interval_secs` 就同时控制遥测深度与本机复检节奏。
  另外遥测 full 档本身已周期性聚合 platform-health / diagnose / qeaudit，
  两条路径互补：`/v1/state` 面向本机 GUI，遥测面向控制服务器。

## boot-display supervise

agent 启动展示层后由专用 supervise 线程守护：每 5s 探活，崩溃按指数退避
重启（1s → 60s 封顶），恢复运行后退避自动重置。

## IPC

```
GET /v1/state
→ {
    "overall": "booting" | "ready" | "degraded",
    "config_sufficient": bool,
    "phases": [
      { "phase": "firewall", "status": "ok", "message": "...", "detail": {...} },
      ...
    ]
  }
```

`status`: `pending` | `running` | `ok` | `failed` | `skipped`。

## 新增 crate：qe-nftables

声明式防火墙适配：`FirewallSpec`（预设 + 端口规则）→ `nft -f -` 脚本，
整表（`inet qe_agent`）重建保证幂等。feature `json-backend` 可切换
libnftables JSON API（依赖系统库，默认关闭）。失败返回 `Report` 而非
panic——防火墙失败不应阻断启动。

## 代码位置

* 流水线：`apps/qe-agent/src/boot/`（`pipeline.rs` 编排，`state.rs` 共享状态）
* 防火墙：`crates/qe-nftables/`
* 展示层：`apps/boot-display/`（见其 README）
