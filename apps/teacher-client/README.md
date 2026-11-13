# QEStudio Agent Client

`qe-agent`（QEStudio 机器 agent daemon）的桌面 GUI 外壳，基于 Tauri 2 + Vue 3 + TypeScript + Vite + Tailwind CSS 4 + Pinia。

当前阶段为 **P3：GUI 已接入 qe-agent 本机 IPC**（HTTP/1.1 over Unix domain socket）。状态、日志、注册请求都走真实 daemon；daemon 未运行时 UI 显示「未连接」、socket 路径与启动引导，不会抛未捕获异常。

## 当前行为

- **默认隐藏启动**：`tauri.conf.json` 主窗口 `visible: false`，且 window-state 插件不恢复 `VISIBLE`；每次启动只出现托盘图标，不弹窗口。
- **托盘常驻**：托盘 tooltip 与菜单为 `QEStudio Agent`，菜单项 `Open` / `Open Logs` / `Quit`；左键点击托盘显示窗口；关闭窗口只是隐藏回托盘，退出请用托盘 `Quit`。
- **单实例**：再次启动不会产生第二个进程，而是显示 / 聚焦已有窗口；`qestudio://` 参数以 `deep-link` 事件转发给前端。
- **本机 IPC 页面**：
  - `/` 状态页：2s 轮询 `GET /v1/status`，展示注册状态、服务端（mTLS endpoint）、角色、报告模式、设备槽位 / 序列号、daemon 版本、运行时长、socket、数据目录、启动方式与采集策略；daemon 不在时显示「未连接 qe-agent daemon」+ socket + 启动命令（可复制）。
  - `/enroll` 注册页：粘贴 `https://…/enroll?token=…` 链接，调用 `POST /v1/enroll`；分别展示已签发（slot/serial/mTLS endpoint）、等待审批（enroll_id）、失败（daemon 错误原文）三种结果，成功后刷新 status。监听 `deep-link` 事件并自动填入链接。
  - `/logs` 日志页：进入即拉 `GET /v1/logs?limit=200`，每 2s 全量刷新；解析 tracing 行的 `时间 / LEVEL / target: message`，WARN/ERROR 着色；可按行查看原始日志。
  - `/settings` 设置页：主题、界面语言、开机自启、隐藏启动偏好；另展示只读的 daemon 信息（角色 / 服务端 / socket / 报告模式 / 采集间隔 / 策略来源）。
- **保留的工作台外壳**：无边框标题栏 + 可关闭多标签页 + 底部状态栏（连接状态 / socket / 角色 / 版本）+ 通用 `ui/` 组件。

## 本机 IPC

- socket 路径：环境变量 `QE_AGENT_IPC_SOCKET`，默认 `/run/qe-agent/agent.sock`；
- 传输：HTTP/1.1 over Unix domain socket，JSON；webview 不直接连 socket，统一由 Rust 层请求后再经 `#[tauri::command]` 暴露；
- 错误会带上可读信息（socket 不存在 / 权限不足 / 超时 / HTTP 状态 + daemon error message），UI 据此判断「daemon 未运行」。

| Tauri command | daemon 端点 | 用途 |
| --- | --- | --- |
| `agent_ipc_status` | `GET /v1/status` | 状态页、标题栏、状态栏 |
| `agent_ipc_logs` | `GET /v1/logs?limit=N` | 日志页（limit 1..2000，默认 200） |
| `agent_ipc_policy` | `GET /v1/policy` | 设置 / 状态页采集策略（只读） |
| `agent_ipc_enroll` | `POST /v1/enroll` | 注册页（`trust=true`，默认 `wait_secs=300`） |
| `agent_ipc_endpoint` | — | 展示当前 socket 路径（支持浏览器预览回退） |

开发联调：

```sh
# 1) 只起 IPC 的调试 daemon（agent 工作区）
cd /home/qingzhenzi/Projects/QEStudio/room_agent
cargo build -p qe-agent
./target/debug/qe-agent serve --ipc-socket /tmp/qe-agent-dev.sock

# 2) GUI 通过环境变量指向该 socket
cd /home/qingzhenzi/Projects/QEStudio/room_agent/apps/agent-client
QE_AGENT_IPC_SOCKET=/tmp/qe-agent-dev.sock pnpm tauri dev
```

真实守护由 systemd 启动：`qe-agent serve --role both --ipc-socket /run/qe-agent/agent.sock`。GUI 用户需在 `qe-agent` 组内（socket 权限 0660），否则状态页会显示权限不足的提示。

## 目录

