# virt-crate

Rust 封装库：统一 trait 层管理 Linux 平台虚拟机（KVM / libvirt），底层复用成熟库，不重造轮子。

## 结构

| crate | 作用 | 状态 |
|---|---|---|
| `virt-core` | Hypervisor 无关的 trait、类型、配置解析、XML 工具（零后端依赖） | 可用 |
| `virt-libvirt` | libvirtd 后端：建盘 / 定义 / 启停 / 截图 / 自启动 / 图形控制台 | 可用（生产在用） |
| `virt-kvm` | `/dev/kvm` 后端：VM 对象 + vCPU + 端口/MMIO 退出 | 仅低层，无法 boot guest |

## 能力边界（先看这张表，别被 trait 名字骗了）

| 能力 | libvirt 后端 | KVM 后端 |
|---|---|---|
| 定义持久化虚拟机 | ✅ 生成 XML / 直接吃 XML | ❌ 无配置存储 |
| 创建 qcow2/raw 磁盘 | ✅ `create_disk()`（qcow2 走 `qemu-img`） | ❌ |
| 启动 / 关机 / 重启 / 暂停 | ✅ | ❌ 返回 `Unsupported` |
| 列出 / 查询状态 / 磁盘清单 | ✅ 去重、按名排序 | ✅ 仅进程内登记表 |
| 随宿主机自启动 | ✅ | ❌ |
| 图形控制台（VNC/SPICE） | ✅ `Domain::graphics()` | ❌ |
| 截图（PNG / PNM） | ✅ `Domain::screenshot()` | ❌ |
| 串口控制台 | ✅ 默认渲染 pty serial | ❌ |
| 内存映射 + 跑 guest 代码 | ❌ | ✅ `RawVm`/`RawVcpu` |
| 安装 ISO / 弹出光盘 | ✅ `cdrom` + `eject_cdrom()` | ❌ |

`virt-kvm` **不是** VMM：KVM 只给 VM 对象、内存槽和 vCPU，没有 BIOS、设备模型、引导加载。
要跑完整虚拟机请用 `virt-libvirt`；要自己写 VMM 再用 `virt-kvm` 的低层接口。

## 两层抽象

### 1. 生命周期层（类 virsh）

```rust
use virt_core::{Domain, Hypervisor, SpecLoader, LoadRules};
use virt_libvirt::LibvirtHypervisor;

let hv = LibvirtHypervisor::connect()?;              // qemu:///system
for d in hv.list_domains()? {
    println!("{} {:?}", d.name, d.state);
}
let dom = hv.domain_by_name("debian12")?;
dom.start()?;
for gfx in dom.graphics()? {                          // 画面端点
    println!("{} {}", gfx.kind.as_str(), gfx.endpoint());
}
```

从配置文件定义一台（`key = value`）：

```text
name = demo
vcpus = 2
memory_kib = 2097152
disk = /var/lib/libvirt/images/demo.qcow2
nic = virtio,bridge=br0
display = vnc,listen=127.0.0.1
```

```rust
let spec = virt_core::load_spec_file("demo.conf")?;
let dom = hv.load(&spec, &LoadRules::default())?;    // 校验 + define，不启动
dom.start()?;
```

也可以直接吃 libvirt 原生 XML（`load_spec_file` 按首字符 `<` 自动识别），XML 会原样透传给 libvirt；
`<name>` / `<vcpu>` / `<memory>` 会被解析出来供规则校验，其余字段不做归一化。

### 2. 低层构造层（KVM ioctl 风格）

```rust
use virt_core::{RawVcpu, RawVm, RunMode, VcpuExit, VmBuilder};

let (vm, mut vcpus) = VmBuilder::new().vcpus(1).memory_bytes(1 << 20)
    .build(|_cfg| Ok(domain.clone()))?;
let slot = unsafe { vm.set_user_memory_region(0, size, host_addr)? };   // 返回槽位号
loop {
    match vcpus[0].run(RunMode::Run, &mut handler)? {
        VcpuExit::Hlt => break,
        VcpuExit::Io { port, is_write } => { /* 数据在 handler 里 */ }
        _ => {}
    }
}
```

退出数据走 `IoHandler` 而不是返回值，因为 `in` / MMIO 读要求调用者**回填**缓冲区，
KVM 会在下一次 run 时把它写回 guest。这是旧版把 `data` 塞进返回值时做不到的事。

