# 前端架构与规范

> “规范”不是某个唯一标准。对 QEStudio 来说，规范是**团队约定 + 可自动检查的约束**：目录分层清晰、组件职责单一、类型完整、状态可追踪、关键逻辑有测试、Tauri 边界集中。本文记录当前约定，后续改动优先遵守；确有必要再调整。

## 1. 分层

```text
views/                    路由级页面，只做编排
components/shell/         桌面外壳：标题栏、标签页、活动栏、状态栏、命令面板
components/<feature>/     领域组件：overview / projects / runs / reports / environments / settings
components/ui/            通用展示组件：按钮式小组件、图表、弹层、徽标
stores/                   Pinia 状态与动作
composables/              可复用的 Vue 逻辑（窗口、时钟、快捷键、平台信息）
lib/                      纯函数与适配层（格式化、导出、报表 HTML、Tauri 外部调用）
data/                     数据契约与 mock 数据
router/                   路由与 meta
layouts/                  工作台布局
assets/                   设计令牌与全局样式
```

### 各层职责

| 层 | 应该做什么 | 不应该做什么 |
| --- | --- | --- |
| `views` | 读取 store、组合领域组件、处理路由 | 大段表格/弹窗标记、纯计算逻辑 |
| `components/shell` | 桌面级交互外壳 | 具体业务页面逻辑 |
| `components/<feature>` | 一个业务区块的完整 UI 与交互 | 跨页面全局状态、直接调 Tauri |
| `components/ui` | 无业务含义的展示与交互原语 | import store、router、Tauri API |
| `stores` | 领域状态、动作、派生数据 | DOM 操作、组件生命周期 |
| `composables` | 可复用的响应式逻辑与浏览器/Tauri 适配 | 具体页面布局 |
| `lib` | 纯函数、数据转换、导出文件、外部链接 | Vue 响应式、组件状态 |
| `data` | 类型、mock、常量 | 副作用 |

## 2. 组件约定

1. **单一职责**：一个组件只解决一件事。页面级 `view` 控制在 150 行左右，领域组件通常 50–150 行。
2. **Props down / events up**：展示组件通过 `defineProps` 接收数据，通过 `defineEmits` 通知父级；不直接修改 props。
3. **类型完整**：所有 props、emits、store、纯函数都有类型；项目开启 `strict`、`noUnusedLocals`、`noUnusedParameters`。
4. **展示组件不依赖全局**：`components/ui` 不 import store/router/Tauri，方便复用和测试。
5. **领域组件可以使用 store**：例如 `AppearanceSettings` 直接读写 `useSettingsStore`，这是有意的“容器组件”；纯展示部分继续下沉到 `ui`。
6. **跨领域复用先放 `ui` 或 `lib`**，不要从一个 feature 直接 import 另一个 feature 的内部组件。
7. **命名**：
   - 组件文件 `PascalCase.vue`
   - composable `useXxx.ts`
   - Pinia store `useXxxStore`
   - 纯函数 `verbNoun`（如 `buildReportHtml`、`sortReports`）
8. **不要为抽象而抽象**：同一模式出现 3 次以上再抽通用组件；否则优先保持可读性。

### 弹窗与浮层

所有弹窗共享 `components/ui/dialog/BaseDialog.vue` 与 `composables/useDialogFocus.ts`，统一处理：

- Escape 关闭（可用 `persistent` 禁用）
- 点击遮罩关闭
- 焦点锁定、关闭后焦点恢复
- body 滚动锁定
- `role="dialog" / "alertdialog"`、`aria-labelledby`、`aria-describedby`

| 组件 | 场景 |
| --- | --- |
| `BaseDialog` | 自定义弹窗底座，支持尺寸、tone、header/icon/footer slot |
| `AppModal` | 兼容旧 API 的标准弹窗 |
| `ConfirmDialog` | 危险 / 警告 / 成功 / 信息确认，支持 loading |
| `NoticeDialog` | 单按钮结果提示 |
| `FormDialog` | 表单弹窗，Enter 提交、busy 锁定、footer 固定 |
| `AppDrawer` | 左右侧抽屉，适合日志、详情、辅助表单 |
| `PopoverMenu` | 锚定菜单，支持禁用、危险项、图标、分隔线 |
| `Toast` | 非阻塞轻量反馈（`stores/ui` + `shell/ToastViewport`） |

可在开发环境访问 `/#/ui-kit` 查看全部样式；入口在「设置 → 关于 → 组件预览」。

### 独立窗口

`shell/DetachedWindowButton.vue` 不是应用内浮层，而是通过 Rust 命令创建真正的系统窗口：

