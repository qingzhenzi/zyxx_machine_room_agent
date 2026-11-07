//! `qe-agent vm`：本机虚拟机全生命周期管理（libvirt）。
//!
//! 一条命令覆盖「建盘 → 定义 → 装系统 → 日常开关机 → 看画面 → 删除」，
//! 不再需要记 `virsh` 的子命令。所有操作走 `virt-crate` 的运行时门面，
//! 后端不可用时给出可执行的下一步，而不是一句 "error"。

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};

use virt_core::{
    Accel, BootDevice, DisplayConfig, DisplayKind, DomainState, LoadRules, VmSpec, net::parse_nic,
};
use virt_crate::{AnyDomain, Backend};
use virt_libvirt::{DiskFormat, LibvirtHypervisor, create_disk, parse_size};

/// `qe-agent vm` 的参数：一个全局 URI + 子命令。
#[derive(Debug, Args)]
pub struct VmArgs {
    /// libvirt URI（默认 qemu:///system，不可用时回落 qemu:///session）
    #[arg(long, global = true, value_name = "URI")]
    pub uri: Option<String>,

    #[command(subcommand)]
    pub cmd: VmCmd,
}

#[derive(Debug, Subcommand)]
pub enum VmCmd {
    /// 列出虚拟机（等价 `virsh list --all`）
    Ls,

    /// 查看一台虚拟机的详情（磁盘、画面、自启动）
    Info {
        /// 虚拟机名（`vm ls` 的 NAME 列）
        name: String,
    },

    /// 创建一台虚拟机：建盘、写定义，可选挂 ISO 并直接启动
    Create(CreateArgs),

    /// 启动
    Start { name: String },

    /// 请求 guest 正常关机（ACPI，需要 guest 配合）
    Shutdown { name: String },

    /// 立即断电（相当于拔电源）
    Destroy { name: String },

    /// 重启
    Reboot { name: String },

    /// 暂停（挂起 CPU）
    Suspend { name: String },

    /// 从暂停恢复
    Resume { name: String },

    /// 删除虚拟机定义，可选连同磁盘文件一起删
    Rm {
        name: String,
        /// 同时删除磁盘镜像文件（不可恢复）
        #[arg(long)]
        with_disks: bool,
        /// 正在运行也强行断电后删除
        #[arg(long)]
        force: bool,
    },

    /// 开关「随宿主机启动」
    Autostart {
        name: String,
        /// on / off
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },

    /// 卸载安装 ISO（装完系统后必须执行，否则会一直从光盘启动）
    Eject { name: String },

    /// 打印 VNC 画面连接方式
    Vnc { name: String },

    /// 截图保存到文件（默认用驱动给的格式；-o 指定 .png/.ppm 时按需转换）
    Screenshot {
        name: String,
        /// 输出路径（默认 ./<name>.ppm）
        #[arg(short, long)]
        out: Option<PathBuf>,
    },

    /// 打开串口控制台（等价 `virsh console`，需要交互终端）
    Console { name: String },
}

/// `vm create` 的参数。
#[derive(Debug, Args)]
pub struct CreateArgs {
    /// 虚拟机名（同时作为 libvirt 域名）
    name: String,

    /// vCPU 数量
    #[arg(long, default_value_t = 2)]
    vcpus: u32,

    /// 内存（MiB）
    #[arg(long = "memory", value_name = "MiB", default_value_t = 2048)]
    memory_mib: u64,

    /// 磁盘镜像路径，可重复；不传则用 <data-dir>/vms/<name>/disk.qcow2
    #[arg(long)]
    disk: Vec<PathBuf>,

    /// 新建磁盘的大小（已存在的文件按原样使用）
    #[arg(long, default_value = "20G")]
    disk_size: String,

    /// 安装 ISO：挂成 CD-ROM 并优先从光盘启动
    #[arg(long)]
    iso: Option<PathBuf>,

    /// 网卡：bridge=br0 / network=default / user / none，可重复
    #[arg(long)]
    network: Vec<String>,

    /// 直通的物理 GPU PCI 地址（dddd:bb:dd.f），可重复
    #[arg(long = "gpu", value_name = "PCI")]
    gpu: Vec<String>,

    /// 图形控制台协议
    #[arg(long, value_enum, default_value = "vnc")]
    display: DisplayArg,

    /// VNC/SPICE 密码（监听非回环地址时必填）
    #[arg(long)]
    vnc_password: Option<String>,