`RawVcpu::as_any_mut()` 提供架构相关的下行转换，用于设置段基址 / RIP 等引导前状态：

```rust
let vcpu = vcpus[0].as_any_mut().downcast_mut::<virt_kvm::KvmVcpu>().unwrap();
vcpu.set_real_mode_entry(0)?;   // 16 位实模式，CS base = 0
```

完整可运行示例：`cargo run --features kvm-backend --example lowlevel-kvm`
（真的往 0xB8000 写字符，然后断言内存里确实是那个字符；需要 `/dev/kvm`）。

## 命令行：`qe-agent vm`

库里的能力都接到了 `qe-agent` 上，房间机器不用再记 `virsh` 参数：

```bash
# 1. 建一台并装系统（没有现成镜像时）
qe-agent vm create debian12 --vcpus 2 --memory 2048 \
    --disk /var/lib/qe/vms/debian12.qcow2 --disk-size 20G \
    --iso /var/lib/qe/iso/debian-13.iso --start
# 自动建 qcow2、挂 CD-ROM、从光盘启动；--start 后直接打印 VNC 地址

# 2. 看画面 / 截图
qe-agent vm vnc debian12
qe-agent vm screenshot debian12 -o debian12.png

# 3. 装完系统弹掉光盘，否则重启又进安装界面
qe-agent vm eject debian12

# 4. 日常
qe-agent vm ls
qe-agent vm info debian12
qe-agent vm start debian12      # 也有 shutdown / reboot / suspend / resume / destroy
qe-agent vm autostart debian12 on

# 5. 删除（--with-disks 连磁盘一起删）
qe-agent vm rm debian12 --with-disks
```

不传 `--disk` 时默认落在 `<data-dir>/vms/<name>/disk.qcow2`。
`--accel auto`（默认）在 `/dev/kvm` 可用时用 KVM，否则回落到 TCG 软件模拟
（慢，但在没有嵌套虚拟化的机器上也能跑）；`--accel kvm` 强制要求 KVM。
`--uri` 可指定 libvirt 连接，默认 `qemu:///system`，不可用时回落 `qemu:///session`。

`vm create` 的等价库调用：

```rust
use std::path::Path;
use virt_core::{DisplayConfig, Domain, LoadRules, SpecLoader, VmSpec};
use virt_libvirt::{create_disk, DiskFormat, LibvirtHypervisor};

let (hv, _uri) = LibvirtHypervisor::connect_best()?;
create_disk(Path::new("/var/lib/qe/vms/demo.qcow2"), 20 << 30, DiskFormat::Qcow2)?;
let spec = VmSpec {
    name: "demo".into(),
    vcpus: 2,
    memory_kib: 2 * 1024 * 1024,
    disks: vec!["/var/lib/qe/vms/demo.qcow2".into()],
    display: DisplayConfig::default(),
    ..Default::default()
};
let dom = hv.load(&spec, &LoadRules::default())?;   // 校验 + define，不启动
dom.start()?;
```

`connect_best()` 返回实际使用的 URI：`virsh` 等辅助命令必须用同一个，否则看不到域。
`QE_VIRT_URI` 可强制指定连接。

## 怎么看虚拟机画面

画面就是 QEMU 内建的 VNC/SPICE 服务。三步：确认有图形设备 → 拿端口 → 连上去。

**1. 确认配置里有 `<graphics>`。** 本 crate 生成的 XML 默认带：

```xml
<graphics type='vnc' port='-1' autoport='yes' listen='127.0.0.1'>
  <listen type='address' address='127.0.0.1'/>
</graphics>
```

`autoport='yes'` 让 libvirt 自动挑空闲端口；`listen='127.0.0.1'` 只监听本机回环，这是安全默认值。

**2. 拿实际端口。** 有三种等价办法：

```bash
qe-agent vm vnc debian12          # 本 crate 封装，直接打印可复制的命令
virsh vncdisplay debian12         # 输出 127.0.0.1:0（display 号，端口 = 5900 + N）
virsh dumpxml debian12 | grep graphics
```

`Domain::graphics()` 返回 `GraphicsInfo { kind, host, port, password }`，
`vnc_display()` 给出 display 号（端口 5900 → `:0`，5901 → `:1`）。

