# Room Node 教师端桌面客户端（teacher-client）

> 状态：**阶段 1 MVP 已完成**（2026-11-07）。Tauri v2 桌面壳 + 本地静态 HTML/JS/CSS，
> 提供「登录控制节点 → 查看可用机器 / 课程简表 → 在浏览器中打开教师工作台」。
> 代码位置：`room_agent/apps/teacher-client/`。

## 1. 目标与边界

教师端 Web 工作台（`/teacher`，见 `room_node/frontend/src/views/TeacherView.vue`）已经具备
今日课程、房间、工单、镜像、VM 控制等完整能力。桌面端的定位是**轻量常驻入口**：

- 免开浏览器即可确认「控制节点是否可达、我的机器/课程概况」；
- 一键跳转完整 Web 工作台做写操作/复杂交互；
- 为后续「课表提醒、托盘常驻、自动更新」提供原生壳。

本阶段**只读**：不实现排课编辑、VM 控制、工单处理等写操作（仍走 Web）。

## 2. 目录结构

```text
room_agent/apps/teacher-client/
├── .cargo/config.toml          # crates.io 镜像（与 apps/agent-client 一致，rsproxy）
├── dist/                       # 本地静态前端（无 Node 构建链，直接嵌入二进制）
│   ├── index.html              # 登录页 + 教师数据页
│   ├── styles.css
│   └── app.js                  # 登录/请求/渲染/localStorage/退出/打开浏览器
└── src-tauri/
    ├── Cargo.toml              # 独立 Cargo workspace（name = teacher-client）
    ├── Cargo.lock              # 本应用独立锁文件（不污染 room_agent 后端 workspace）
    ├── build.rs                # tauri_build::build()
    ├── tauri.conf.json         # 窗口 1100×760、CSP、frontendDist=../dist
    ├── capabilities/default.json
    ├── icons/                  # 32/128/128@2x/512 PNG（Tauri 默认窗口图标）
    └── src/main.rs             # api_request / open_external 两个命令 + 应用入口
```

## 3. 系统依赖

Rust `1.88+`（本机验证：1.97.1），以及 Tauri v2 Linux 依赖：

```bash
# Debian / Ubuntu（本机已具备：webkit2gtk-4.1 2.52.6、gtk+-3.0 3.24.52）
sudo apt install -y libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

`cargo build` 以 `pkg-config` 探测 `webkit2gtk-4.1`/`javascriptcoregtk-4.1`/`gtk+-3.0`；
缺失时编译会直接报 `webkit2gtk-4.1 was not found`，按上面的包名安装即可。
运行期还需要系统 `xdg-open`（Linux）或 `open`（macOS）才能「在浏览器中打开教师工作台」。

## 4. 构建

> 该应用与 `apps/agent-client` 一样是**独立 Cargo 工程**，没有加入
> `room_agent/Cargo.toml` 的 workspace：Tauri/webkit2gtk 依赖不应进入后端 workspace 的
> `Cargo.lock`，也不应让后端 CI/`cargo build` 拉入 GUI 依赖。因此 `-p teacher-client`
> 要在应用自己的工程根目录下执行（直接用 `-p teacher-client` 也可以，因为该工程根就是
> 它自己的 workspace）。

```bash
# 方式一（推荐）：进入应用自己的 Cargo 工程根（含 .cargo/config.toml 镜像配置）
cd room_agent/apps/teacher-client/src-tauri

cargo check -p teacher-client            # 快速类型检查
cargo build -p teacher-client            # debug 二进制：target/debug/teacher-client
cargo build -p teacher-client --release  # 发布二进制：target/release/teacher-client
```

```bash
# 方式二：从 room_agent/ 根目录用 --manifest-path 指定（已验证可增量通过；
# 首次全新构建请优先用方式一，镜像配置在 teacher-client/.cargo/config.toml）
cd room_agent
cargo check --manifest-path apps/teacher-client/src-tauri/Cargo.toml -p teacher-client
cargo build --manifest-path apps/teacher-client/src-tauri/Cargo.toml -p teacher-client --release
```

> 注意：在 `room_agent/` 根目录直接执行 `cargo check -p teacher-client` 会报
> `package ID specification teacher-client did not match any packages` —— 这是**预期行为**：
> 根 workspace 的 `members` 不包含 Tauri 应用（与 `apps/agent-client` 的既有架构一致）。
> 如确需让根 workspace 认识它，需要改 `room_agent/Cargo.toml` 的 `members`，但这会把
> webkit2gtk/tauri 依赖带进后端锁文件与 CI，本阶段刻意不做。

本机验证产物（2026-11-07）：

| profile | 路径 | 大小 |
| --- | --- | --- |
| debug | `room_agent/apps/teacher-client/src-tauri/target/debug/teacher-client` | 231 MB（含调试信息） |
| release | `room_agent/apps/teacher-client/src-tauri/target/release/teacher-client` | 约 6.3 MB（strip + thin LTO） |

`cargo build` 会把 `dist/` 里的静态资源通过 `tauri::generate_context!` 嵌入二进制，
**修改 `dist/` 后需要重新 `cargo build`**。

## 5. 运行

```bash
# 桌面环境直接运行
./room_agent/apps/teacher-client/src-tauri/target/release/teacher-client

