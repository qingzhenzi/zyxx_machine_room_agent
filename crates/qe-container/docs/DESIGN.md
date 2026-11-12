# qe-container 设计

> 状态：v0.1，已实现并通过本机 Docker 29.7 与 Incus 7.5 冒烟。本文记录取舍，不替代 rustdoc。

## 1. 定位

`qe-container` 是**本地 Docker 控制面**，不是测试框架、不是 compose 替代品、也不是 Web 仪表盘。

它解决三类问题：

1. 管理已有容器：list / inspect / start / stop / restart / remove / logs / exec / stats；
2. 启停常驻服务：Python/Node 服务、DB、设备辅助进程；
3. 管理本地 bridge 网络：自定义 subnet / gateway / bridge interface，规划冲突，显式重建。

不做：Dockerfile 构建、多服务依赖编排、Kubernetes、daemon 配置修改。

## 2. 对象模型

```text
Engine
├── images      pull / inspect / list
├── containers  create / start / stop / restart / remove
│               logs / wait / exec / stats
├── networks    create / list / inspect / remove / connect / disconnect
├── services    ensure / status / stop
└── jobs        start / logs / wait / finish
```

`ContainerSpec` 是唯一容器模型。`Job` 与 `Service` 共用它：

- `Job`：一次性执行，结束后按 `remove_on_finish` 删除；
- `Service`：常驻，带 restart policy、healthcheck、spec hash 与 drift 策略。

高层操作会给容器打内部标签：

| label | 含义 |
| --- | --- |
| `qe.managed=true` | 由本 crate 创建 |
| `qe.kind=container\|job\|service` | 生命周期类型 |
| `qe.created-by=qe-container` | 来源 |
| `qe.spec-hash=<sha256>` | service 的规范配置哈希 |

外部容器没有这些标签，高层 `ensure_service` 会拒绝接管；低层 `remove_container` 等仍可显式操作，但不会自动回收。

## 3. 传输选型

选 **bollard + Docker Engine API**，而不是 CLI：

- 结构化 stdout/stderr、pull 进度、stats、exec 退出码；
- 不需要解析 `docker` 输出，也不依赖 CLI 子进程存活；
- 本机 Docker 29.7 / API 1.55，bollard 客户端 1.53，`negotiate_version()` 正常。

代价与边界：

- Docker 优先；Podman 的兼容 API 可以试，但 bridge name / subnet / stats 不承诺一致；
- 暂未启用 bollard 的 TLS/SSH feature，只保证 unix socket 与 `DOCKER_HOST` 的 http/tcp；
- Docker Desktop 下 `docker1` 位于 VM 内，宿主机看不到同名网桥；
- **没有 daemon.json 修改**。改默认 `docker0` 网段需要 `bip`/`default-address-pools` + 重启 dockerd，属于宿主级破坏操作，不在客户端里静默做。

### 3.1 Incus 后端

`IncusBackend`（`src/backend/incus/`，按 `transport`/`wire`/`client`/`mapping`/`streams`/`backend`
分层的模块树）把同一个 `Backend` trait 映射到 Incus REST API
（`/var/lib/incus/unix.socket`，`http://incus/1.0/...`），实测于 Incus 7.5。传输层用
hyper 1.x + hyperlocal 直连 unix socket，不依赖 `incus` CLI；JSON envelope
（`{"type":"sync|async|error","metadata":...}`）由 `IncusClient` 统一解析，async 操作
轮询 `/1.0/operations/{id}`，operation 被快速清理（404）时视为完成、由调用方验证后置
条件。`Transport` trait 抽象传输，单测用 `FakeTransport` 路由表离线覆盖全部映射。

Docker→Incus 语义映射（经 20+ 轮 live 探针逐一验证）：

