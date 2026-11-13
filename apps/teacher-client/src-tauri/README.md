# Rust 侧（Tauri 2）

QEStudio Agent 的原生层：负责隐藏启动 + 托盘（关闭窗口隐藏回托盘、Quit 仅走托盘/菜单）、原生菜单、前端可调用的命令，以及 Linux Wayland 下的置顶桥接。P0.2 不接入 qe-agent IPC。前端（`../src`）只通过 `invoke` 与插件 API 与这里交互，浏览器预览不依赖本目录。

## 模块地图

```text
src-tauri/
├── src/
│   ├── main.rs                  可执行入口，调用 qe_agent_client_lib::run()
│   ├── lib.rs                   只做 Builder 装配：插件注册、invoke_handler、setup 编排
│   ├── ipc.rs                   qe-agent 本机 IPC 客户端（HTTP/1.1 over UDS）+ 命令 + mock UDS 单测
│   ├── error.rs                 统一错误类型与命令错误转换
│   ├── windowing.rs             主窗口显示 / 聚焦（单实例唤醒、启动时展示）
│   ├── tray.rs                  托盘图标与菜单
│   ├── menu.rs                  macOS 原生菜单（其他平台不注册）
│   ├── commands/
│   │   ├── platform.rs          platform_info、desktop_capabilities
│   │   ├── files.rs             save_text_file、read_import_file
│   │   ├── keychain.rs          keychain_get / keychain_set / keychain_delete
│   │   └── windows.rs           open_workspace_window、open_detached_window
│   └── desktop/
│       ├── mod.rs               会话检测（x11 / wayland / native）与能力探测
│       ├── pin.rs               置顶策略编排（原生 always_on_top vs Wayland 桥接）
│       └── kwin.rs              KDE KWin Wayland 置顶桥接（D-Bus 脚本，仅 Linux）
├── capabilities/default.json    窗口 capability 与权限
├── tauri.conf.json              主配置
├── tauri.update-http.conf.json  本地更新服务器（HTTP）测试覆盖
├── tauri.update-https.conf.json 本地更新服务器（自签 HTTPS）测试覆盖
├── Cargo.toml / Cargo.lock      依赖与版本（版本号需与 package.json、tauri.conf.json 一致）
├── rustfmt.toml                 格式化配置（`pnpm rust:fmt`）
├── build.rs                     构建脚本
└── icons/                       打包图标
```

约束：`lib.rs` 不承载业务逻辑，新增能力下沉到 `commands/` 或 `desktop/`；跨模块错误统一走 `error.rs`，便于前端只处理一种错误形态。

## 前端可调用的命令

命令名保持 `snake_case`；参数在 Rust 侧为 `snake_case`，前端 `invoke` 时使用对应的 `camelCase`。

| 命令 | 状态 | 用途 / 后续 |
| --- | --- | --- |
| `agent_ipc_status` | **P3 使用** | `GET /v1/status`：状态页 / 标题栏 / 状态栏 |
| `agent_ipc_logs` | **P3 使用** | `GET /v1/logs?limit=N`：日志页（1..2000，默认 200） |
| `agent_ipc_policy` | **P3 使用** | `GET /v1/policy`：采集策略只读展示 |
| `agent_ipc_enroll` | **P3 使用** | `POST /v1/enroll`：注册（trust=true，默认 wait_secs=300） |
| `agent_ipc_endpoint` | **P3 使用** | 返回当前 `QE_AGENT_IPC_SOCKET` / 默认 socket 路径 |
| `platform_info` | 未调用 | 应用版本、Tauri 版本、OS、架构；P3 可用于状态页信息 |
| `desktop_capabilities` | 未调用 | 会话类型与置顶能力探测（原独立窗口用） |
| `save_text_file` / `read_import_file` | 未调用 | 原报告导出 / 项目导入用，P3 可删除 |
| `keychain_get` / `keychain_set` / `keychain_delete` | 未调用 | 原 session 存储；P3 若需保存注册凭据可复用（service 已改为 `org.qestudio.agent`） |
| `open_workspace_window` / `open_detached_window` | 未调用 | 原多窗口功能，P0.2 已删除对应前端页面 |

P3 前端调用 Tauri 插件 API（autostart、deep-link、log、window）、`getVersion`，以及 `agent_ipc_*` 命令。IPC 约定：`QE_AGENT_IPC_SOCKET` 覆盖 socket 路径（默认 `/run/qe-agent/agent.sock`）；客户端在 Rust 侧用 `std::os::unix::net::UnixStream` 发 HTTP/1.1 请求并解析 JSON，错误统一转成人可读的字符串返回。协议见后端 `docs/agent-ipc.md`。其余（files/keychain/windows）命令暂时保留注册，后续清理。