# 无桌面/CI 冒烟（Xvfb + 强制 X11，避免 Wayland 抢占）
cd room_agent/apps/teacher-client/src-tauri
xvfb-run -a env GDK_BACKEND=x11 ./target/release/teacher-client
```

启动后：

1. 「控制节点地址」默认 `http://127.0.0.1:5174`（Web 控制台入口，`/api` 由 Vite/nginx
   反代到 `qe-api`；也可直接填其他部署地址）；
2. 输入教师账号/密码登录（账号需要 `teacher` 角色，见迁移
   `room_node/migrations/0033_teacher_role.sql`）；
3. 登录后展示：账号信息、可用机器数/在线数、课表数、机器简表（设备槽位/序列号/房间/
   在线/健康）与课程简表（课程/房间/起止日期/状态）；
4. 点「在浏览器中打开教师工作台」→ 用系统默认浏览器打开 `{控制节点}/teacher`；
5. 「退出登录」清空 localStorage 并调用 `POST /auth/logout`。

> 本机已验证（2026-11-07，Xvfb `:99` 1100×760 截图 + xdotool 自动输入）：
> 使用 `admin` 账号登录本机控制节点（`127.0.0.1:5174`），成功展示 17 台机器 / 6 条课表；
> 错误密码提示「账号或密码错误」；退出登录回到登录页且 token 已清除。

## 6. 接口与数据流

```text
WebView (dist/app.js, localStorage 存 token)
  │  window.__TAURI__.core.invoke('api_request', {method,url,token,body})
  ▼
Rust: reqwest ──HTTP──> 控制节点（默认 http://127.0.0.1:5174）
  ├─ POST /api/v1/auth/login          → access_token / refresh_token / user
  ├─ GET  /api/v1/auth/me             → 当前账号（角色/权限）
  ├─ GET  /api/v1/admin/machines      → 机器简表（需要 device:read）
  └─ GET  /api/v1/admin/schedules     → 课表列表（需要 schedule:read）
```

- **HTTP 走 Rust 而不是 WebView fetch**：控制节点 API 的 CORS 白名单默认只放行
  `http://localhost:5173`（见 `room_node/.env` 的 `CORS_ALLOWED_ORIGINS`），而 Tauri WebView
  的源是 `tauri://localhost` / `http://tauri.localhost`；直接 fetch 会被浏览器 CORS 拦截。
  由 Rust 侧发起请求即绕开 CORS，也避免要求控制节点为桌面端放宽白名单。
- `api_request` 只接受 `http/https`，方法白名单 GET/POST/PUT/PATCH/DELETE，20s 超时；
  响应统一返回 `{status, body}`，前端按 HTTP 状态码区分 401/403/409 等。
- `open_external` 只接受 `http/https`，调用系统默认浏览器（Linux `xdg-open` / macOS `open` /
  Windows `cmd /C start`）。
- token 存在 localStorage 的 `room-node.teacher.*` 键中；启动时自动恢复并调用 `/auth/me`
  校验，401 自动清理并回登录页。

## 7. 配置与安全

`src-tauri/tauri.conf.json`：

- `productName = "Room Node Teacher"`、`identifier = "org.qestudio.teacher"`；
- 窗口标题「Room Node 教师端」，尺寸 **1100×760**（min 880×600、居中、可缩放）；
- `build.frontendDist = "../dist"`（纯静态，无 devServer / 无 Node 构建）；
- `app.withGlobalTauri = true`（`dist/app.js` 通过 `window.__TAURI__` 调用命令）；
- CSP：`script-src 'self'`；`connect-src 'self' ipc: http://ipc.localhost http: https:`
  （业务请求实际走 Rust，connect-src 只作为 WebView 侧兜底；`http: https:` 覆盖用户
  自定义控制节点地址）。
