# 架构说明：用 Rust 做 Linux 主机安全核查

本文回答一个问题：**"检查 GRUB、设备、虚拟化、空间、状态、网络、限制 root、保护进程"这 8 件事，哪些该在用户态做，哪些需要内核模块，Rust 里分别怎么写。**

结论先行：**8 项里 7 项半都属于用户态可观测事实，不需要内核模块。** 内核模块只在"必须拦截/必须第一时间感知"的场景才值得付出代价。

---

## 1. 为什么不直接写成内核模块

| 维度 | 用户态（本仓库） | 内核模块（LKM / RFL） |
| --- | --- | --- |
| 判据来源 | `/proc`、`/sys`、`/etc`、netlink | 同上 + 内核内部结构 |
| 崩溃影响 | 进程退出，systemd 拉起 | **整机 panic / 数据损坏** |
| 升级成本 | 换二进制 | 每个内核版本重编；Secure Boot 下还要签名 |
| 可移植性 | 跨发行版几乎一致 | 内核 ABI 不稳定，Rust-for-Linux 仍无稳定 API 保证 |
| 权限 | 少量 capability | ring 0，全有 |
| 开发调试 | 普通 `cargo run` | QEMU + gdb + 内核日志 |

Rust-for-Linux 自 6.1 进入主线，6.18 LTS 已可用，但仍是"能用"而非"好用"：需要特定工具链（`rustc` 版本由内核 `scripts/min-tool-version.sh` 锁定）、`bindgen`、以及内核内 Rust 抽象层随版本变化。**把 90% 的检查逻辑放进用户态，只在真正需要的地方下沉**，是当前工程上最优的分层。

### 推荐的四层结构

```
┌─────────────────────────────────────────────────────────────┐
│ L4  Rust-for-Linux 模块（仅特殊 hook）                        │
│     例：需在内核路径上做自定义决策、特定硬件寄存器访问           │
├─────────────────────────────────────────────────────────────┤
│ L3  强制访问控制：AppArmor / SELinux / BPF-LSM                │
│     例：阻止未授权进程 exec、限制 qeaudit 自身能读哪些文件        │
├─────────────────────────────────────────────────────────────┤
│ L2  eBPF（aya）：事件流，无需内核模块、CO-RE 可移植              │
│     例：进程退出、exec、文件打开、TCP 连接、LSM hook            │
├─────────────────────────────────────────────────────────────┤
│ L1  用户态核查器（本仓库）：/proc /sys /etc + 少量只读命令        │
│     覆盖：GRUB、虚拟化、设备、空间、网络、root、进程状态           │
└─────────────────────────────────────────────────────────────┘
```

**先做 L1，按需加 L2/L3，L4 只在没有别的办法时才碰。**

---

## 2. 八项需求 → 数据源 → Rust 实现

| 需求 | 首选判据（内核事实） | 辅助 / 兜底 | 需要的权限 | 代码位置 |
| --- | --- | --- | --- | --- |
| 检查 GRUB 设置 | `/proc/cmdline`（生效值）、`/etc/default/grub`（配置值）、`grub.cfg` 权限、`/sys/firmware/efi/efivars/SecureBoot-*`、`/sys/kernel/security/lockdown` | `mokutil --sb-state`、`grub2-setpassword` 产物 | 读 `/boot`（root） | `src/checks/grub.rs` |
| 检查设备功能 | `/sys/bus/pci/devices/*/driver`、`/sys/module/<m>`、`/sys/class/*`、`/boot/config-$(uname -r)` | `lspci -k`、`dmesg`、`smartctl` | 基本无需特权 | `src/checks/device.rs` |
| 检查虚拟化 | CPUID leaf 1 `ECX[31]`、`/sys/hypervisor/type`、`/sys/class/dmi/id/*`、`/sys/module/kvm_*/parameters/nested`、`/sys/kernel/iommu_groups` | `systemd-detect-virt` | `/dev/kvm` 需可 stat | `src/checks/virt.rs` |
| 检查设备空间 | `statvfs(2)`、`/proc/self/mountinfo` | `df`（不推荐，locale 依赖） | 无 | `src/checks/space.rs` |
| 检查设备状态 | `/proc/sys/kernel/tainted`、`/sys/class/thermal/*/temp`、`/sys/class/block/*/device/state`、`systemctl --failed` | `journalctl -k -p err` | `CAP_SYSLOG` 读 dmesg | `src/checks/device.rs` |
| 连接网络 | `/proc/net/tcp{,6}`、`/sys/class/net/*/operstate`、`connect(2)` 探测、`nft list ruleset` | `ss -lntp`、`iptables-save` | 读 nft 需 `CAP_NET_ADMIN` | `src/checks/network.rs` |
| 限制 root | `/etc/passwd`、`/etc/shadow`、`/etc/ssh/sshd_config`(+`Include`)、`/etc/sudoers*`、`/proc/<pid>/status` CapEff、`/sys/fs/selinux/enforce` | `visudo -c`、`sshd -T` | 读 shadow 需 root/DAC | `src/checks/root.rs` |
| 保护进程 | `prctl(2)`、`/proc/self/status`、`oom_score_adj`、`systemctl show`、AppArmor/SELinux 状态 | eBPF / LSM（见 §5） | 视动作而定 | `src/checks/process.rs`、`src/harden.rs` |

