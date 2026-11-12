# platform-diagnose

设备故障**智能识别与根因分析**。Rust 库 + CLI。`platform-health` 回答"这台设备是什么、健康吗"，本 crate 回答"它为什么出问题"。

采样运行时信号（CPU/内存/IO/温度/网络/进程）在一个滑动窗口上，跑一套**确定性规则引擎**，跨 **应用 / 系统 / 主机 / 网络** 四个维度给出排序后的根因假设 + 证据 + 肇事进程 + 处置建议。

```console
$ platform-diagnose -s lag
symptom : lag/stutter
samples : 3
verdict : [system] Memory thrashing: 3476 swap-out pages/s (confidence 100%); +5 secondary finding(s)

>> [system ] memory.thrashing       conf 100%
     Memory thrashing: 3476 swap-out pages/s, 4190 major faults/s
       * offender: node-MainThread (pid 5265, role memory, 957 MB)
       -> Add RAM, reduce the working set, or cap the biggest memory consumer.
   [host   ] thermal.throttling     conf  90%
     Thermal throttling: k10temp Tctl at 84°C, frequency capped
```

## 为什么是规则引擎，不是机器学习

设备端 agent 必须**离线、可解释、免训练**。你们没有标注过的故障数据，ML 既训不动也说不清"为什么判定是它"。所以走**专家规则 + 置信度打分**：每条 `Finding` 都带触发它的证据信号、量化的置信度、和可读的处置建议。置信度 = 幅度（超阈值多少）+ 持续性（整个窗口都高 vs 单点尖峰），避免单个噪声样本误报。

## 四个维度与根因树

| 维度 | 你问的问题 | 检测的根因 |
|---|---|---|
| **应用** `app` | 什么应用占用最多？什么导致的？ | CPU 霸王、内存霸王、IO 霸王、D 状态阻塞、（Phase 3：FD/连接爆、健康检查超时、重启循环） |
| **系统** `system` | 卡顿是系统层吗？ | 内存抖动(换页)、内存耗尽、swap 抖动、CPU 满载、IO 等待、VM 抢占(steal)、僵尸堆积 |
| **主机** `host` | 底层硬件问题？ | 温度墙降频、盘满、盘报废(SMART)、ECC 内存错误 |
| **网络** `network` | 网络还是系统的问题？ | 高延迟、丢包/重传、DNS 慢/失败、目标不可达、网卡错包丢包、（Phase 2：带宽打满） |

**"网络 or 系统"的核心判别**（`triage.net_vs_local` 规则）：本地 CPU/内存/IO/换页都正常 **且** 网络探测差 → 明确指向网络路径或远端，让你别在本地瞎找。

## 用法

### CLI

```console
platform-diagnose -s lag                          # 卡顿排查
platform-diagnose -s timeout -t api:443 --dns api # 超时排查 + 探测该目标
platform-diagnose -s network --probes 1.1.1.1:443,10.0.0.1:80 --dns example.com
platform-diagnose -s generic -d 15 -i 1           # 全面体检，采样 15s
platform-diagnose -s lag --json                   # 机器可读
```

`-s`：`lag` / `timeout` / `network` / `app` / `resource` / `disk` / `generic`。
退出码：`0` 健康、`1` 有发现、`2` 高置信主因（≥70%）。

### 库

```rust
use platform_diagnose::{Diagnoser, Options, ProbeConfig, Symptom};
use std::time::Duration;

let opts = Options {
    window_size: 10,
    probe: Some(ProbeConfig {
        targets: vec!["1.1.1.1:443".into()],
        dns_name: Some("example.com".into()),
    }),
    collect_baseline: true,   // 采集 platform-health 快照，启用主机规则
    ..Options::default()
};
let mut d = Diagnoser::new(opts);
let report = d.sweep(&Symptom::Lag, Duration::from_secs(10), Duration::from_secs(1));

println!("{}", report.summary);
for f in &report.findings {
    println!("[{}] {} ({:.0}%)", f.dimension, f.cause.code(), f.confidence * 100.0);
    for o in &f.offenders { println!("  offender: {}", o.name); }
}
```

也可以后台 `d.observe()` 持续采样、症状出现时 `d.diagnose(&symptom)`。

## 采样与探测

- **被动**（默认，零开销、无需 root）：`/proc/stat`、`/proc/meminfo`、`/proc/vmstat`（换页/缺页速率）、`/proc/diskstats`、`/proc/net/dev`（错包丢包）、`/proc/net/snmp`（TCP 重传）、`/proc/net/tcp*`（socket 状态）、`/proc/<pid>/*`（进程 CPU/内存/IO/状态/FD）、hwmon（温度）、cpufreq（降频检测）。累计计数在采样器里折算成**每秒速率**，规则直接判"是否正在抖动"而非"swap 用了多少"。
- **主动**（opt-in，产生流量、无需 root、无额外依赖）：TCP connect 测 RTT 到多个目标、`to_socket_addrs` 测 DNS 耗时。真 ICMP 要 root，暂不做（用 TCP connect 代替）。

## 健康规则与阈值

`Thresholds` 全部可调（含 CPU 饱和、内存耗尽/抖动、IO 等待、steal、温度、进程霸王、网络重传/延迟/DNS 等），详见 `rules::Thresholds` 默认值。`platform-diagnose -s lag --json` 会带上每条证据的 `observed`/`threshold`。

## 与 platform-health 的关系

诊断时可选地采集一份 `platform-health` 快照作为**基线**（`collect_baseline`），主机维度规则（盘满/SMART 报废/ECC）从快照读取，不必重复实现。两个 crate 都是 MIT、无 GPL/AGPL。

## 开发

```console
cargo test                    # 13 个合成样本引擎测试
cargo clippy --all-targets -- -D warnings
cargo run -- -s lag -d 3
```

测试用**合成采样窗口**驱动规则引擎，证明"从信号 alone 能得出正确根因"，不需要真的把机器跑出故障。

## 边界与路线

- **Phase 1（当前）**：核心采样 + 规则引擎 + 上表全部根因 + 主动网络探测 + CLI。
- **Phase 2**：更多网络环境探测（丢包窗口统计、带宽、抖动）、多目标相关。
- **Phase 3**：应用深度（FD/连接数归因、健康检查超时、重启循环、OOM/dmesg 解析、iostat 式 per-disk await）。
- **Phase 4**：规则阈值外置配置、QEStudio 后端集成（按 `identity.fingerprint` 归并诊断）。
- **平台**：当前 Linux-only（读 `/proc`/sysfs）。Windows 的 `/proc` 等价物（PDH/WinAPI）可按需接。

## 许可证

MIT。依赖：`platform-health`(路径, MIT)、`serde`。无 GPL/AGPL。
