# qe-agent 本机 IPC（GUI ↔ daemon）

> 状态：**P3 已完成**（daemon 侧 IPC + agent-client 接线均验收）。
> 实现代码：`crates/agent/src/{ipc.rs,logging.rs}`；GUI 侧 `agent-client/src-tauri/src/ipc.rs` +
> `src/lib/ipc.ts` / `src/stores/agent.ts`。协议变更时同步更新两侧。
>
> 已验收：daemon 单测（含 UDS 集成测试）、agent-client `pnpm check`（30 tests）与
> `pnpm check:rust`（21 tests）、真实 `qe-agent serve` 联调 status/logs/policy/enroll。

## 1. 为什么是两个进程

- `qe-agent` daemon 可能以 root（或专用用户）常驻，GUI 以登录用户运行；
- GUI 默认隐藏（托盘），崩溃/退出不应影响上报与镜像任务；
- 私钥只落在 daemon 的数据目录，GUI 永远不读证书/私钥；
- 房间机器可能是无头环境，daemon 必须能独立运行。

因此 `agent-client` 只做控制面：展示状态、触发注册、查看日志、修改少量本地偏好。

## 2. 进程形态

单 systemd 服务、单进程同时跑两个角色：

```bash
qe-agent run --role both --ipc-socket /run/qe-agent/agent.sock
```

- `--role node` / `--role machine` 仍可单独运行（有各自的 `--ipc-socket`，但推荐 `both`）；
- `--role both` 里 machine 跑在独立线程、node 跑在主线程（tokio），共用同一张证书与数据目录；
- `--ipc-socket` 只在守护模式生效；`--once/--print` 不启动 IPC（进程会退出）。

## 3. 传输

- Linux / macOS：Unix domain socket，systemd 示例为 `/run/qe-agent/agent.sock`
  （`RuntimeDirectory=qe-agent`，socket 权限 `0660`，属组 `qe-agent`）；
- GUI 用户需加入 `qe-agent` 组；也可用环境变量 `QE_AGENT_IPC_SOCKET` 覆盖路径（开发用）；
- 协议：HTTP/1.1 over UDS，JSON body；**不监听 TCP**；
- GUI 侧由 Tauri Rust 层连接 UDS，再通过 `#[tauri::command]` 暴露给 Vue（webview 不直接连 socket）。

Windows 第一版不做（Linux-first）；后续可用 named pipe + 同一套 JSON 协议。

## 4. 端点（已实现）

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/v1/healthz` | `{"ok":true}`，用于 GUI 判断 daemon 是否在跑 |
| GET | `/v1/status` | 守护进程与身份状态（状态页/托盘 tooltip） |
| GET | `/v1/policy` | 当前采集间隔（本地解析值；远端策略暂未回读） |
| GET | `/v1/logs?limit=` | 最近日志（内存环形缓冲，最多 2000 行） |
| POST | `/v1/enroll` | 用注册链接注册（阻塞到成功或 approval pending） |

后续（P3.5/P4）：`GET /v1/transfers`（image.pull 任务）、`GET /v1/policy` 回读远端 revision、
日志结构化（level/target/ts）、托盘/深链直接触发 enroll。

### GET /v1/status

```json
{
  "version": "0.1.0",
  "roles": ["node", "machine"],
  "report_mode": "mtls",
  "enrolled": true,
  "device_slot": "room-101",
  "serial": "4f40ccef…",
  "mtls_endpoint": "https://node:4441",
  "data_dir": "/var/lib/qe-agent",
  "socket": "/run/qe-agent/agent.sock",
  "started_at": 1761962400,
  "uptime_secs": 3600,
  "intervals": { "fast_interval_secs": 10, "full_interval_secs": 300 }
}
```

未注册时 `enrolled=false`、`serial=null`；`intervals` 在仅 node 角色时为 `null`。

### GET /v1/logs

```json
{ "items": ["2026-11-01T03:19:22Z  INFO qe_agent::run: 上报成功 round=1 tier=\"fast\""] }
```

MVP 返回 tracing 格式化后的整行文本；等 GUI 日志页稳定后再拆成
`{ts, level, target, message}`。

### POST /v1/enroll

```json
{ "link": "https://…/enroll?token=…&slot=…", "trust": true, "wait_secs": 300 }
```

- 响应：`{"status":"issued","device_slot":"…","serial":"…","mtls_endpoint":"…"}` 或
  `{"status":"pending","enroll_id":"…"}`；
- `trust=true` 表示用户已确认 TOFU 指纹；不传时由 device-pki-client 走交互确认（在无 TTY 的
  daemon 里会失败，GUI 应显式传 `true` 并在界面上展示指纹核对）；
- 同一时间只允许一个注册请求（409）；
- **热加载限制**：注册写盘后 `/v1/status` 立即反映；但已启动的 node/machine 角色在重启前
  仍使用旧凭证（后续 P4 做热加载）。

### GET /v1/policy

```json
{
  "source": "local",
  "revision": null,
  "policy": {
    "telemetry_fast_interval_secs": 10,
    "telemetry_full_interval_secs": 300
  }
}
```

当前只回显 daemon 解析到的本地/远端启动值；远端 revision 与镜像策略回读留到 P4。
GUI 只读展示（写策略仍走管理端 `/admin/agent-configs`）。

## 5. 安全

- socket 只允许本机、文件权限 `0660`；不做网络监听；
- 不通过 IPC 传递私钥/证书内容；`status` 只回显 slot/serial/endpoint；
- `enroll` 的链接含一次性 token：GUI 内存中展示后不落盘；daemon 只把结果落
  `<data_dir>/creds/`；
- 不在 IPC 里提供关机端点；停止 daemon 走 systemd。

## 6. 状态与剩余项

已完成（P3）：

- daemon：`--ipc-socket` / `qe-agent serve`（调试只用 IPC）/ `--role both`；
- GUI：status/logs/policy/enroll 全部真实数据；断连显示 socket + 启动引导；托盘 Open Logs；
- 验证：`pnpm check`（30 tests）、`pnpm check:rust`（21 tests）、真实 daemon 联调。

剩余（P4 / 后续）：

1. 打包与安装脚本：`pnpm tauri build` 产物验证、`qe-agent` 组/socket 权限、GUI 自启安装；
   （systemd 单元与手工安装步骤见 [`agent-deploy.md`](agent-deploy.md)）
2. enroll 后已启动角色的热加载（当前需重启）；
3. `/v1/policy` 回读远端 revision、`/v1/transfers`、日志结构化；
4. Windows named pipe（Linux-first 之后再评估）。