几个容易被写错、代码里专门处理过的点：

1. **GRUB 要分"配置值"和"生效值"**。只读 `/etc/default/grub` 会漏掉"改了没跑 `grub-mkconfig`"的情况；只读 `/proc/cmdline` 又不知道意图。两者对比才有价值（`GRUB-09`）。
2. **Secure Boot efivar 的格式**是 4 字节属性头 + 1 字节数据，判断 `data[4] == 1`，不是文件非空。
3. **`lockdown` 的当前值是带方括号的**：`none [integrity] confidentiality`，要解析中括号而不是取第一个词。
4. **块设备状态不是统一的 `running`**：SCSI/SATA 是 `running`，NVMe 是 `live`。只认 `running` 会把所有 NVMe 机器误报。
5. **内核线程不占用户态能力**：`/proc/<pid>/status` 里 kthread 的 `CapEff` 也是全 1，统计"root 全能力进程"时必须按 `PPid == 2`（kthreadd）排掉，否则一台机器能报出三百条。
6. **`/etc/ssh/sshd_config` 是"后者覆盖前者"**：`Include` 展开后按出现顺序合并，`sshd_config.d/99-hardening.conf` 才真正生效。只 grep 主文件会误判。
7. **权限不足时必须 `Skip` 而不是 `Fail`**：非 root 读不到 `grub.cfg` 时断言"没设口令"是纯误报。代码里用"一个能读的都没有 → skip"处理。
8. **`/proc/net/tcp` 的地址是小端十六进制**：`0100007F:0016` → `127.0.0.1:22`，要 `u32::from_str_radix(...).to_ne_bytes()`。

---

## 3. Rust 选型

| 用途 | 选择 | 理由 |
| --- | --- | --- |
| CLI | `clap`（derive） | 子命令/枚举/`--help` 一行搞定 |
| 配置 | `toml` + `serde` | 运维友好；`deny_unknown_fields` 防手滑 |
| 输出 | `serde_json` | 报告给平台/流水线消费 |
| 系统调用 | `libc` 直接调 | `statvfs`、`prctl`、`mlockall`、`gmtime_r`，依赖最少 |
| 更安全的封装（可选） | `rustix` / `nix` | 把 errno 变成 `Result`，适合长期演进 |
| eBPF（可选） | `aya` | 纯 Rust、无 libbpf、CO-RE |
| seccomp（可选） | `seccompiler` 或 `libseccomp` | 生成并加载 BPF 过滤器 |
| 进程信息（可选） | `procfs` / `sysinfo` | 省掉手写解析，但会多一层抽象 |

**刻意不做的**：不通过 `sh -c` 执行命令。所有外部命令走 `Command::new(prog).args([...])`，参数逐个传递，天然没有 shell 注入；统一带超时（`util::run`），避免 `systemctl` 卡死拖垮整轮扫描。

**为什么不用 `df`/`free`/`lscpu` 解析**：输出受 locale、列宽、版本影响，且每次多一个进程。`statvfs` 和 `/sys` 是稳定接口。

---

## 4. 代码结构与数据流

```
src/
├── main.rs        CLI、调度、退出码
├── model.rs       Severity / Status / Finding（唯一数据模型）
├── config.rs      TOML 策略，全字段 Default
├── check.rs       Check trait + 注册表
├── checks/
│   ├── grub.rs    引导链 9 项
│   ├── kernel.rs  模块加载面 3 项
│   ├── virt.rs    虚拟化 4 项
│   ├── space.rs   容量/挂载 4 项
│   ├── device.rs  设备功能与状态 7 项
│   ├── network.rs 网络 6 项
│   ├── root.rs    root 收敛 7 项
│   └── process.rs 进程保护 4 项
├── report.rs      表格 / JSON / 退出码
├── harden.rs      prctl 自我保护
└── util.rs        /proc /sys / statvfs / 命令超时
```