| Docker 概念 | Incus 实现 |
| --- | --- |
| labels | `user.<key>` 配置项（实例与网络均支持，可往返） |
| env | `environment.<key>` 配置项 |
| entrypoint + command | `raw.lxc: lxc.init.cmd = /bin/sh -c "<argv> > /qe-init.out 2> /qe-init.err; echo $? > /qe-exit-code"` |
| published ports | `proxy` 设备（`listen: tcp:<host>:<port>`、`connect: tcp:127.0.0.1:<cport>`、`bind: host`）；临时端口用 `TcpListener bind :0` 预分配 |
| bind mounts | `disk` 设备（`source`/`path`/`readonly`） |
| named volumes | custom storage volume，`pool`/`source` 磁盘设备挂载（自动创建） |
| network attach / static_ip | `nic` 设备（`network` 字段 + `ipv4.address`）；未指定网络时取 default profile 的 nic |
| root read-only | 根 `disk` 设备 `readonly: "true"` |
| tmpfs | `raw.lxc: lxc.mount.entry = tmpfs <rel> tmpfs <opts>,create=dir 0 0` |
| ulimits | `limits.kernel.<name> = soft:hard`（`lxc.prlimit.*` 会被拒） |
| 资源限制 | `limits.memory`（字节）、`limits.cpu`（整数）或 `limits.cpu.allowance`（"50%"）、`limits.processes` |
| cap_add / cap_drop | `lxc.cap.keep` / `lxc.cap.drop` |
| logs | wrapper 重定向的 `/qe-init.out` + `/qe-init.err`，经 files API 增量读取 |
| exec | `POST .../exec`（`record-output`）→ 输出文件 `/logs/exec-output/exec_<id>.stdout/.stderr` 增量轮询，退出码在 operation `metadata.return` |
| exit code | `/qe-exit-code` 文件（files API 读取，停止后幂等） |
| stats | `GET /state`：cpu.usage(ns)/memory/network.counters/processes，cpu% 双采样差分 |

**日志/退出码为何走 files API 而非 console**：console 通道是 drain 语义——运行中读取会
消费掉内容、多消费者互抢、短命容器输出丢失、停止后回放不一致，且实例退出瞬间会报瞬时
500（"Invalid PID -1"）。wrapper 把 stdout/stderr/退出码重定向进容器内文件后，files API
读取是累积、幂等、可多消费者的，成为唯一可靠通道；console 仅作无 wrapper 容器的兜底
（容忍一切瞬时错误）。`LogState` 按行拆分时保留残缺尾行，流结束前 flush（无换行结尾的
输出如 `printf` 不丢失）。

`sh -c` wrapper 的引号规则（liblxc 解析 `lxc.init.cmd` 实测）：单引号包裹 + `'` → `'\''`
转义；含 `"` 或 `\` 的参数无法安全表达，直接报 `InvalidSpec`。

状态码映射：102=Stopped（`last_used_at` 为零值/1970 → Created，否则 Exited）、
103=Running、110=Frozen→Paused、103..=110 视为 live。

网络注意点：网络名即 Linux 网桥接口名，**≤15 字符**（`create_network` 预检）；删除设备
需 PUT 全量 devices（PATCH null 报 400）；force 删除必须先 `PUT state {action:"stop",force:true}`
再 DELETE；运行中热插 `nic` 会因 DNS 校验报 409。

已知限制（有意为之）：

- `pull_image` 不支持 registry 拉取，报错提示 `incus image import`（`qe-agent-node` 的
  `IncusImporter` 负责镜像供给）；create 支持 `source:{"type":"image","alias"|"fingerprint"}`；
- `security.no_new_privileges`、日志轮转设置无等价物（`lxc.no.new.privs` / `lxc.console.rotate`
  会破坏配置加载），创建时忽略；
- 退出码仅在指定 `command`/`entrypoint`（走 wrapper）时可靠，镜像默认 init 的容器报 `0`；
- `exec` 的 `user` 必须数字 uid，无 cwd 字段；`tty` 记录为非交互输出。

## 4. 网络规划

`docker1` 被当作**自定义 bridge 网络**：

```toml
[network]
name = "docker1"
subnet = "172.30.0.0/16"
gateway = "172.30.0.1"
bridge_name = "docker1"
```

创建时映射到 Docker：

- `IPAM.Config[].Subnet` / `Gateway`；
- `Options["com.docker.network.bridge.name"] = "docker1"`；
- 容器的 `static_ip` 走 `EndpointSettings.IPAMConfig.IPv4Address`。

`NetworkSpec::plan(existing)` 比较 driver / subnet / gateway / bridge name，返回：

| action | 含义 |
| --- | --- |
| `Create` | 不存在 |
| `Reuse` | 已匹配 |
| `Conflict` | 存在但不匹配 |

`ensure_network` 默认不重建；`NetworkEnsureOptions { recreate, force }` 才允许删除重建。Docker 不支持原地改 subnet，所以重建流程是：

1. 检查附着容器；
2. 未给 `force` 则报 `Conflict` 并列出容器；
3. 给了 `force` 则逐個 disconnect；
4. `remove_network` → `create_network`。

**不自动重连**被断开的容器。这一步故意留给调用方，因为“断开后怎么接回”取决于业务；对 service，推荐先 `service down` 再重建网络。

## 5. 配置模型

一条事实来源：`ServiceSpec` / `ContainerSpec`。

- 代码 API：`ContainerSpec::new(...).with_*`；
- TOML：`ServiceFile::load` → 同一个结构；
- spec hash 只实现一次，drift 检测靠它。

文件结构：

```toml
version = 1

