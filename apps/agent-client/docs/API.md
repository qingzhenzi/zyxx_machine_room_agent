# QEStudio API 规范（Mock 版）

> 状态：**已实现 TypeScript 契约 + 内存 Mock 传输**，页面接入进行中。  
> 真实后端目前提供认证 / 公告 / RBAC；项目、运行、报告、环境、通知属于客户端领域 API，已在 Mock 中定义，真实后端需要按本文补齐。

## 1. 基本约定

| 项 | 约定 |
| --- | --- |
| Base URL | `/api/v1` |
| 传输 | JSON，`Content-Type: application/json` |
| 字段命名 | `snake_case` |
| 时间 | ISO-8601 UTC，例如 `2026-10-28T12:43:30Z` |
| ID | 用户/角色/公告为 UUID；项目/报告为字符串；运行为递增数字字符串 |
| 空值 | 明确的 `null`，不使用空字符串代替 |
| 认证 | `Authorization: Bearer <access_token>` |
| 成功响应 | 直接返回资源或分页对象，不额外包 `data` |
| 错误响应 | `{ "error": { "code", "message", "details?" } }` |
| 追踪 | 响应头 `X-Request-Id`、`X-Trace-Id`；错误对象也带 `traceId` |
| 幂等 | `GET/PUT/DELETE` 幂等；`POST /runs`、`POST /reports` 等创建型接口非幂等 |
| 分页 | `page`（从 1 开始）、`page_size`（1–100，默认 20） |
| 排序 | 服务端默认排序；客户端不假设数组顺序稳定 |

### 分页响应

```json
{
  "items": [],
  "total": 128,
  "page": 1,
  "page_size": 20
}
```

### 错误响应

```json
{
  "error": {
    "code": "forbidden",
    "message": "缺少权限：project:write",
    "details": null
  }
}
```

错误码：

| code | HTTP | 含义 |
| --- | --- | --- |
| `bad_request` | 400 | 参数或请求体非法 |
| `unauthorized` | 401 | 未登录、token 失效 |
| `forbidden` | 403 | 已登录但权限不足 / 账号停用 |
| `not_found` | 404 | 资源或接口不存在 |
| `conflict` | 409 | 唯一约束、状态冲突、最后一个管理员保护 |
| `internal_error` | 500 | 服务端错误（Mock 中不主动返回） |

## 2. 认证与权限

### 2.1 登录

`POST /auth/login`

```json
{ "username": "admin", "password": "Admin@12345" }
```

返回：

```json
{
  "token_type": "Bearer",
  "access_token": "mock_at_...",
  "refresh_token": "mock_rt_...",
  "expires_in": 900,
  "user": { "...": "UserProfile" }
}
```

- access token 有效期 15 分钟。
- refresh token 有效期 7 天，刷新后旧 refresh token 立即失效。
- 登录失败统一返回 `401 unauthorized`，不区分用户名/密码错误。

### 2.2 刷新 / 退出 / 当前用户

| 方法 | 路径 | 说明 | 认证 |
| --- | --- | --- | --- |
| POST | `/auth/refresh` | `{ refresh_token }` 换新令牌 | 否 |
| POST | `/auth/logout` | `{ refresh_token? }` 撤销 refresh token | 否 |
| GET | `/auth/me` | 当前用户、角色、权限 | 是 |

### 2.3 Mock 账号

| 用户名 | 密码 | 角色 | 说明 |
| --- | --- | --- | --- |
| `admin` | `Admin@12345` | admin | 全部权限 |
| `qingzhenzi` | `Admin@12345` | admin | 客户端演示账号 |
| `editor` | `Editor@12345` | editor | 可写项目/运行/报告 |
| `viewer` | `Viewer@12345` | viewer | 只读 |

### 2.4 权限点

```
announcement:read/write/publish/delete
project:read/write/delete
run:read/write
report:read/write
environment:read/write
notification:read
user:manage
role:manage
```

> `notification` 的已读操作只需要 `notification:read`（自己的通知），不要求写权限。

## 3. 项目 Projects

| 方法 | 路径 | 权限 | 说明 |
| --- | --- | --- | --- |
| GET | `/projects` | `project:read` | 分页；`keyword`、`health` |
| POST | `/projects` | `project:write` | 创建项目 |
| GET | `/projects/{id}` | `project:read` | 项目详情 |
| PATCH | `/projects/{id}` | `project:write` | 局部更新 |
| DELETE | `/projects/{id}` | `project:delete` | 删除项目（不级联删除历史运行） |
| GET | `/projects/{id}/suites` | `project:read` | 套件列表 |

