# qeaudit — Linux 主机安全基线核查器（Rust 参考实现）

一个**纯用户态**的 Linux 安全核查工具：检查引导链、虚拟化、设备、磁盘、网络、root 收敛与进程保护，输出人读表格与机器可读 JSON。

> **lib + bin**：除 CLI 外也是库（`qeaudit::scan` / `scan_json` 等），守护进程
> `qe-agent` 以进程内库直调方式做安全基线采集（见 `qe-agent-telemetry::external`），
> 不 spawn 子进程；CLI 保持独立可用（CI 门禁、单机排查）。

> 设计取舍、为什么不写内核模块、以及 eBPF/LSM/RFL 的扩展路径，见 [docs/architecture.md](docs/architecture.md)。

## 快速开始

```bash
# 构建（中国大陆网络见下方"构建说明"）
cargo build --release

# 全量扫描
./target/release/qeaudit scan

# 指定基线策略 + JSON 报告
./target/release/qeaudit --config config/baseline.toml scan --format json --out report.json

# 只跑某一类检查
./target/release/qeaudit scan --only grub,network

# 启动时加固自身进程
./target/release/qeaudit --harden scan

# 查看所有检查器
./target/release/qeaudit list
```

退出码：`0` 干净，`1` 仅告警，`2` 存在失败项，`3` 配置/IO 错误。CI 里用 `--fail-on fail` 卡门禁；定时任务用 `--fail-on never`。

## 覆盖的 44 项检查

| 类别 | 条数 | 关注点 |
| --- | --- | --- |
| GRUB / 引导 | 9 | 内核参数必需项/禁用项、`grub.cfg` 权限、GRUB 口令、Secure Boot、lockdown、模块签名能力、救援 shell、配置是否生效 |
| 内核模块加载面 | 3 | 未签名模块是否被拒、模块加载是否冻结、kexec 是否禁用 |
| 虚拟化 | 4 | 环境识别、策略一致性、IOMMU、嵌套虚拟化 |
| 磁盘空间 | 4 | 容量、inode、临时目录挂载加固、世界可写目录 |
| 设备 | 7 | 必需模块、设备类别、关键 PCI 驱动绑定、失败单元、内核 taint、温度、块设备状态 |
| 网络 | 6 | 网卡、出网连通性、监听端口、内核网络参数、DNS、防火墙 |
| root 收敛 | 7 | UID 0 唯一、shadow 与空口令、SSH 加固、sudo、内核提权面、LSM、root 能力集 |
| 进程保护 | 4 | 自身加固、受保护进程 OOM 优先级、systemd 沙箱评分、重启与看门狗 |

## 生产部署

```bash
install -m0755 target/release/qeaudit /usr/local/bin/qeaudit
install -D -m0644 config/baseline.toml /etc/qeaudit/baseline.toml
install -m0644 deploy/qeaudit.service deploy/qeaudit.timer /etc/systemd/system/
systemctl daemon-reload && systemctl enable --now qeaudit.timer
```

`deploy/qeaudit.service` 里已经写了完整的沙箱指令（`ProtectSystem=strict`、`SystemCallFilter=@system-service`、能力收敛、`OOMScoreAdjust=-900`），可以直接当模板。

## 策略文件

见 [config/baseline.toml](config/baseline.toml)。所有字段都有默认值，只写要覆盖的部分；字段名拼错会直接报错，不会静默忽略。

## 项目结构

```
src/
  main.rs        CLI / 调度 / 退出码
  model.rs       Severity / Status / Finding
  config.rs      TOML 策略
  check.rs       Check trait 与注册表
  checks/        7 类检查器（每类一个文件）
  report.rs      表格 / JSON / 退出码
  harden.rs      prctl 自我保护
  util.rs        /proc /sys / statvfs / 带超时的命令执行
config/          基线策略示例
deploy/          systemd 单元与 timer、AppArmor 模板
docs/            架构与选型说明
qep2p/           独立工具：iroh/QUIC 文件传输 + 自建 relay，见其 README
```

## 附带工具：qep2p

本仓库还有一个独立的 workspace 成员 [qep2p/](qep2p/README.md)：基于 iroh/QUIC 的内网优先文件传输，支持 mDNS 发现、BLAKE3 校验、断点续传、文件分享/下载/推送，以及自建 relay。它不依赖 `qeaudit` 的任何代码，可以单独拆走。

```bash
cargo build -p qep2p --release
./target/release/qep2p id
./target/release/qep2p serve --inbox ~/Downloads/qep2p
./target/release/qep2p share ./big.iso
```

身份模型、库 API、自建 relay 部署和安全边界见 [qep2p/README.md](qep2p/README.md)。

## 构建说明

本机直连 crates.io 的 sparse index 会超时，可以用一次性镜像参数构建（不改全局配置）：

```bash
cargo build --release \
  --config 'source.crates-io.replace-with="rsproxy"' \
  --config 'source.rsproxy.registry="sparse+https://rsproxy.cn/index/"'
```

`Cargo.lock` 已提交，依赖版本可复现。

## 已知边界

- **只读**：当前只检查、不改配置。自动修复（`--apply`）是下一阶段，需要审计日志与回滚。
- **路径硬编码**：测试时可测性不足，`--root <dir>` 前缀在路线图里。
- **非 root 会降级**：读不到 `/etc/shadow`、`grub.cfg` 时对应项为 `Skip` 而非 `Fail`，这是刻意的（避免误报）。
- **未包含 eBPF**：进程/文件事件采集是 L2 扩展，见架构文档 §5。