- 主窗口把 `{ title, subtitle, body, movable, resizable, controls, alwaysOnTop }` 写入 `localStorage` 的 `qestudio.detached.<id>`，再调用 `open_detached_window`
- 新窗口加载 `index.html#/detached/<id>`，该路由位于 `WorkbenchLayout` 之外，只渲染 `DetachedWindowView`
- 同一 id 再次打开时 Rust 侧聚焦已有窗口并同步 `resizable` / `maximizable` / `always_on_top`，前端用 Tauri 事件 / `BroadcastChannel` 通知它重新读取载荷
- 窗口形态由 `movable` × `resizable` 组合：固定 / 仅移动 / 仅调整 / 自由；标题栏拖拽区仅在 `movable` 时渲染
- 无边框窗口的缩放由 `DetachedWindowView` 的 8 个自绘把手调用 `core:window:allow-start-resize-dragging` 完成
- 右上角复用 `shell/WindowControls.vue`：`controls=false` 时整体隐藏，只能按 Esc 关闭；`alwaysOnTop=true` 时在头部显示「窗口置顶」徽标
- `lib/desktop-capabilities.ts` 通过 `desktop_capabilities` 命令探测窗口后端：
  - X11 / macOS / Windows：原生 `always_on_top`
  - KDE Plasma Wayland：Rust 侧 `kwin_bridge` 用 zbus 调 KWin scripting（`loadScript` + `start`），按「标题 + 隐藏唯一标记」设置 `keepAbove`（同名窗口不会串）；脚本仅驻留 KWin 内存，窗口销毁时 `unloadScript`
  - 其他 Wayland 合成器：无通用置顶协议，头部显示「置顶不可用」，可用 `pnpm tauri:x11` 走 X11 兼容模式
- 浏览器预览无 Tauri 时回退到命名 `window.open`；载荷注册表带 24h TTL 与 20 条上限，避免无限增长
- 组件只依赖 `lib/detached-window.ts`，不直接 import Tauri API

## 3. 数据流

```text
Tauri / API（mock 或 HTTP）
      ↓
stores（状态 + 动作 + 异步状态）
      ↓
views（路由编排、选择、路由 query）
      ↓ props
components（展示/交互）
      ↓ emit
views / stores（更新状态）
```

示例：运行记录

- `RunsView` 负责选中的运行、筛选条件、路由 query。
- `RunsHeader`、`RunFilters`、`RunTable`、`RunInspector` 负责具体 UI。
- `useRunsStore` 负责运行数据与模拟的实时进度。
- `RunLogConsole` 只负责日志渲染与自动滚动，不知道 store 的存在。

## 4. API 层

协议与 Mock 的完整说明见 [API.md](API.md)。

```text
contracts/    DTO 与请求/响应类型（唯一事实来源）
endpoints/   按域拆分的调用函数，只依赖 ApiClient
client.ts    注入 token、拼接 header、归一化错误
mock/        内存数据库 + 路由处理器 + MockTransport
```

约定：

- 契约字段使用 `snake_case`，与 Rust 后端一致；页面 view model 由 mapper 转换。
- 成功直接返回资源或 `PageDto`；错误统一为 `{ error: { code, message, details? } }`。
- 服务端负责分页、筛选、排序与权限校验；客户端不复制业务规则。
- 所有请求经 `ApiTransport`，当前默认是 MockTransport；切换 HTTP 时只替换 transport。
- 权限在 API 层校验（Mock 已实现），页面只做展示与禁用态，不作为安全边界。
- i18n：`src/i18n` 存放英文词典与 locale 切换；中文源文案作为 key，Mock 业务数据保持原语言。
- 新增接口的顺序：`contracts → endpoints → mock handler → test → docs`。
- 页面数据经 `mappers.ts` 转成 view model；组件不直接依赖 DTO。
- 会话由 `stores/session.ts` 管理，路由守卫负责登录态；API 权限只做校验，不作为前端安全边界。
- 页面/组件不直接调用 `fetch` 或 `invoke`；只通过 store/endpoint。

> 迁移状态：API 层、登录/session 与全部业务页面已完成；运行进度/日志由 Mock API 推进、`runs` store 轮询，工作区数据持久化在 localStorage，下一步换成 Tauri 事件或 WebSocket 流。

## 5. Tauri 边界

所有原生调用集中在少数文件，组件不直接 `invoke`：