创建请求：

```json
{
  "name": "支付网关",
  "repo": "qestudio/payments-gateway",
  "owner": "林墨",
  "framework": "Vitest · Playwright",
  "description": "可选",
  "team": "交易平台",
  "branch": "main"
}
```

`Project` 字段：`id/name/repo/description/owner/team/framework/language/branch/coverage/pass_rate/cases/suites/avg_duration_seconds/last_run_at/health/trend/updated_at`。

## 4. 运行 Runs

| 方法 | 路径 | 权限 | 说明 |
| --- | --- | --- | --- |
| GET | `/runs` | `run:read` | 分页；`status/project_id/environment/keyword` |
| POST | `/runs` | `run:write` | 触发运行 |
| GET | `/runs/{id}` | `run:read` | 运行详情（含 steps/logs） |
| POST | `/runs/{id}/cancel` | `run:write` | 取消运行 |
| POST | `/runs/{id}/rerun` | `run:write` | 重新运行，返回新运行 |
| GET | `/runs/{id}/logs?cursor=&limit=` | `run:read` | 游标分页日志 |

创建运行：

```json
{
  "project_id": "payments-gateway",
  "environment_id": "production",
  "branch": "main",
  "commit": "a91f2c4"
}
```

日志响应：

```json
{
  "items": [
    { "seq": 1, "at": "2026-10-28T12:40:19Z", "level": "info", "message": "开始 API 回归" }
  ],
  "next_cursor": "1",
  "has_more": true
}
```

约束：

- 只有 `running` / `queued` 可以取消。
- 取消后未完成步骤标记为 `skipped`，日志保留。
- 日志按 `seq` 递增，客户端用 `next_cursor` 继续拉取。

## 5. 报告 Reports

| 方法 | 路径 | 权限 | 说明 |
| --- | --- | --- | --- |
| GET | `/reports` | `report:read` | 分页；`project_id/keyword` |
| GET | `/reports/{id}` | `report:read` | 报告详情 |
| POST | `/reports` | `report:write` | `{ run_id }` 由运行生成报告 |

约束：

- 同一运行只能生成一份报告（409）。
- 报告包含 `failures / flaky / suites / trend`。
- 导出 HTML 由客户端完成（`lib/report.ts`），API 不返回文件流。

## 6. 环境 Environments

| 方法 | 路径 | 权限 | 说明 |
| --- | --- | --- | --- |
| GET | `/environments` | `environment:read` | 分页；`kind/status/keyword` |
| POST | `/environments` | `environment:write` | 新增 |
| GET | `/environments/{id}` | `environment:read` | 详情（含 variables） |
| PATCH | `/environments/{id}` | `environment:write` | 修改 |
| DELETE | `/environments/{id}` | `environment:write` | 删除 |
| POST | `/environments/{id}/check` | `environment:write` | 单环境连通性检测 |
| POST | `/environments/check` | `environment:write` | 全部检测 |
| GET | `/environments/{id}/variables` | `environment:read` | 变量列表（敏感值脱敏） |
| PUT | `/environments/{id}/variables` | `environment:write` | 覆盖变量 |

列表返回摘要，不包含 `variables`；详情包含。

## 7. 通知 Notifications

| 方法 | 路径 | 权限 | 说明 |
| --- | --- | --- | --- |
| GET | `/notifications` | `notification:read` | 分页；`unread_only/keyword` |
| GET | `/notifications/unread-count` | `notification:read` | 未读数 |
| POST | `/notifications/{id}/read` | `notification:read` | 单条已读 |
| POST | `/notifications/read-all` | `notification:read` | 全部已读 |

## 8. 公告 Announcements

公开：

| 方法 | 路径 | 认证 | 说明 |
| --- | --- | --- | --- |
| GET | `/announcements` | 否 | 仅 `published`；置顶优先 |
| GET | `/announcements/{id}` | 否 | 未发布返回 404 |

后台：

| 方法 | 路径 | 权限 | 说明 |
| --- | --- | --- | --- |
| GET | `/admin/announcements` | `announcement:read` | 含草稿/归档；`status/keyword` |
| POST | `/admin/announcements` | `announcement:write` | 发布态还需 `announcement:publish` |
| PATCH | `/admin/announcements/{id}` | `announcement:write` | 改状态还需 `announcement:publish` |
| DELETE | `/admin/announcements/{id}` | `announcement:delete` | 删除 |

## 9. 用户 / 角色 / 权限（后台）

