# qe-container

面向本地容器引擎的独立控制面库：管理已有容器、启动常驻服务、发布端口、规划 bridge 网络。支持 **Docker**（bollard）与 **Incus**（REST over unix socket）两个后端，另有不依赖 daemon 的 `MockBackend`。client-app 只是它的消费者之一，crate 本身不依赖 Tauri，也不依赖 QEStudio 的业务类型。

```text
Engine
├── containers  list / inspect / create / start / stop / restart / remove
│               logs / wait / exec / stats
├── images      pull / inspect / list
├── networks    create / list / inspect / remove / connect / disconnect
├── services    ensure / status / stop（常驻容器 + 健康检查 + 端口）
└── jobs        start / logs / wait / finish（一次性容器）
```

## 快速开始

默认后端是 Docker，需要可用的 Docker daemon（本机测试于 Docker 29.7 / API 1.55）：

```sh
cargo run --features cli -- info
cargo run --features cli -- ps -a
cargo run --features cli -- run --image alpine:3.20 -- echo hello
```

切换后端用全局参数 `--engine docker|incus|mock`，或环境变量 `QE_CONTAINER_BACKEND`；
Incus 的 socket 路径可用 `QE_CONTAINER_INCUS_SOCKET` 覆盖（默认 `/var/lib/incus/unix.socket`）：

```sh
cargo run --features cli -- --engine incus info
cargo run --features cli -- --engine incus run --image <fingerprint-or-alias> -- echo hello
```

服务文件：

```toml
version = 1

[network]
name = "docker1"
subnet = "172.30.0.0/16"
gateway = "172.30.0.1"
bridge_name = "docker1"

[service]
name = "py-api"
image = "python:3.12-slim"
command = ["python", "-m", "http.server", "8000", "--bind", "0.0.0.0"]
workdir = "/workspace"
restart = "unless-stopped"
env = { PYTHONUNBUFFERED = "1" }
mounts = [{ source = "./app", target = "/workspace" }]
ports = [{ container = 8000, host = 18000 }]
network = "docker1"
static_ip = "172.30.0.10"
log = { max_size = "10m", max_file = 3 }
healthcheck = { type = "http", port = 8000, path = "/", interval = "1s", timeout = "1s", retries = 30 }
```

```sh
cargo run --features cli -- service up -f examples/python-service.toml
cargo run --features cli -- service status -f examples/python-service.toml
cargo run --features cli -- service logs -f examples/python-service.toml --follow
cargo run --features cli -- service down -f examples/python-service.toml
```

`service up` 是幂等的：已有同 spec 的容器会复用；spec 变了默认报 `SpecDrift`，用 `--keep` 保留、`--recreate` 重建。网络变更用 `--recreate-network`，若还有容器附着再加 `--force-network`（会先断开，不自动重连）。

## Rust API

```rust
use qe_container::{ContainerSpec, Engine, JobOptions, LogOptions};

#[tokio::main]
async fn main() -> qe_container::Result<()> {
    let engine = Engine::connect_from_env().await?;

    let job = engine
        .start_job(
            &ContainerSpec::new("hello", "alpine:3.20")
                .with_command(["sh", "-c", "echo hello"]),
            &JobOptions::default(),
        )
        .await?;

    let mut logs = job.logs(&LogOptions::follow())?;
    while let Some(line) = futures_util::StreamExt::next(&mut logs).await {
        print!("{}", line?.text);
    }
    let status = job.finish().await?;
    assert!(status.success());
    Ok(())
}
```

常驻服务直接走同一个模型：

```rust
use qe_container::{ContainerSpec, EnsureOptions, EnsurePolicy, PullPolicy, ServiceSpec};

let spec = ServiceSpec::new(
    ContainerSpec::new("py-api", "python:3.12-slim")
        .with_command(["python", "-m", "http.server", "8000"])
        .with_port(qe_container::PortBinding::published(18000, 8000)),
);

let status = engine
    .ensure_service(
        &spec,
        &EnsureOptions {
            policy: EnsurePolicy::Error,
            wait_healthy: true,
            timeout: std::time::Duration::from_secs(60),
            pull_policy: PullPolicy::IfMissing,
        },
    )
    .await?;
```