**3. 连上去。**

- 本机看图：`vncviewer 127.0.0.1:5900`（Remmina / TigerVNC / RealVNC 同理）
- 从别的机器看图：VNC 只听回环，走 SSH 隧道最安全

  ```bash
  ssh -L 5900:127.0.0.1:5900 user@room-pc
  # 然后在你自己机器上
  vncviewer 127.0.0.1:5900
  ```

- 只要一张截图，不装客户端：`qe-agent vm screenshot debian12 -o a.png`
  （libvirt 会直接给 PNG；老驱动给 PNM，指定 `.png` 时用 ImageMagick 转一次）
- 只有文本需求：`qe-agent vm console debian12`，走 pty 串口（生成的 XML 默认带 `<serial type='pty'>`）
- 想要更流畅、带声音：把 `--display spice` 建一台，用 `remote-viewer` 连

**安全提醒：** 如果把 `listen` 改成 `0.0.0.0` 让外网直连，必须同时设密码，
否则等于把键盘鼠标交给整个网段。`LoadRules::require_display_auth`（默认开启）会直接拒绝
"公网监听 + 无密码"的配置，`render_xml` 也不会自动放开监听地址。

## 运行时切换后端

```rust
use virt_core::Domain;
use virt_crate::Backend;

let hv = Backend::auto()?;       // 优先 libvirt，没有则回退 KVM
println!("{} {}", hv.name(), hv.version_string()?);
for vm in hv.list_domains()? {
    println!("{} {:?}", vm.name, vm.state);
}

// 生命周期与截图也走门面；KVM 后端会诚实返回 Unsupported
let dom = hv.domain("debian12")?;
dom.start()?;
let png = hv.screenshot_of("debian12")?;
```

`Backend` 是运行时的 `enum` 门面 —— trait 本身带关联类型，`Box<dyn Hypervisor>` 只能指定单个
`Domain` 类型，跨后端切换得靠这层枚举。

## 使用

```toml
[dependencies]
virt-crate = { path = "…", features = ["libvirt-backend"] }   # 或 kvm-backend
```

示例：

```bash
cargo run --all-features --example lifecycle          # libvirt 全流程 + KVM 登记
cargo run --features kvm-backend --example lowlevel-kvm
```

## 测试

```bash
cargo test -p virt-core -p virt-libvirt     # 39 个用例
cargo test -p virt-libvirt -- --ignored     # 1 个真实 QEMU 全流程（需要 libvirtd + qemu）
cargo test -p virt-kvm -- --ignored         # 5 个用例，需要 /dev/kvm
```

- `virt-core`：16 个，配置 / 网卡 / XML / 安装参数解析，纯逻辑。
- `virt-libvirt`：23 个。其中 `defines_disk_domain_for_real` 真的连 `qemu:///session`
  定义一台带磁盘的 VM（`dev='v'` 非法设备名的回归测试）；
  `full_lifecycle_with_real_qemu`（默认 `#[ignore]`）会建 qcow2、定义、启动、
  等 VNC 端口、截图、断电、删除，是一次真正的端到端验证。
- `virt-kvm`：5 个，默认 `#[ignore]`；其中 `runs_real_mode_guest_until_hlt` 真的建 VM、
  映射内存、跑 16 位实模式代码并断言它改写了 VGA 文本缓冲，`lowlevel-kvm` 示例做同样的事。

## 已知限制

- 仅支持 Linux。
- 生成的域 XML 固定 x86_64 + q35 + SeaBIOS；其他架构或需要 UEFI 时请直接给 `raw_xml`。
- `Accel::Auto` 只检查 `/dev/kvm` 是否可打开，不探测嵌套虚拟化能力；打开失败才回落 TCG。
- `virt-libvirt` 需要系统装 libvirt（`virt` crate 链接 `libvirt.so`），建盘还需要 `qemu-img`。
- `is_available()` 会真连一次 libvirtd，别在轮询里反复调用；持有 `LibvirtHypervisor` 复用连接。
- 截图格式由驱动决定（QEMU ≥ 7 为 PNG，老驱动为 PNM），写文件前先看 magic bytes。
- KVM 后端不追踪 guest 运行状态（`Domain::state()` 恒为 `ShutOff`），因为它不负责跑 guest。