数据流是单向的：

```
Config ──┐
         ├─► Check::run(&Ctx) ─► Vec<Finding> ─► Report ─┬─► 表格(stdout)
系统事实 ─┘                                              ├─► JSON(文件)
                                                         └─► 退出码(0/1/2)
```

设计上的三个约束：

- **一个检查项一条 `Finding`**，ID 稳定（`GRUB-04`），可直接对接到工单/白名单/趋势库；
- **单个检查器 panic 被 `catch_unwind` 隔离**，降级为 `Skip`，不影响整轮；
- **策略与代码分离**，阈值全在 TOML，改基线不用改代码、不用重编。

---

## 5. 需要"更实时/更强"时怎么加（L2/L3）

### 5.1 eBPF：进程与文件事件（推荐先做这个）

`aya` 的典型骨架（示意，未在本仓库编译）：

```rust
// 内核侧：tp/sched/sched_process_exit → 关键进程退出即上报
#[tracepoint(name = "sched_process_exit", category = "sched")]
pub fn on_exit(ctx: TracePointContext) -> u32 {
    // 读 comm/pid，写入 ringbuf
    0
}
```

```rust
// 用户侧
let mut bpf = Ebpf::load(include_bytes_aligned!("../target/bpf/prog"))?;
let mut ring = RingBuf::try_from(bpf.take_map("EVENTS").unwrap())?;
```

用途：关键进程被 `kill -9`、被替换二进制后立即感知；比轮询 `/proc` 快一个数量级。
前置条件：内核 `CONFIG_BPF=y`、`CONFIG_BPF_SYSCALL=y`、BTF（`/sys/kernel/btf/vmlinux`），以及 `CAP_BPF`（5.8+）。

### 5.2 BPF-LSM：真正"保护进程"

要**阻止**而不是**记录**，需要 LSM hook：

- `bprm_check_security`：拦截 exec（例如阻止从 `/tmp` 执行）；
- `file_open`：阻止读关键文件（`/etc/shadow`、私钥）；
- `task_kill`：阻止对受保护进程发信号。

前提：内核 `CONFIG_BPF_LSM=y` 且启动参数 `lsm=...,bpf`，当前 `lsm=` 列表里能看到 `bpf`。加载后是**在内核里做策略判定**，这就是"保护进程"的内核侧正解，而不是写一个模块去 hook 系统调用。

### 5.3 限制 root 加载未签名模块（运行时三档开关）

这是"能不能在启动后收掉 root 的模块加载权"的标准答案，三档强度递增，**都只能开不能关**（写 0 返回 EPERM，重启才复位）：

| 机制 | 命令 | 拒绝什么 | 代价 |
| --- | --- | --- | --- |
| 签名强制 | `echo 1 > /sys/module/module/parameters/sig_enforce` | 未签名 / 签名不可验证的模块 | 树外模块（nvidia、zfs、vbox）必须先签名 |
| lockdown | 启动参数 `lockdown=integrity` | 未签名模块 + kexec + `/dev/mem` + MSR | 部分运维工具受限 |
| 冻结加载 | `sysctl -w kernel.modules_disabled=1` | **所有**模块，含已签名 | 热插拔、USB、后续驱动全废 |

前置条件：`sig_enforce` 节点只在 `CONFIG_MODULE_SIG=y` 时存在（本机 CachyOS：`SIG=y`、`SIG_ALL=y`、当前 `N`）。若内核是 `CONFIG_MODULE_SIG_FORCE=y`，则开机即强制，cmdline 也改不回来。

**但这不是"限制 root"的边界**，只是提高门槛：

- root 可以 `reboot` 并在 GRUB 里去掉 `module.sig_enforce=1`——所以 GRUB 口令 + Secure Boot 是前置条件；
- root 可以 `kexec` 一个未加固内核——所以要 `kernel.kexec_load_disabled=1` 或 lockdown；
- Secure Boot 关闭时，root 有可能把自建密钥塞进内核密钥环再签自己的模块（是否成功取决于内核配置与 secondary keyring 的写权限），所以**签名强制不能当作对抗 root 的隔离边界**；
- 这些开关只影响"新模块加载"，已加载的模块和已存在的内核内存写入路径（`/dev/mem`、`iopl`）需要 lockdown 才能堵住。

完整的收敛链条：固件口令 → Secure Boot → 签名内核 → GRUB 口令 → `lockdown=integrity` → 启动末期 `modules_disabled=1` → `kexec_load_disabled=1`。

对应检查：`KERN-01/02/03`（`src/checks/kernel.rs`）与 `GRUB-07`。