- `composables/useDesktopWindow.ts`：窗口控制
- `composables/usePlatformInfo.ts`：`platform_info`
- `lib/export.ts`：`save_text_file`
- `lib/external.ts`：`openUrl`
- `lib/detached-window.ts`：`open_detached_window`、载荷注册表与窗口更新同步
- `lib/desktop-notification.ts`：`notification:default` 系统通知
- `src-tauri/src/`：Rust 侧按职责拆分——`lib.rs` 只做 Builder 装配，`commands/{platform,files,keychain,windows}.rs` 放前端可调用命令，`windowing.rs` / `tray.rs` / `menu.rs` 分别管窗口、托盘与 macOS 原生菜单，`desktop/` 做会话检测与 Wayland/KWin 置顶桥接，`error.rs` 统一错误；模块地图见 [src-tauri/README.md](../src-tauri/README.md)
- `tauri-plugin-window-state`：窗口尺寸/位置恢复
- `tauri-plugin-updater` / `tauri-plugin-process`：更新检查、安装与重启（`VITE_UPDATER_ENABLED` 控制）
- `stores/tabs.ts`：标签页 localStorage 持久化

好处：浏览器预览可以安全降级；替换原生实现时只改边界层。

## 6. 质量门禁

```sh
pnpm lint        # oxlint，0 warnings / 0 errors
pnpm type-check  # vue-tsc，strict
pnpm test        # vitest + @vue/test-utils + jsdom
pnpm build       # 生产构建
pnpm check       # 以上全部串行执行
pnpm check:rust  # cargo fmt --check + clippy -D warnings + cargo test
```

`pnpm check:rust` 需要 Rust 工具链与 WebKitGTK 等系统依赖，不并入 `pnpm check`，由 CI 的独立 `rust` job 执行。

当前覆盖：

- API：auth/refresh、RBAC、项目分页/CRUD、运行/日志/取消、报告生成、环境/变量、通知、公告、用户/角色/权限
- 会话：登录页、token 持久化、刷新、退出、按权限禁用操作
- 异步 UI：LoadingState / ErrorState / 重试
- 组件：`StatusBadge`（状态、色调、脉冲点）、`ConfirmDialog`（tone、事件、loading）
- store：标签页打开/关闭/固定页保护/关闭其他
- 纯函数：项目套件与指标、报表排序与 HTML 转义
- CI：`.gitea/workflows/ci.yml`（verify：install / lint / type-check / test / build；rust：fmt / clippy / test；e2e 独立 job：Playwright + Chromium）
- E2E：登录、刷新恢复、退出、工作台导航、运行触发/取消/重跑、viewer RBAC、登录/概览/项目/运行/报告/环境 axe 可访问性、更新检查、中英切换
- 手工验证：浏览器逐页 + 弹窗/抽屉/菜单 + Tauri 原生二进制渲染

新增逻辑优先补纯函数测试；涉及 store 的动作补 store 测试；关键交互补组件测试。

## 7. 目录约定

```text
src/components/
├── shell/          桌面外壳（跨页面、单例）
├── overview/       概览领域
├── projects/       项目领域
├── runs/           运行领域
├── reports/        报告领域
├── environments/   环境领域
├── settings/       设置领域
└── ui/             通用展示组件
```

新增页面时：

1. 在 `views/` 建页面，只做编排。
2. 在 `components/<feature>/` 建领域组件。
3. 可复用的展示件放 `ui/`，纯逻辑放 `lib/`。
4. 在 `router/index.ts` 注册路由与 `meta`。
5. 补 store/纯函数测试与 `pnpm check`。

## 8. 有意不做的事

- **不引入自动导入**：显式 import 更容易追踪依赖，桌面端包体也可控。
- **不做 barrel 文件**：避免循环依赖和 tree-shaking 噪音。
- **不建全局事件总线**：跨组件通信优先 store 或 props/emits。
- **不为了“组件化”拆碎**：一个 20 行的纯展示块不必单独成文件。
- **不直接接后端**：当前是 mock 数据；接入 API 时新增 `lib/api/` 与鉴权 store，而不是把 fetch 写进组件。

## 9. 与“市面规范”的关系

当前结构接近主流 Vue 3 + Vite + Tauri 项目的常见做法：

- Vue 官方风格指南：`<script setup>`、PascalCase 组件、props/emits 显式声明
- Pinia 官方模式：setup store、动作内聚
- Tauri 官方建议：前端与 Rust 命令通过清晰的 API 边界交互
- Feature-based 目录：按业务域分组，而不是按文件类型堆平
- Vitest + Vue Test Utils：贴近 Vue 生态的测试组合
- oxlint：快速、零配置的 correctness 检查

它不是“唯一正确”的模板。团队规模变大后，可以再考虑：

- ESLint + Prettier / oxfmt 的格式化门禁
- Storybook 或 Histoire 做 UI 组件目录
- Playwright E2E 覆盖桌面关键路径
- CI 中执行 `pnpm check`