| 方法 | 路径 | 权限 | 说明 |
| --- | --- | --- | --- |
| GET | `/admin/users` | `user:manage` | 分页；`keyword/is_active` |
| POST | `/admin/users` | `user:manage` | 创建用户 |
| PATCH | `/admin/users/{id}` | `user:manage` | 修改用户 |
| PUT | `/admin/users/{id}/roles` | `user:manage` | 设置角色 |
| GET | `/admin/roles` | `role:manage` 或 `user:manage` | 角色列表 |
| POST | `/admin/roles` | `role:manage` | 创建角色 |
| PUT | `/admin/roles/{id}/permissions` | `role:manage` | 设置权限 |
| DELETE | `/admin/roles/{id}` | `role:manage` | 删除非系统角色 |
| GET | `/admin/permissions` | `role:manage` 或 `user:manage` | 权限点列表 |

安全约束（Mock 已实现）：

- 不能停用自己。
- 不能停用/降级最后一个启用管理员。
- 系统角色 `admin` 不能改权限、不能删除。
- 仍被用户使用的角色不能删除。

## 10. 客户端 API 架构

```text
src/api/
├── contracts/          # 所有请求/响应 DTO（唯一事实来源）
├── endpoints/          # 按域拆分的类型安全调用函数
├── client.ts           # ApiClient：token、header、错误归一化
├── errors.ts           # ApiError / apiErrorMessage
├── mock/
│   ├── database.ts     # 内存数据、用户/角色/权限、种子
│   ├── handlers.ts     # 路由、校验、权限、分页
│   └── transport.ts    # MockTransport（延迟 + 错误包装）
├── http/transport.ts   # fetch transport：超时、错误 envelope、追踪头
├── mappers.ts          # DTO → 页面 view model
└── index.ts            # createApi(transport) 工厂 + 默认 mock 实例
```

使用：

```ts
import { api } from '@/api'

const tokens = await api.auth.login({ username: 'admin', password: 'Admin@12345' })
api.client.setAccessToken(tokens.access_token)

const page = await api.projects.list({ page: 1, page_size: 20 })
```

切换真实后端时只需：

```ts
const api = createApi(createHttpTransport({ baseUrl: '/api/v1' }))
```

> HTTP transport、401 自动刷新后重试已实现；设置 `VITE_API_MODE=http` 和 `VITE_API_BASE_URL` 即可切换。Mock 仍是默认。

## 11. Mock 行为

- 延迟 120–320ms；测试可传 `latencyMs: 0`。
- 业务数据保存在内存，并持久化到 localStorage（key: `qestudio.mock-db.v1`）；刷新/重启会恢复。清除浏览器站点数据即可回到种子。
- 快照包含 Mock 账号与密码，**只能使用演示凭据**；真实环境必须换成服务端令牌与安全存储。
- 令牌是自包含的 `mock_at / mock_rt`（含用户名、nonce、过期时间），刷新页面后 session 仍可恢复；退出登录只在当前进程撤销 refresh token。生产环境必须换成服务端签发令牌 + 安全存储。
- 环境检测为随机延迟漂移，不访问真实网络。
- 运行创建后由 Mock API 按 45 秒时间轴推进进度、步骤与日志；客户端每 2.8 秒轮询详情。刷新/重启会保留当前进度。下一步替换为 Tauri 事件或 WebSocket 流。
- 登录账号见 2.3。

## 12. 测试

`src/api/__tests__/mock-api.spec.ts` 覆盖 16 个接口场景；`mappers.spec.ts` 覆盖 DTO 映射；`persistence.spec.ts` 覆盖 Mock 数据库持久化；`stores/__tests__/session.spec.ts` 覆盖登录恢复与权限。当前全项目 56 个单测 + 20 个 E2E（含 axe 可访问性、更新检查、中英切换、项目管理、环境变量、用例集 CRUD、管理端与全局搜索、引导、网络状态、提及通知与工作区切换）。

- 未登录 401 / token 刷新轮换
- RBAC：viewer 只读、admin 可写
- 项目分页/过滤/创建/更新/删除
- 运行创建/日志游标/取消
- 环境检测/变量
- 通知已读/未读数
- 公告发布与权限
- 404 错误类型

运行：

```sh
pnpm test
```

## 13. 兼容性与后续

- 本文是 **v1** 契约；字段只增不改，破坏性变更走 `/api/v2`。
- 真实后端接入时需新增项目/运行/报告/环境/通知路由；认证/公告/RBAC 已存在，字段命名一致。
- 待办：由 `contracts/` 生成 OpenAPI 3.1 文档、请求取消/重试策略、错误上报。