### 5.4 Rust-for-Linux 模块（最后手段）

只有在以下情况才考虑：需要访问 eBPF 覆盖不到的内核路径、需要直接操作特定硬件、或需要在内核里做极低延迟决策。

最小骨架（示意）：

```rust
// kernel/qe_guard.rs（不在 cargo workspace 内，用内核的 Makefile 构建）
use kernel::prelude::*;

module! {
    type: QeGuard,
    name: "qe_guard",
    author: "QE",
    description: "example RFL module",
    license: "GPL",
}

struct QeGuard;

impl kernel::Module for QeGuard {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        pr_info!("qe_guard loaded\n");
        Ok(QeGuard)
    }
}
```

代价要提前算清楚：内核升级即重编、Secure Boot 需签名、崩溃即宕机。**不建议把"检查"逻辑放进来。**

### 5.5 文件完整性 / 进程监督的其他原语

- `fanotify`（`FAN_OPEN_PERM`）：拦截文件打开，用户态决策，比 inotify 强；
- `pidfd_open` + `poll`：无 pid 复用风险的进程监督，Rust 里用 `libc::syscall(SYS_pidfd_open, pid, 0)`；
- `seccomp` 过滤器：限制自身可用系统调用（配合 `SystemCallFilter=@system-service` 已能覆盖大部分）。

---

## 6. "保护进程"的三层落地（本仓库已实现的部分）

| 层 | 机制 | 代码 |
| --- | --- | --- |
| 自我保护 | `PR_SET_DUMPABLE=0`（禁 ptrace/读内存）、`PR_SET_NO_NEW_PRIVS=1`、`mlockall`（不进 swap）、`RLIMIT_CORE=0` | `src/harden.rs`，`--harden` |
| 被守护 | `oom_score_adj`、`Restart=always`、`WatchdogSec` | `PROC-02/04` |
| 被约束 | `NoNewPrivileges`、`ProtectSystem=strict`、`PrivateTmp`、`CapabilityBoundingSet`、`SystemCallFilter` | `deploy/qeaudit.service`、`PROC-03` |

注意 systemd 的语义陷阱：**`CapabilityBoundingSet=` 为空表示"一个能力都不给"（最严格）**，而"未设置"才是全量能力。`systemctl show` 里两者都可能是空串，需要结合单元文件判断——代码里用"非空且含 `cap_sys_admin`"作为不达标条件来规避这个歧义。

---

## 7. 工程化注意事项

- **权限最小化**：整轮扫描只需 `CAP_DAC_READ_SEARCH`(shadow) + `CAP_SYS_PTRACE`(他人 /proc) + `CAP_SYSLOG`(dmesg) + `CAP_NET_ADMIN`(nft)。不要用裸 root 跑一切。
- **性能**：本机全量 44 项在探测目标可达时 < 1s；目标不可达会等满一个超时（默认 2s），这也是唯一明显的慢点。所有外部命令与网络探测都有超时，不会挂死。
- **误报优先于漏报**：权限不足、平台不支持、组件未安装 → `Skip`，并在报告里说明原因。`Skip` 数量本身是运维信号（agent 权限配错了）。
- **跨发行版**：Debian/Ubuntu、RHEL/CentOS、SUSE、Arch、Alpine、容器。差异集中在 `grub.cfg` 路径、`sshd_config.d`、`/etc/sysconfig`，代码里都做了多路径探测。
- **可测试性（待补）**：当前路径是硬编码的 `/proc`、`/etc`。要写单元测试，建议引入 `--root <dir>` 前缀，把 fixture 文件树喂给检查器，断言 JSON 输出。这是下一步最该做的事。
- **修复动作要分级**：`只报告` → `给出命令` → `--apply 自动修复`。自动修复必须带审计日志、幂等、可回滚，且默认关闭。改 `grub.cfg`、改 `sshd_config` 都属于"改错就失联"的操作。
- **分发**：`musl` 静态链接（`x86_64-unknown-linux-musl`）避免 glibc 版本问题；RPM/DEB 打包；二进制签名的校验和进 SBOM。

---

## 8. 路线图

1. `--root` 前缀 + fixture 测试 + golden JSON（可测试性）
2. `--apply` 修复框架（dry-run 默认，审计日志）
3. eBPF 事件采集（进程退出/exec/文件打开），接入同一套 `Finding`
4. 报告签名 + 增量上报（只传 diff，降低带宽）
5. 策略版本化（基线包带版本号与来源，支持灰度）
6. CIS/等保条款映射表自动生成（现在 `reference` 字段是手工标注）