`ServiceSpec` 和 TOML 共用同一个结构；`ServiceFile::load` 只是 TOML 前端。map 字段使用 `BTreeMap`，spec hash 稳定，drift 检测不依赖文件顺序。

## CLI

| 命令 | 作用 |
| --- | --- |
| `info` | daemon 版本、API 版本、rootless、容器/镜像数量 |
| `ps [-a] [--name] [--label] [--network]` | 列表，自动区分 managed / 外部容器 |
| `inspect <容器>` | 完整 inspect（JSON） |
| `start / stop / restart <容器>` | 启停已有容器；`stop/restart` 支持 `--timeout` |
| `rm <容器> [-f]` | 删除容器 |
| `logs <容器> [-f] [--tail]` | 日志（stdout/stderr 分离） |
| `exec <容器> -- <命令>` | 非交互 exec，透传退出码 |
| `stats <容器> [--once]` | CPU / 内存 / 网络 / PIDs |
| `pull <镜像>` | 流式拉取进度 |
| `run --image ... -- <命令>` | 一次性任务，流式日志，结束自动删除 |
| `service up/status/down/logs -f file.toml` | 常驻服务生命周期 |
| `network ls/inspect/create/rm/plan` | bridge 网络管理 |
| `config validate/show/export -f file.toml` | 离线解析、校验、导出（不需要 daemon） |

`--json` 可放在任意子命令前做机器可读输出。全局请求超时用 `--request-timeout`（默认 `120s`）；容器 stop/restart 的 grace period 用子命令的 `--timeout`。全局 `--engine docker|incus|mock` 选择后端（也可用 `QE_CONTAINER_BACKEND` 环境变量）。

## 功能开关

```toml
[features]
default = ["config"]
config = ["dep:toml", "dep:dotenvy"]   # ServiceFile::load / TOML 支持
cli = ["config", "dep:clap", "dep:tracing-subscriber"]
```

不用 TOML 时可以 `default-features = false`，只拿代码 API。`MockBackend` 始终可用，不依赖 daemon。

## Incus 后端

`IncusBackend` 把同一套 `Backend` 契约映射到 Incus REST API（`/var/lib/incus/unix.socket`，`http://incus/1.0/...`），已实测于 Incus 7.5。语义映射（详见 `src/backend/incus/` 模块文档与 [docs/DESIGN.md](docs/DESIGN.md)）：

| 概念 | Incus 实现 |
| --- | --- |
| labels / env | `user.<key>` / `environment.<key>` 配置项 |
| entrypoint + command | `raw.lxc: lxc.init.cmd`（`sh -c` wrapper，输出/退出码落到 `/qe-init.out`、`/qe-init.err`、`/qe-exit-code`） |
| published ports | `proxy` 设备（`bind: host`） |
| bind mount / named volume | `disk` 设备 / custom storage volume |
| 资源限制、ulimits | `limits.memory` / `limits.cpu[.allowance]` / `limits.kernel.<name>` |
| logs / 退出码 | files API 读 wrapper 输出文件（console 仅兜底） |
| exec | `POST .../exec` + `record-output` 轮询输出文件 |

已知限制（有意为之，均有文档说明）：