- `capabilities/default.json` 仅授予 `core:default`；`api_request`/`open_external` 是应用
  自定义命令，不依赖额外插件 ACL。

安全取舍（MVP）：

- token 明文存 localStorage（与 Web 控制台一致）；后续可迁移到系统 keyring 或
  `tauri-plugin-store` 加密存储；
- 未启用 updater/深链/托盘，避免本阶段扩大攻击面；
- Rust 命令做了 scheme/方法白名单，不能作为任意协议代理。

## 8. 与 Web 教师工作台的关系

| 维度 | 本桌面客户端（阶段 1） | Web 教师工作台 |
| --- | --- | --- |
| 登录/权限 | 同一控制节点、同一账号、同一 RBAC（teacher 角色） | 同左 |
| 数据源 | `auth/me`、`admin/machines`、`admin/schedules`（只读） | 排课工作台聚合、房间、镜像、工单、VM 控制等 |
| 写操作 | 不做，引导跳转 Web | 全部能力 |
| 前端技术 | 纯静态 HTML/CSS/JS 嵌入 Tauri | Vue3 + Vite |
| 分发 | 原生二进制（后续 deb/AppImage/dmg/msi） | 浏览器访问控制节点 |

后续如需在客户端内嵌完整工作台，可评估 Tauri WebView 直接加载 `{base}/teacher`
（需处理跨源 CSP 与登录态共享），本阶段保持「外部浏览器打开」以降低复杂度。

## 9. 验证记录（2026-11-07）

| 检查 | 命令 | 结果 |
| --- | --- | --- |
| 类型检查 | `cargo check -p teacher-client`（在 `src-tauri/`） | ✅ 通过（1m13s，0 warning） |
| debug 构建 | `cargo build -p teacher-client` | ✅ `target/debug/teacher-client`（231 MB） |
| release 构建 | `cargo build -p teacher-client --release` | ✅ `target/release/teacher-client`（约 6.3 MB） |
| 格式 | `cargo fmt --check` | ✅ 通过 |
| 静态页语法 | `node --check dist/app.js` | ✅ 通过 |
| 启动冒烟 | `xvfb-run` / `Xvfb :99` + `GDK_BACKEND=x11` | ✅ 窗口「Room Node 教师端」1100×760，WebKitWebProcess 正常 |
| 登录联调 | xdotool 输入 admin 账号 | ✅ 机器 17 台 / 课表 6 条；错误密码有提示 |
| 退出登录 | 点击「退出登录」 | ✅ 回登录页，token 清理 |

## 10. 阶段规划

- **阶段 2（提醒与常驻）**
  - 轮询 `GET /api/v1/schedule-board`（或后续教师聚合接口），在课程开始前 N 分钟弹
    系统通知（`tauri-plugin-notification`）；
  - 系统托盘/关闭到托盘（`tauri-plugin-tray`、`tauri-plugin-window-state`）；
  - 按教师过滤的「我的课程/我的机器」接口（需要后端增加 teacher-scope 读模型，
    当前 admin 接口返回全量）；
  - token 迁移到 keyring；断网时展示 localStorage 缓存快照。
- **阶段 3（分发与更新）**
  - `cargo tauri build` 打出 `.deb`/AppImage/dmg/msi，接入 CI 与代码签名；
  - 自动更新（`tauri-plugin-updater` + 更新清单服务器 + minisign 公钥）；
  - 单实例（`tauri-plugin-single-instance`）与 `qestudio://teacher` 深链；
  - 多控制节点配置管理、错误上报与日志（`tauri-plugin-log`）。

## 11. 已知限制 / 排障

- 机器与课表是**全量只读列表**（admin 接口返回所有数据），还不是严格意义的「我的」；
  需要后端提供教师维度的聚合/过滤接口。
- 账号必须有 `teacher` 角色（`device:read`、`schedule:read` 等），否则表格显示
  「缺少 device:read / schedule:read 权限」但登录仍成功。
- 控制节点地址填错/服务未启动 → 登录页提示「无法连接控制节点」；`/api/v1` 反代不存在 → HTTP 404。
- Linux 下若报 `webkit2gtk-4.1 was not found`，按 §3 安装依赖；无 GPU 的容器里会有
  `MESA-EGL: DRI3 error` 警告，不影响运行（软件渲染）。
- 修改 `dist/` 后没重新 `cargo build` 会看到旧界面（资源被嵌入二进制）。