```text
src/
├── components/shell/       标题栏、标签页、状态栏、Agent 导航、DaemonOfflinePanel、Toast
├── components/ui/          通用 UI 组件（对话框、Badge、开关等）
├── layouts/WorkbenchLayout.vue  工作台外壳（全局 status 轮询、深链接 / 菜单事件）
├── views/                  Status / Enroll / Logs / Settings 页面
├── stores/                 agent（IPC 状态/日志/注册）、settings、tabs、ui
├── lib/ipc.ts              invoke 适配层（daemon 请求类型 + 浏览器回退）
├── lib/desktop.ts          autostart、深链接、native-menu 桥
├── i18n/                   zh-CN（中文 key）+ en-US 翻译
├── router/                 hash 路由：/ /enroll /logs /settings
└── assets/main.css         主题与工具类
src-tauri/
├── src/ipc.rs              UDS HTTP 客户端 + `agent_ipc_*` 命令 + mock UDS 单测
├── src/tray.rs             托盘图标与 Open / Open Logs / Quit 菜单
├── src/windowing.rs        显示 / 聚焦主窗口（托盘、单实例共用）
├── src/lib.rs              Tauri Builder：插件、单实例、深链接、窗口行为、命令注册
├── tauri.conf.json         productName / identifier / 窗口 visible:false
└── capabilities/           主窗口 capability
```

## 安装/打包（2026-11-07 完成）

### 首次启动初始化动画（推荐首次部署）

首次启动 agent-client 时（本地无 `qe-agent-client.onboarded.v1` 标记）会**自动显示窗口**并播放
全屏初始化动画：

1. 逐项检测（带进度条/转圈）：Linux systemd、pkexec、qe-agent 安装、systemd 服务、qe-agent 组；
2. 未安装/未运行 → 显示安装步骤：选择 qe-agent 二进制 → **pkexec 一键安装并启动 daemon**
   （期间显示安装进度/输出）；没有 pkexec 时给出等价 sudo 命令；
3. 已运行但未注册 → 在动画内粘贴注册链接完成 `POST /v1/enroll`；
4. 已注册且服务正常 → 短暂成功动画后自动进入状态页，并写入 `onboarded` 标记（之后启动恢复托盘隐藏）。

初始化是**必须完成**的：首次启动不提供“稍后设置/跳过”，完成安装+注册后才会进入状态页；
安装完成后可在断连面板点「初始化向导」重新打开同一引导页（也会在 `/setup` 路由保留诊断/修复入口）。

> 初始化安装同样通过 **pkexec** 调用应用内置的 `agent-setup.sh`（嵌入二进制；安装时写到
> 临时目录，运行结束删除）→ 安装 systemd 单元/env、可选启动、把当前用户加入 `qe-agent` 组。
> 需要 PolicyKit（pkexec）与 systemd；GUI 只拿到 pkexec 授权，不保存 root 密码。

### 手动安装/修复（可选）

桌面端也保留 `/setup` 诊断页（断连面板「初始化向导」进入），无需离开 GUI：

1. **环境检查**：systemd / pkexec / qe-agent 二进制 / systemd 单元 / 服务状态 / qe-agent 组；
2. **选择 qe-agent 二进制**（默认探测 `/usr/local/bin/qe-agent`，也可填构建产物路径）；
3. 点「安装并启动 daemon」→ 通过 **pkexec** 调用应用内置的 `agent-setup.sh`
   （嵌入二进制；安装时写到临时目录，运行结束删除）→ 安装 systemd 单元/env、可选启动、
   把当前用户加入 `qe-agent` 组；
4. 「启动/重启服务」按钮；随后「下一步：注册设备」跳到 `/enroll` 粘贴注册链接；
5. 失败时显示 pkexec/脚本 stdout+stderr；没有 pkexec 时页面给出等价 sudo 命令。
   安装脚本（用户级/整机）会把 `agent-setup.sh` + systemd/env 模板放到
   `~/.local/share/qe-agent-client/` 或 `/usr/share/qe-agent-client/`，供手动兜底。

> 应用内安装需要 PolicyKit（pkexec）与 systemd；GUI 只拿到 pkexec 授权，不保存 root 密码。

### 用户级安装（推荐给桌面用户，无需 root）

```bash
# 先在 agent-client 目录构建（或用 --gui-bin 指定已有二进制）
cd room_agent/apps/agent-client
pnpm install && pnpm tauri build --debug        # 产物 src-tauri/target/debug/qe-agent-client
bash scripts/install-agent-client.sh            # 安装到 ~/.local，并写登录自启
# 选项：--prefix DIR / --no-autostart / --add-group（sudo 加入 qe-agent 组）/ --dry-run
```

安装内容：
- `~/.local/bin/qe-agent-client`
- `~/.local/share/applications/qe-agent-client.desktop`（应用菜单）
- `~/.local/share/icons/hicolor/128x128/apps/qe-agent-client.png`
- `~/.config/autostart/qe-agent-client.desktop`（登录自启，默认开启）
- 检测 `qe-agent` 组：不在组内会打印 `sudo usermod -aG qe-agent $USER`（需重新登录）

