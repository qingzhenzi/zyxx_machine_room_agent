# QEStudio Boot Display

全屏、由 `qe-agent` 驱动的展示层：设备首次启动画面、资源加载进度、待机画面。

## 定位

本应用是**纯哑显示层**：所有状态机与业务判断都在 `qe-agent` 里。agent 负责spawn、
supervise（崩溃自动重启）、通过 IPC 推送场景；本应用只渲染。

```
qe-agent (systemd 常驻，拥有决策权)
   │  spawn + supervise
   ▼
boot-display (本应用, 全屏置顶无边框)
   │  HTTP/1.1 over UDS: /run/qe-agent/agent.sock
   ▼
qe-agent ←→ agent-client (用户交互仍在 client)
```

## 场景

| scene      | 用途                                   |
| ---------- | -------------------------------------- |
| `boot`     | 启动 logo + 文案                       |
| `progress` | 资源下载/合成进度（可确定或不确定条） |
| `idle`     | 待机时钟画面                           |

## IPC 协议

前端每秒轮询 `GET /display/state`（经 Rust 侧转发，webview 不接触 socket），
响应：

```json
{
  "scene": "progress",
  "message": "正在下载资源",
  "detail": "3/10",
  "progress": 30
}
```

`progress` 为 `null` 时进度条呈不确定动画。agent 不可达时保持当前画面并重试，不黑屏。

Socket 发现：环境变量 `QE_AGENT_IPC_SOCKET`（开发覆盖）或默认
`/run/qe-agent/agent.sock`，与 agent-client 一致。

## 开发

```bash
pnpm install
pnpm tauri dev          # 全屏窗口，注意会抢占屏幕
pnpm tauri:x11          # 强制 X11 后端
pnpm check              # lint + type-check + test + build
pnpm check:rust         # fmt + clippy + cargo test
bash scripts/desktop-smoke.sh
```

与 agent-client（vite 端口 1420/1421）隔离，本应用使用 1430/1431。

## TODO

- [ ] `qe-agent` 侧实现 `/display/state` 路由（当前前端收到的是默认 boot 场景）
- [ ] agent 侧 spawn + supervise + 崩溃退避重启
- [ ] 播放资源清单与本地缓存（策略对齐 agent-client 的 update 机制）
- [ ] systemd user service 单元（自启动由 agent 链路负责，本应用不自启）