命令清单以 `src/lib.rs` 的 `tauri::generate_handler!` 为准；前端不直接 `invoke` 的业务组件请通过 `src/lib` / `src/composables` 适配层调用。

## Capability 权限模型

`capabilities/default.json` 的 `windows` 只覆盖主窗口：

- `main`：主窗口（无边框标题栏，P0.2 默认隐藏、托盘唤起）

权限按「前端实际调用」授予：

- `core:default`：事件监听（`onResized` / `onFocusChanged` / 拖拽事件 / 深链接事件）、窗口状态读取等只读能力
- 显式窗口写操作：`allow-start-dragging`（`data-tauri-drag-region` 标题栏）、`allow-start-resize-dragging`（自绘缩放手柄）、`allow-minimize`、`allow-toggle-maximize`、`allow-close`；`allow-is-maximized` 已在 `core:default` 中，保留显式声明是为了让意图可读
- 插件默认权限：`opener:default`、`notification:default`、`updater:default`、`process:default`、`autostart:default`、`dialog:default`、`log:default`、`deep-link:default`
- `global-shortcut`：插件的 `default` 不授予任何权限（官方有意保守），因此额外显式声明 `allow-register` / `allow-unregister`，否则 `Cmd/Ctrl+Shift+K` 注册会被静默拒绝

新增窗口类型时，必须同时更新这里的 `windows` glob；新增插件调用时，优先使用插件 `default` 权限集，不足再补单项 `allow-*`。

## 额外配置

- `tauri.update-http.conf.json`：把 updater endpoint 指向本地假更新服务器 `http://127.0.0.1:8787/latest.json`，并开启 `dangerousInsecureTransportProtocol`（仅本地测试）
- `tauri.update-https.conf.json`：同上，但指向自签 HTTPS 的 `https://localhost:8787/latest.json`

两者都通过 `pnpm tauri build --config <文件>` 叠加到主配置上，服务器与证书流程见 `../tools/update-server/README.md`。

基础 `tauri.conf.json` **有意不开启** `bundle.createUpdaterArtifacts`：开启后每次 `tauri build` 都必须提供 `TAURI_SIGNING_PRIVATE_KEY`，未配置签名 key 的普通构建会直接失败。签名更新产物统一由发布命令覆盖：

```sh
pnpm tauri build --bundles deb \
  --config '{"bundle":{"createUpdaterArtifacts":true}}'
```

## 本地质量门禁

```sh
pnpm rust:fmt          # cargo fmt 格式化（可写）
pnpm check:rust        # fmt --check + clippy -D warnings + cargo test
pnpm tauri build --debug --no-bundle   # 不打包的快速构建验证
```

`check:rust` 单独于根目录的 `pnpm check`：CI 的 verify job 只装 Node，不装 WebKitGTK，而 clippy/test 需要链接系统库。CI 里有独立的 `rust` job 执行同样的三条命令。

系统依赖（Linux）：

```sh
# Arch / CachyOS
sudo pacman -S webkit2gtk-4.1 base-devel curl wget file openssl libappindicator-gtk3 librsvg

# Ubuntu / Debian
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev \
  patchelf libxdo-dev libssl-dev build-essential
```

`cargo test` / `cargo clippy` 在 `dev` 模式下不会读取打包后的前端产物，因此无需先执行 `pnpm build`。

## 如何新增一个 Rust 命令

1. 在 `src/commands/` 对应模块中定义命令，例如 `files.rs`（复用 `crate::error::AppResult`）：
   ```rust
   #[tauri::command]
   pub fn read_something(path: String) -> AppResult<String> { /* ... */ }
   ```
2. 在 `src/lib.rs` 的 `tauri::generate_handler![...]` 中注册命令名；未注册的命令前端调用会报 `not found`。
3. 从 `src/lib/` 或 `src/composables/` 统一封装 `invoke('read_something', { path })`，组件不直接调用。
4. 参数命名：Rust 用 `snake_case`，前端 `invoke` 用 `camelCase`（如 Rust `always_on_top` ↔ JS `alwaysOnTop`）。命令名本身保持 `snake_case`。
5. 自定义命令不需要在 capability 中授权；只有 `core:*` 与插件命令需要。
6. 错误返回 `Result<T, E>`，`E` 需可序列化（可直接用 `String` 或 `error.rs` 中的统一错误）。
7. 补对应纯函数测试，并运行 `pnpm check:rust`。