### 整机安装（daemon + 展示层 + GUI + systemd，root）

```bash
# workspace 根目录
cargo build -p qe-agent --release
cd room_agent/apps/agent-client && pnpm tauri build --debug && cd -
sudo bash room_agent/scripts/install-agent.sh --start --gui-user "$USER"
# 可选：--gui-bin PATH / --no-gui / --no-boot-display
```

该脚本会安装 `/usr/local/bin/qe-agent`（systemd 单元 `/etc/systemd/system/qe-agent.service`、
`/etc/qe-agent.env`、RuntimeDirectory `/run/qe-agent` 0750 root:qe-agent）、boot-display（如有预构建）、
GUI 二进制/菜单/图标，并为 `--gui-user` 写登录自启、加入 `qe-agent` 组。
socket `/run/qe-agent/agent.sock` 由 agent 设为 0660，组用户可访问。

### 卸载

```bash
pkill -f qe-agent-client 2>/dev/null || true
rm -f ~/.local/bin/qe-agent-client \
      ~/.local/share/applications/qe-agent-client.desktop \
      ~/.config/autostart/qe-agent-client.desktop \
      ~/.local/share/icons/hicolor/128x128/apps/qe-agent-client.png
sudo systemctl disable --now qe-agent
sudo rm -f /etc/systemd/system/qe-agent.service /usr/local/bin/qe-agent-client
```

安装脚本可重复执行；已有 `/etc/qe-agent.env` 不会被覆盖。

## 开发

```sh
pnpm install
pnpm dev            # 浏览器预览（IPC 不可用，页面显示未连接 daemon）
pnpm tauri dev      # 桌面端；启动后从托盘打开窗口
```

桌面端启动后没有窗口属于预期：点击托盘图标，或再次启动（单实例）唤出窗口。

## 质量门禁

```sh
pnpm type-check     # vue-tsc --noEmit
pnpm lint           # oxlint src
pnpm test           # vitest run（单元测试）
pnpm build          # vue-tsc --noEmit && vite build
pnpm check          # version:check + lint + type-check + test + build
pnpm test:e2e       # Playwright 冒烟（需先安装浏览器）
pnpm check:rust     # cargo fmt --check + clippy -D warnings + cargo test
```

### Tests

- 前端单测：`agent` store（连上 / 断连 / 日志解析 / enroll issued|pending|failed / policy 回退）、`lib/ipc` 的链接校验与浏览器回退、tabs、i18n、StatusBadge、ConfirmDialog。
- Rust 单测：`src/ipc.rs` 用 mock UDS server 覆盖 status/logs/enroll 请求行与 JSON 解析、HTTP 错误消息、socket 不存在、读超时、非法响应。
- 真实 daemon 联调（需要先启动上面的 `qe-agent serve`）：

  ```sh
  QE_AGENT_IPC_SOCKET=/tmp/qe-agent-dev.sock \
    cargo test --manifest-path src-tauri/Cargo.toml --lib ipc::tests::real_daemon -- --ignored
  ```

- Playwright e2e 只覆盖浏览器预览下的断连 UI（状态页/日志页的未连接引导、注册页本地校验与浏览器回退错误、设置页 socket 展示），不依赖真实 daemon。
- `pnpm test:desktop`（`scripts/desktop-smoke.sh`）在 X11 下验证二进制启动存活且**没有可见窗口**（默认隐藏）。

## 产品重命名对照

| 位置 | 值 |
| --- | --- |
| `package.json` name | `qe-agent-client` |
| `src-tauri/Cargo.toml` package | `qe-agent-client` |
| Rust lib name | `qe_agent_client_lib`（`main.rs` 调用 `qe_agent_client_lib::run()`） |
| `tauri.conf.json` productName | `QEStudio Agent` |
| identifier | `org.qestudio.agent` |
| 主窗口 title | `QEStudio Agent` |
| 托盘文案 / tooltip | `Open / Open Logs / Quit QEStudio Agent` / `QEStudio Agent` |

## 后续 TODO

- ✅ 打包/安装：daemon systemd 单元 + GUI 菜单/登录自启 + `qe-agent` 组/socket 权限安装脚本已完成（见「安装/打包」；用户级 `scripts/install-agent-client.sh`、整机 `scripts/install-agent.sh --gui-user`）。
- daemon 热加载：注册成功后已启动的 node/machine 角色需重启才使用新凭证（见 ../docs/agent-ipc.md），GUI 目前只提示。
- P3.5/P4：`GET /v1/transfers`、远端 policy revision 回读、日志结构化（level/ts/target）、Windows named pipe。
- 清理 `docs/`（旧工作台文档与截图）与 `template/`（client-app 模板副本）；本阶段未动，避免与功能改造混在一起。