[network]   # 可选
...

[service]   # 必需，扁平
...
```

约束：

- `version = 1`，未知版本拒绝；
- 未知字段在任意层级报错（`config_file.rs` 手工检查，因为 serde 的 `flatten` 会吞掉 `deny_unknown_fields`）；
- 相对 `mounts.source` / `env_file` 按文件所在目录解析；
- `${VAR}` / `${VAR:-default}` 用进程环境展开，未设置且无默认值时报错；
- `env` 优先于 `env_file`；
- map 字段用 `BTreeMap`，hash 与顺序无关；
- 默认日志 `max-size=10m` / `max-file=3`。

`ServiceFile::load` 会做路径解析、变量展开、隐式网络绑定（`[network]` 存在但 `[service].network` 省略时自动引用），然后校验。`config validate/show/export` 在 CLI 里不需要 daemon。

## 6. 端口与健康检查

```toml
ports = [{ container = 8000, host = 18000 }]  # host_ip 默认 127.0.0.1
```

- `host` 省略 = Docker 分配临时端口；`host_port()` 从 inspect 读回；
- 容器端口 → 宿主端口映射存在 `ServiceStatus.host_ports`；
- `0.0.0.0` 暴露需要显式配置，防火墙不在 crate 内处理。

健康检查由 crate 从宿主或 `exec` 侧执行，**不注入 Docker HEALTHCHECK**，因为很多 slim 镜像没有 curl/wget：

| 类型 | 探针 |
| --- | --- |
| `http` | 通过发布的宿主端口发原始 HTTP/1.1 GET，匹配状态码 |
| `tcp` | 连接发布的宿主端口 |
| `exec` | 容器内执行命令，退出码 0 为健康 |

`http`/`tcp` 必须使用已发布的端口；`exec` 适合容器间网络或没有宿主端口的情况。`start_period` / `interval` / `timeout` / `retries` 可配。

## 7. 生命周期与并发

- `ensure_service` 持有一个进程内 async mutex，避免同名服务并发创建；
- 已存在同 hash：复用；不 running 则 start；
- hash 不同：`EnsurePolicy::Error`（默认）/ `Keep` / `Recreate`；
- 创建后 start 失败会尝试删除半成品容器；
- `Job` 的 `wait` 在 Docker 返回非零退出码时仍返回 `ExitStatus`，不是传输错误（bollard 会把非零 exit 包成 `DockerContainerWaitError`，这里做了归一化）；
- 未做后台 reaper 线程；`prune_jobs(force)` 由调用方在启动时显式调用。

## 8. 安全边界

Docker API / socket 等价 root。crate 的默认值：

- 不 privileged、不加 cap、不挂 `docker.sock`、不继承宿主 env；
- bind mount 要求绝对路径；`tmpfs`/`read_only`/`cap_drop`/`no-new-privileges` 可配；
- argv 数组直传，不拼 shell；
- 只自动回收 `qe.kind=job`；
- 重建网络是显式操作。

client-app 适配层必须再加一层：

- 前端传来的 mount 只允许工作区根目录下；
- 不允许前端直接开启 privileged / 任意宿主路径；
- 删除、网络重建需要二次确认；
- 日志/统计通过 `ipc::Channel` 流式转发，组件不直接调用原始 Docker 操作。

## 9. 错误模型

`Error` 是公开枚举，不泄漏 bollard 类型：

- `Config` / `InvalidSpec`：校验与文件问题；
- `EngineUnavailable`：daemon 不可达；
- `Docker { status, message }`：daemon 错误响应；
- `NotFound` / `Conflict`；
- `SpecDrift` / `HealthCheck` / `Timeout`；
- `Image` / `Network` / `Io` / `SerdeJson`。

`Error::code()` 提供稳定字符串，便于 Tauri/HTTP 层映射。

## 10. 测试策略

- 纯函数：ByteSize、name/bridge 校验、hash 顺序无关、TOML 未知字段、env 展开、路径解析；
- `MockBackend`：service 幂等、drift、job 日志/清理、health exec、network create/reuse/recreate、外部容器拒绝接管；
- Incus 映射：`FakeTransport` 路由表 + 映射黄金用例（wrapper 引号、标签往返、状态码、proxy/disk/nic 解析、日志拆行与尾行 flush、增量 offset、force 删除先停），全部离线；
- 真实 Docker：`QE_CONTAINER_TESTS=1` 才跑 `tests/docker_smoke.rs`；
- 真实 Incus：`QE_CONTAINER_INCUS_TESTS=1` 才跑 `tests/incus_smoke.rs`（生命周期、网络、bind mount + 资源限制三条链路，本机 Incus 7.5 全过）；
- CI 默认不需要 daemon；mock 不能替代 API 兼容性，所以保留显式冒烟。

本机已手工验证：Docker 侧 `info`、`ps`、job echo/非零退出、redis service 的 create/status/logs/down、http/tcp health、自定义 bridge `qe-smoke0` + subnet create/inspect/rm、网络冲突与 `--force-network` 断开重建；Incus 侧 create/start/exec/stats/logs/wait/list/remove、网络 ensure/inspect/remove、bind mount 只读挂载、limits.processes。

## 11. 已知限制 / 下一步

- 未实现 Dockerfile build、compose、named volume 管理、PID1 attach、`/events`、registry 登录；
- `exec` 非交互（CLI 用 capture），没有 stdin/TTY resize；
- `NetworkSpec` 的 labels/internal/attachable 不参与 drift 比较；
- `ContainerSpec::validate` 不检查 bind source 是否存在（Docker 会以 root 创建目录，调用方应自行保证）；
- service 的 `HealthCheck::Http/Tcp` 依赖已发布端口；
- Podman 未纳入测试矩阵；
- Incus 侧：无 registry 拉取、无 no-new-privileges/日志轮转等价物、退出码依赖 wrapper、
  网络名 ≤15 字符、运行中不能热插 nic、exec user 必须数字 uid（详见 3.1 节）；
- Incus 后端的 service/jobs 高层编排走同一 `Engine`，未针对 Incus 单独做 healthcheck 扩展；
- client-app 的 Tauri 适配层与 UI 是下一个里程碑。

## 12. 命名

包名 `qe-container`，lib 名 `qe_container`，二进制 `qe-container`；目录 `crates/qe-container`（原 `container-crate`，2026-11 并入 room_agent workspace 并改名对齐）。这样与 backend 的 `qe-*` 命名一致，也方便以后独立发布。