    /// 随宿主机自动启动
    #[arg(long)]
    autostart: bool,

    /// 定义完立即启动
    #[arg(long)]
    start: bool,

    /// CPU 加速：auto=有 /dev/kvm 就用，否则 TCG 软件模拟
    #[arg(long, value_enum, default_value = "auto")]
    accel: AccelArg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DisplayArg {
    Vnc,
    Spice,
    None,
}

impl DisplayArg {
    fn to_core(self) -> DisplayKind {
        match self {
            Self::Vnc => DisplayKind::Vnc,
            Self::Spice => DisplayKind::Spice,
            Self::None => DisplayKind::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AccelArg {
    Auto,
    Kvm,
    Tcg,
}

impl AccelArg {
    fn to_core(self) -> Accel {
        match self {
            Self::Auto => Accel::Auto,
            Self::Kvm => Accel::Kvm,
            Self::Tcg => Accel::Tcg,
        }
    }
}

/// 入口：连接后端并分发子命令。
pub fn run(args: VmArgs, data_dir: &Path) -> Result<()> {
    let (hv, uri) = connect(args.uri.as_deref())?;
    match args.cmd {
        VmCmd::Ls => list(&hv),
        VmCmd::Info { name } => info(&hv, &name),
        VmCmd::Create(create) => create_vm(&hv, create, data_dir),
        VmCmd::Start { name } => {
            domain(&hv, &name)?.start()?;
            println!("已启动 {name}。看画面：qe-agent vm vnc {name}");
            Ok(())
        }
        VmCmd::Shutdown { name } => {
            domain(&hv, &name)?.shutdown()?;
            println!("已向 {name} 发送关机请求（guest 未装 ACPI 时不会响应，改用 destroy）");
            Ok(())
        }
        VmCmd::Destroy { name } => {
            domain(&hv, &name)?.destroy()?;
            println!("已强制断电 {name}");
            Ok(())
        }
        VmCmd::Reboot { name } => {
            domain(&hv, &name)?.reboot()?;
            println!("已重启 {name}");
            Ok(())
        }
        VmCmd::Suspend { name } => {
            domain(&hv, &name)?.suspend()?;
            println!("已暂停 {name}");
            Ok(())
        }
        VmCmd::Resume { name } => {
            domain(&hv, &name)?.resume()?;
            println!("已恢复 {name}");
            Ok(())
        }
        VmCmd::Rm {
            name,
            with_disks,
            force,
        } => remove(&hv, &name, with_disks, force),
        VmCmd::Autostart { name, state } => {
            let dom = domain(&hv, &name)?;
            dom.set_autostart(state == "on")?;
            println!(
                "{} 的自启动已{}",
                name,
                if state == "on" { "开启" } else { "关闭" }
            );
            Ok(())
        }
        VmCmd::Eject { name } => eject(&hv, &name),
        VmCmd::Vnc { name } => vnc(&hv, &name),
        VmCmd::Screenshot { name, out } => screenshot(&hv, &name, out),
        VmCmd::Console { name } => console(&uri, &name),
    }
}

/// 连接 libvirt：显式 URI 优先，否则 system → session 回落。
fn connect(uri: Option<&str>) -> Result<(Backend, String)> {
    if let Some(uri) = uri {
        let hv = LibvirtHypervisor::connect_uri(uri)
            .with_context(|| format!("连接 libvirt 失败：{uri}"))?;
        return Ok((Backend::from(hv), uri.to_owned()));
    }
    let (hv, used) = LibvirtHypervisor::connect_best().context(
        "连接 libvirt 失败：确认 libvirtd 在运行（systemctl status libvirtd），\
         或在 root 下操作；本机体验可用 --uri qemu:///session",
    )?;
    if used != virt_libvirt::DEFAULT_URI {
        eprintln!(
            "提示：qemu:///system 不可用，已使用 {used}；\
             该连接下定义的虚拟只对当前用户可见。"
        );
    }
    Ok((Backend::from(hv), used))
}

fn domain(hv: &Backend, name: &str) -> Result<AnyDomain> {
    hv.domain(name)
        .with_context(|| format!("找不到虚拟机 {name}（qe-agent vm ls 查看现有清单）"))
}

fn list(hv: &Backend) -> Result<()> {
    let domains = hv.list_domains().context("读取虚拟机清单失败")?;
    if domains.is_empty() {
        println!("（没有已定义的虚拟机。创建：qe-agent vm create <名字> --iso <安装镜像>）");
        return Ok(());
    }
    println!(
        "{:<24} {:<12} {:>5} {:>10} {:<4}",
        "NAME", "STATE", "VCPU", "MEM", "AUTO"
    );
    for d in domains {
        let autostart = hv
            .domain(&d.name)
            .and_then(|dom| dom.autostart())
            .map(|on| if on { "on" } else { "-" })
            .unwrap_or("-");
        println!(
            "{:<24} {:<12} {:>5} {:>10} {:<4}",
            d.name,
            state_label(d.state),
            d.vcpus,
            format!("{} MiB", d.max_memory_kib / 1024),
            autostart,
        );
    }
    Ok(())
}

fn info(hv: &Backend, name: &str) -> Result<()> {
    let dom = domain(hv, name)?;
    let snapshot = dom.info().context("读取虚拟机状态失败")?;

    println!("名称     : {}", snapshot.name);
    println!(
        "ID       : {}",
        if snapshot.id.is_unset() {
            "—（未运行）".to_owned()
        } else {
            snapshot.id.to_string()
        }
    );
    println!("状态     : {}", state_label(snapshot.state));
    println!("vCPU     : {}", snapshot.vcpus);
    println!(
        "内存     : {} MiB（当前 {} MiB）",
        snapshot.max_memory_kib / 1024,
        snapshot.memory_kib / 1024
    );
    println!(
        "持久化   : {}",
        if dom.persistent().unwrap_or(false) {
            "是"
        } else {
            "否"
        }
    );
    println!(
        "自启动   : {}",
        if dom.autostart().unwrap_or(false) {
            "是"
        } else {
            "否"
        }
    );

    let disks = dom.disk_paths().unwrap_or_default();
    if disks.is_empty() {
        println!("磁盘     : —");
    } else {
        for (index, disk) in disks.iter().enumerate() {
            let label = if index == 0 { "磁盘" } else { "     " };
            println!("{label}     : {disk}");
        }
    }

    match dom.console() {
        Ok(Some(gfx)) => {
            println!("画面     : {} {}:{}", gfx.kind.as_str(), gfx.host, gfx.port);
            if let Some(display) = gfx.vnc_display() {
                println!("           display :{display}");
            }
        }
        Ok(None) => println!("画面     : 无图形控制台（headless，用 vm console）"),
        Err(error) => println!("画面     : 读取失败（{error}）"),
    }
    Ok(())
}

fn create_vm(hv: &Backend, args: CreateArgs, data_dir: &Path) -> Result<()> {
    // 用清单判断重名，而不是 lookup：后者在正常情况下也会让 libvirt
    // 往 stderr 打一行 "Domain not found"，看起来像出错。
    let exists = hv
        .list_domains()
        .map(|domains| domains.iter().any(|d| d.name == args.name))
        .unwrap_or(false);
    if exists {
        bail!(
            "虚拟机 {} 已存在；改个名字，或先 qe-agent vm rm {}",
            args.name,
            args.name
        );
    }

    let name = args.name.clone();
    let disks: Vec<PathBuf> = if args.disk.is_empty() {
        vec![data_dir.join("vms").join(&name).join("disk.qcow2")]
    } else {
        args.disk.clone()
    };

    let size = parse_size(&args.disk_size).context("解析 --disk-size 失败")?;
    let mut disk_paths = Vec::with_capacity(disks.len());
    for disk in &disks {
        if disk.exists() {
            println!("使用已有磁盘 {}", disk.display());
        } else {
            let format = DiskFormat::for_path(disk);
            create_disk(disk, size, format).with_context(|| {
                format!(
                    "创建磁盘 {} 失败（qcow2 需要 qemu-img；可设 QE_QEMU_IMG 指定路径）",
                    disk.display()
                )
            })?;
            println!(
                "已创建 {} 磁盘 {}（{}）",
                format.as_str(),
                disk.display(),
                args.disk_size
            );
        }
        disk_paths.push(disk.to_string_lossy().into_owned());
    }

    let cdrom = args
        .iso
        .as_ref()
        .map(|iso| iso.to_string_lossy().into_owned());

    let mut spec = VmSpec {
        name: name.clone(),
        vcpus: args.vcpus,
        memory_kib: args.memory_mib * 1024,
        disks: disk_paths,
        display: DisplayConfig {
            kind: args.display.to_core(),
            password: args.vnc_password.clone(),
            listen: None,
        },
        cdrom,
        boot: if args.iso.is_some() {
            BootDevice::Cdrom
        } else {
            BootDevice::Hd
        },
        accel: args.accel.to_core(),
        autostart: args.autostart,
        ..Default::default()
    };
    for network in &args.network {
        spec.nics
            .push(parse_nic(network).with_context(|| format!("解析 --network {network:?} 失败"))?);
    }
    for raw in &args.gpu {
        spec.gpus.push(
            virt_core::GpuPassthrough::new(raw.clone())
                .with_context(|| format!("解析 --gpu {raw:?} 失败（期望 dddd:bb:dd.f）"))?,
        );
        println!("将直通 GPU {raw}（宿主机需已绑定 vfio-pci，见 docs/gpu-passthrough.md）");
    }

    let dom = hv
        .define(&spec, &LoadRules::default())
        .context("定义虚拟机失败：检查磁盘/ISO 路径是否存在；qemu:///system 权限不足时改用 --uri qemu:///session")?;
    println!("已定义虚拟机 {name}");

    if args.start {
        dom.start()
            .context("启动失败（若 accel=auto 已回落到 TCG 仍失败，看 libvirt 日志）")?;
        println!("已启动 {name}");
        if let Some(hint) = vnc_hint(&dom) {
            println!("{hint}");
        } else if args.display == DisplayArg::None {
            println!("无图形控制台；文本登录：qe-agent vm console {name}");
        }
        if args.iso.is_some() {
            println!(
                "正在从 ISO 启动；装完系统后执行 `qe-agent vm eject {name}`，否则会重复进安装界面"
            );
        }
    }
    Ok(())
}

fn remove(hv: &Backend, name: &str, with_disks: bool, force: bool) -> Result<()> {
    let dom = domain(hv, name)?;
    if dom.state().map(|s| s.is_active()).unwrap_or(false) {
        if !force {
            bail!("{name} 正在运行；先 qe-agent vm shutdown {name}，或加 --force 强制断电");
        }
        dom.destroy().context("强制断电失败")?;
        println!("已强制断电 {name}");
    }

    let disks = dom.disk_paths().unwrap_or_default();
    dom.undefine().context("删除定义失败")?;
    println!("已删除定义 {name}");

    if with_disks {
        for disk in disks {
            match std::fs::remove_file(&disk) {
                Ok(()) => println!("已删除磁盘 {disk}"),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    println!("磁盘已不存在 {disk}");
                }
                Err(error) => eprintln!("删除磁盘 {disk} 失败：{error}"),
            }
        }
    } else if !disks.is_empty() {
        println!("磁盘文件保留（不再使用时加 --with-disks 一并删除）");
    }
    Ok(())
}

fn eject(hv: &Backend, name: &str) -> Result<()> {
    match hv.eject_cdrom(name)? {
        true => println!("已卸载 {name} 的光盘，下次启动从硬盘引导"),
        false => println!("{name} 没有挂载光盘，无需操作"),
    }
    Ok(())
}

fn vnc(hv: &Backend, name: &str) -> Result<()> {
    let dom = domain(hv, name)?;
    let state = dom.state().unwrap_or(DomainState::Unknown);
    let consoles = dom.graphics().context("读取图形控制台失败")?;

    if consoles.is_empty() {
        bail!(
            "虚拟机 {name} 没有图形控制台（headless）。\n\
             文本登录：qe-agent vm console {name}\n\
             只要截图：qe-agent vm screenshot {name}"
        );
    }

    println!("虚拟机 : {name} ({})", state_label(state));
    if !state.is_active() {
        println!("提示   : 未运行，端口要启动后才分配；先 qe-agent vm start {name}");
    }
    for gfx in consoles {
        println!("协议   : {}", gfx.kind.as_str());
        if gfx.port == 0 {
            println!("端点   : 尚未分配（虚拟机启动后 libvirt 才挑端口）");
            continue;
        }
        println!("端点   : {}:{}", gfx.host, gfx.port);
        if gfx.password {
            println!("认证   : 需要密码");
        }
        if let Some(display) = gfx.vnc_display() {
            println!();
            println!("本机看    : vncviewer 127.0.0.1:{}", gfx.port);
            println!("            没装 vncviewer 的话，Remmina / TigerVNC 连同一地址也可以");
            println!();
            println!(
                "别的机器看: ssh -L {port}:127.0.0.1:{port} <user>@<这台机器地址>",
                port = gfx.port
            );
            println!(
                "            然后在你的机器上 vncviewer 127.0.0.1:{}",
                gfx.port
            );
            println!("            （VNC 默认只听 127.0.0.1，走 SSH 隧道即可，别开到公网）");
            println!();
            println!("只要截图  : qe-agent vm screenshot {name} -o {name}.png");
            println!("            （display :{display}）");
        }
    }
    Ok(())
}

fn screenshot(hv: &Backend, name: &str, out: Option<PathBuf>) -> Result<()> {
    let image = hv
        .screenshot_of(name)
        .with_context(|| format!("截图失败（{name} 必须在运行且有图形控制台）"))?;
    let native = ImageKind::detect(&image).with_context(|| {
        format!(
            "无法识别截图格式（前 8 字节：{:?}）",
            &image[..image.len().min(8)]
        )
    })?;

    let out = out.unwrap_or_else(|| PathBuf::from(format!("{name}.{}", native.extension())));
    let wanted = match out.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("png") => ImageKind::Png,
        Some(ext) if ext.eq_ignore_ascii_case("ppm") || ext.eq_ignore_ascii_case("pnm") => {
            ImageKind::Pnm
        }
        _ => {
            bail!(
                "输出文件只支持 .png / .ppm：{}（libvirt 本次返回 {}）",
                out.display(),
                native.extension()
            )
        }
    };

    if wanted == native {
        std::fs::write(&out, &image).with_context(|| format!("写入 {} 失败", out.display()))?;
        println!(
            "已保存截图 {}（{} KiB，{}）",
            out.display(),
            image.len() / 1024,
            native.label()
        );
        return Ok(());
    }

    // 格式不一致时借 ImageMagick 转一次，中间文件用完即删。
    let temp = out.with_extension(native.extension());
    std::fs::write(&temp, &image).with_context(|| format!("写入 {} 失败", temp.display()))?;
    convert_image(&temp, &out)?;
    println!(
        "已保存截图 {}（{} KiB，{} → {}）",
        out.display(),
        image.len() / 1024,
        native.label(),
        wanted.label()
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageKind {
    Png,
    Pnm,
}

impl ImageKind {
    fn detect(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
            Some(Self::Png)
        } else if bytes.starts_with(b"P6") || bytes.starts_with(b"P3") {
            Some(Self::Pnm)
        } else {
            None
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Pnm => "ppm",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Pnm => "PNM",
        }
    }
}

/// 走 ImageMagick 转格式；没有就明确告诉用户装什么。
fn convert_image(source: &Path, target: &Path) -> Result<()> {
    let mut last = String::new();
    for bin in ["magick", "convert"] {
        match Command::new(bin).arg(source).arg(target).status() {
            Ok(status) if status.success() => {
                let _ = std::fs::remove_file(source);
                return Ok(());
            }
            Ok(status) => last = format!("{bin} 退出码 {status}"),
            Err(error) => last = format!("{bin}: {error}"),
        }
    }
    let _ = std::fs::remove_file(source);
    bail!("图片格式转换失败（{last}）；装 imagemagick，或直接用原生格式输出")
}

fn console(uri: &str, name: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;

    println!(
        "连接 {name} 的串口控制台（退出：Ctrl+ ]）。若 guest 没配 console=ttyS0，这里会一片空白。"
    );
    let error = Command::new("virsh")
        .args(["-c", uri, "console", name])
        .exec();
    // exec 只在失败时返回。
    Err(anyhow::anyhow!(
        "无法启动 virsh（{error}）；可直接执行：virsh -c {uri} console {name}"
    ))
}

/// 启动后打印一行「去哪看画面」。
fn vnc_hint(dom: &AnyDomain) -> Option<String> {
    let gfx = dom.console().ok().flatten()?;
    let mut hint = format!("画面：{} {}:{}", gfx.kind.as_str(), gfx.host, gfx.port);
    if let Some(display) = gfx.vnc_display() {
        hint.push_str(&format!(
            "（vncviewer 127.0.0.1:{port}，display :{display}）",
            port = gfx.port
        ));
    }
    Some(hint)
}

/// `DomainState` → virsh 风格状态词。
pub fn state_label(state: DomainState) -> &'static str {
    match state {
        DomainState::Running => "running",
        DomainState::ShutOff => "shut off",
        DomainState::Paused => "paused",
        DomainState::ShuttingDown => "shutting down",
        DomainState::Crashed => "crashed",
        _ => "unknown",
    }
}