- `pull_image` 不支持 registry 拉取，镜像用 `incus image import` 导入（`qe-agent-node` 的 `IncusImporter` 是现成供给方）；
- `security.no_new_privileges`、日志轮转设置无 Incus 等价物，创建时忽略；
- 退出码仅在指定 `command`/`entrypoint`（走 wrapper）时可靠，镜像默认 init 的容器报 `0`；
- 网络名即 Linux 网桥接口名，**≤15 字符**（`create_network` 会预检）；
- 运行中实例不能热插 `nic` 设备（DNS 校验 409），移除设备需 PUT 全量 devices；
- `exec` 的 `user` 必须是数字 uid；argv 含 `"` 或 `\` 的参数会被 wrapper 引号规则拒绝（`InvalidSpec`）。

## 接入守护进程（qe-agent）

已作为守护进程能力库链入（`apps/qe-agent`，遥测路径见 `qe-agent-telemetry::external`
的进程内容器清单采集）。若在其他工程（如桌面 GUI `apps/agent-client`）单独引入，
在对应 `Cargo.toml` 加 path 依赖：

```toml
[dependencies]
qe-container = { path = "../qe-container" }   # workspace 内成员也可用 workspace = true
```

建议适配层只做三件事：

1. `#[tauri::command]` 接收前端传来的类型化 spec；
2. 在 Rust 侧做 mount 白名单（只允许项目/工作区根目录）和 `qe.managed` 策略；
3. 用 Tauri 2 的 `ipc::Channel` 转发 `LogLine` / `StatsSample`，组件不直接 `invoke` 任意 Docker 操作。

浏览器预览返回 “engine unavailable”，不要在前端模拟容器状态。Docker socket/CLI 等价 root，webview 不能拿到原始 Docker 权限。

## 安全边界

- 默认不 privileged、不挂 `docker.sock`、不继承宿主 env、不默认 host network；
- bind mount 必须是显式绝对路径；Tauri 层应再限制到工作区；
- `argv` 数组直接交给 API，不拼 shell；
- 日志默认 `max-size=10m` / `max-file=3`，避免常驻服务写满磁盘；
- 只自动回收 `qe.kind=job` 的容器；外部容器和高层 service 不会被静默删除；
- 网络重建是显式破坏性操作，需要 `--recreate-network`；有容器附着时还要 `--force-network`。

## 测试

```sh
cargo fmt --check
cargo clippy --all-targets --features cli
cargo test --all-features
```

默认测试全部走 `MockBackend` 和纯函数，不需要 Docker。真实 daemon 冒烟：

```sh
QE_CONTAINER_TESTS=1 cargo test --test docker_smoke -- --nocapture
```

`QE_CONTAINER_TEST_IMAGE` 可覆盖默认镜像（默认 `debian:bookworm-slim`）。本机实测过 job、service、端口、healthcheck、自定义 bridge / subnet / bridge name 的 create/rm。

Incus 真实冒烟（需要可达的 Incus daemon 与已导入镜像）：

```sh
QE_CONTAINER_INCUS_TESTS=1 \
QE_CONTAINER_INCUS_IMAGE=<fingerprint-or-alias> \
cargo test --test incus_smoke -- --nocapture
```

覆盖生命周期（create/start/exec/stats/logs/wait/list/remove）、网络（ensure/inspect/remove）、bind mount 与资源限制三条链路。

## 目录

```text
src/
├── backend/       Backend trait；docker.rs（bollard）；incus/（REST over unix socket，按 transport/wire/client/mapping/streams/backend 分层）；mock.rs
├── model/         ContainerSpec / ServiceSpec / NetworkSpec / 读模型 / 单位与校验
├── config.rs      EngineConfig、BackendKind、PullPolicy、NetworkEnsureOptions
├── config_file.rs TOML 加载、${VAR} 展开、相对路径、未知字段检查
├── engine.rs      Engine、JobHandle、service/network 编排、health probe
├── stream.rs      LogLine / ExecChunk / StatsSample / PullProgress
└── bin/           qe-container CLI
tests/             engine_mock、config、docker_smoke、incus_smoke
docs/DESIGN.md     设计取舍与边界
```

## v1 边界

已做：容器/镜像/网络/服务/Job、logs、exec、stats、健康检查、端口、bridge 网络规划与重建、TOML、CLI、mock、真实 Docker 冒烟、**Incus 后端全量 Backend 适配与真实冒烟**。

未做：Dockerfile 构建、compose、named volume 管理（Docker 侧；Incus 侧已支持 custom volume）、PID1 attach、`/events` 流、registry 登录、Docker TLS/SSH、Podman 兼容性承诺、daemon.json 修改、Incus registry 拉取。细节见 [docs/DESIGN.md](docs/DESIGN.md)。
