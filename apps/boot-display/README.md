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

| template   | 用途                                               |
| ---------- | -------------------------------------------------- |
| `boot`     | 启动 logo + 文案                                   |
| `progress` | 资源下载/合成进度（可确定进度或不确定条）          |
| `idle`     | 待机时钟画面                                       |
| `message`  | 纯文本告示（title / body / lines[]）               |
| `enroll`   | 注册流程状态（pending / failed / ok）              |

模板与参数完全由 agent 决定（`POST /v1/display` 推送，见
[`docs/boot-pipeline.md`](../../docs/boot-pipeline.md)），本应用只渲染。

## IPC 协议

前端每秒调用本应用 Rust 侧 `display_state`（内部请求 agent `GET /v1/state`
并取 `display` 字段；webview 不接触 socket），响应：

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

## 安装与自启动

agent 侧用环境变量 `QE_BOOT_DISPLAY_BIN`（或 `<data_dir>/agent-config.json` 的
`boot.display.bin`）定位二进制；找不到时 display 阶段 `skipped`，不影响 daemon
启动。推荐用一键脚本（workspace 根目录，root）：

```bash
pnpm tauri build                     # 产物：src-tauri/target/release/boot-display
sudo bash scripts/install-agent.sh   # 安装 qe-agent + boot-display + systemd
```

脚本优先安装预构建的 `src-tauri/target/release/boot-display`（缺失时回退 debug），
安装到 `/usr/local/bin/boot-display`，并在 `/etc/qe-agent.env` 写入
`QE_BOOT_DISPLAY_BIN=/usr/local/bin/boot-display`；缺少预构建产物时给出构建提示。
自启动由 `qe-agent` 负责（spawn + supervise + 崩溃退避），本应用不需要独立
systemd user unit。

手动安装等价于：

```bash
sudo install -m 0755 src-tauri/target/release/boot-display /usr/local/bin/boot-display
echo 'QE_BOOT_DISPLAY_BIN=/usr/local/bin/boot-display' | sudo tee -a /etc/qe-agent.env
sudo systemctl restart qe-agent
```

## TODO

- [x] `qe-agent` 侧展示层路由：`POST /v1/display` 推送 + `/v1/state.display` 轮询
- [x] agent 侧 spawn + supervise + 崩溃指数退避重启（`apps/qe-agent/src/boot/display.rs`）
- [x] `message` / `enroll` 场景模板（见 `docs/boot-pipeline.md`）
- [ ] 播放资源清单与本地缓存（策略对齐 agent-client 的 update 机制）
