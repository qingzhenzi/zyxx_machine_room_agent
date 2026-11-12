# 交付状态

> 这份文档区分“已经能演示/验证的能力”和“仍依赖外部凭据或基础设施的能力”。

## 已完成并可验证

| 能力 | 状态 | 证据 |
| --- | --- | --- |
| 类型安全 Mock API | ✅ | 47 个接口、8 个域、`src/api/contracts` |
| 登录 / session / RBAC | ✅ | 登录页、路由守卫、viewer 禁用写操作、session 测试 |
| 多语言 | ✅ | 简体中文 / English；语言偏好持久化；界面层完整覆盖 |
| 页面接入 API | ✅ | 项目、环境、报告、通知、运行均走 store → API |
| 项目编辑/删除 | ✅ | 项目头部菜单 + 编辑弹窗 + 删除确认；走 Mock API |
| 运行配置 | ✅ | 项目/环境/分支/套件选择后再触发 |
| 报告主动生成 | ✅ | 运行检查器可生成报告并跳转 |
| 环境变量编辑 | ✅ | 变量增删、脱敏、保存 |
| 套件/用例 CRUD | ✅ | 用例集增删改 + 用例增删改/停用 |
| 管理端页面 | ✅ | 公告/用户/角色/权限管理页，权限控制 |
| 全局搜索 | ✅ | /search 跨项目/运行/报告查询 |
| 团队/工作区切换 | ✅ | 标题栏工作区切换；项目/运行/报告按团队范围过滤 |
| 安全会话存储 | ✅ | Tauri OS keychain；Web 预览回退 localStorage |
| loading / error / empty | ✅ | LoadingState / ErrorState / 重试 |
| 持久化 | ✅ | Mock DB、session（Tauri keychain）、settings、tabs；Tauri 窗口状态 |
| 运行实时推进 | ✅ | Mock API 45s 时间轴 + 2.8s 轮询；刷新不回退 |
| 系统通知 | ✅ | 运行结束通知 + 设置页“发送测试通知” |
| 托盘 / 开机自启 | ✅ | 托盘菜单 Open/Quit；autostart 插件 + 设置开关 |
| 文件导入 / 拖拽 | ✅ | 项目 JSON 文件选择 + Tauri 拖拽事件 |
| 全局快捷键 | ✅ | `Cmd/Ctrl+Shift+K` 打开命令面板 |
| 客户端日志 | ✅ | `tauri-plugin-log` + 前端 error/unhandledrejection |
| 原生菜单 / 多窗口 / 深链接 | ✅ | 原生菜单事件、`workspace-*` 窗口、`qestudio://` 协议 |
| 独立窗口 | ✅ | `DetachedWindowButton` + `/detached/:id` 无壳内容窗口；可配移动 / 缩放 / 窗口按钮 / 置顶，复用 / 刷新 / Esc 关闭，浏览器回退弹出标签 |
| 置顶（Wayland） | ✅ / 🟡 | KDE Plasma Wayland 通过 KWin scripting 置顶；其他 Wayland 合成器无通用协议，UI 显示「置顶不可用」并回退 `pnpm tauri:x11` |
| 首次启动引导 | ✅ | 三步 onboarding，可跳过并重新播放 |
| 帮助中心 / 更新日志 | ✅ | 设置 → 帮助；内置 0.1.0 changelog |
| 离线 / 网络状态 | ✅ | online/offline 监听、状态栏/Toast；HTTP GET 失败回退本地缓存 |
| 更新通道 | ✅ | stable / beta 写入 settings，作为 updater 请求头 |
| 设置生效 | ✅ | 自动刷新、刷新间隔、运行完成/失败通知 |
| a11y | ✅ | axe 覆盖登录/概览/项目/运行/报告/环境/设置/管理/搜索，0 critical/serious |
| CI | ✅ | lint / type-check / unit / build + Rust fmt/clippy/test；独立 E2E job |
| E2E | ✅ | 24 个 Playwright 用例 + 桌面冒烟脚本 |
| Tauri WebDriver UI E2E | 🟡 | 当前只做启动/窗口存活冒烟，未接 tauri-driver |
| 提及/评论通知 | ✅ | Mock 提及通知 + 设置开关过滤 |
| Linux 打包 | ✅ | 签名 `QEStudio_0.1.0_amd64.deb` + `.sig`（约 4.5 MB） |
| 更新器基础设施 | ✅ | updater 插件、公钥、检查 UI、manifest 脚本 |
| 本地假更新服务器 | ✅ | HTTP/HTTPS、stable/beta、下载、自签证书、发布脚本 |
| macOS Gitea workflow | 🟡 | 可按 runner/arch 触发；未在真实 macOS runner 跑过 |
| 发布文档 | ✅ | `docs/RELEASE.md` |

## 仍需要外部条件

### 1. 真实更新源

- `src-tauri/tauri.conf.json` 的 endpoint 仍是占位地址
- 需要真实 `latest.json` + 签名产物 + 可访问的更新服务器
- `VITE_UPDATER_ENABLED` 默认关闭，避免误连

### 2. 代码签名

- Windows Authenticode 证书
- macOS Developer ID + notarization
- 当前 Linux `.deb` 未签名

### 3. 多平台 runner

- Windows / macOS 安装包需要对应 runner 和平台依赖
- 当前自动化只覆盖 Linux `.deb`；手动 workflow 可扩展

### 4. 真实后端

- 目前是 MockTransport；项目/运行/报告/环境接口需要真实后端实现
- 认证/公告/RBAC 已存在于 Rust 后端

## 已知限制

- Mock 业务数据仍为中文（作为演示 fixture）；界面层已支持中英切换
- Mock 数据库快照包含演示账号密码；只可用于演示
- Tauri 下 session 通过 keychain 保存；Web 预览回退 localStorage
- 运行使用轮询而非事件流
- a11y 只覆盖主要页面，未做完整人工审计

## 建议收尾顺序

1. 配置真实更新 endpoint，发布签名 `latest.json`
2. 轮换 updater 密钥，接入 CI secrets
3. Windows/macOS runner + 签名证书
4. 用真实后端替换 MockTransport
5. 运行事件流与完整 a11y 审计
