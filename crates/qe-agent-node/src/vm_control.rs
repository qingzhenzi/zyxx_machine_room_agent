//! VM 远程控制执行器（libvirt + qemu-img）。
//!
//! 阶段 1：list / start / shutdown / reboot / destroy / screenshot；
//! 阶段 1.5：create（底包+差分盘）/ snapshot.* / disk.info / export。
//!
//! 契约（控制面 ↔ agent，`vm.command` 下行）：
//!
//! ```json
//! {"request_id":"<uuid>","op":"<op>","args":{...}}
//! ```
//!
//! op 一览：
//!
//! | op | args | data |
//! |----|------|------|
//! | `list` | — | `{"vms":[{name,state,vcpus,memory_bytes,vnc}]}` |
//! | `start`/`shutdown`/`reboot`/`destroy` | `{name}` | `{name,result:"ok"}` |
//! | `screenshot` | `{name}` | `{format:"png",image_base64,size_bytes}`（≤4MiB） |
//! | `create` | `{name,vcpus?,memory_mib?,disk_gb?,base_volume?,base_path?,gpu?[],start?}` | `{name,disk_path,base,defined,started}` |
//! | `snapshot.create`/`restore`/`delete` | `{name,snapshot,description?}` | `{name,snapshot}` |
//! | `snapshot.list` | `{name}` | `{snapshots:[{id,tag,vm_size,date,vm_clock,icount}]}` |
//! | `disk.info` | `{name}` | `{disks:[{path,format,virtual_size,backing_path,backing_format}],chain:[...]}` |
//! | `export` | `{name,format?="qcow2"}` | `{path,sha256,size_bytes}` |
//!
//! 约束：
//! - 除 `list` 外都必须有合法 `name`（`[A-Za-z0-9._-]{1,64}`，防路径注入）；
//! - `create` 的 `base_volume`（libvirt 卷，`virsh vol-path` 解析）与 `base_path`
//!   二选一；差分盘 `qemu-img create -b <base> -F qcow2`；
//! - `snapshot.*` / `export` 要求域 **shut off**（运行中返回明确错误）；
//! - 每命令超时：普通 30s、截图 20s、create/export 120s；阻塞调用在
//!   `spawn_blocking` 中执行，panic 只让该条命令失败。
//!
//! 结果通过 `POST /machine/command/result` 回传（见 `run.rs` 的 `vm.command` 订阅）。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use virt_crate::{Backend, DomainState, GpuPassthrough, GraphicsInfo, LoadRules, VmSpec};

/// 普通命令超时。
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// 截图超时。
pub const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(20);
/// create / export 这类可能搬大文件的命令超时。
pub const HEAVY_TIMEOUT: Duration = Duration::from_secs(120);
/// 截图 base64 字符串上限（4 MiB）。
pub const MAX_SCREENSHOT_BASE64_BYTES: usize = 4 * 1024 * 1024;
/// 单台 VM 最多直通 GPU 数（与 LoadRules 默认一致）。
pub const MAX_GPUS: usize = 2;

/// 下行 `vm.command` 的操作类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum VmOp {
    #[serde(rename = "list")]
    List,
    #[serde(rename = "start")]
    Start,
    #[serde(rename = "shutdown")]
    Shutdown,
    #[serde(rename = "reboot")]
    Reboot,
    #[serde(rename = "destroy")]
    Destroy,
    #[serde(rename = "screenshot")]
    Screenshot,
    #[serde(rename = "create")]
    Create,
    #[serde(rename = "snapshot.create")]
    SnapshotCreate,
    #[serde(rename = "snapshot.list")]
    SnapshotList,
    #[serde(rename = "snapshot.restore")]
    SnapshotRestore,
    #[serde(rename = "snapshot.delete")]
    SnapshotDelete,
    #[serde(rename = "disk.info")]
    DiskInfo,
    #[serde(rename = "export")]
    Export,
}

impl VmOp {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Start => "start",
            Self::Shutdown => "shutdown",
            Self::Reboot => "reboot",
            Self::Destroy => "destroy",
            Self::Screenshot => "screenshot",
            Self::Create => "create",
            Self::SnapshotCreate => "snapshot.create",
            Self::SnapshotList => "snapshot.list",
            Self::SnapshotRestore => "snapshot.restore",
            Self::SnapshotDelete => "snapshot.delete",
            Self::DiskInfo => "disk.info",
            Self::Export => "export",
        }
    }

    const fn requires_name(self) -> bool {
        !matches!(self, Self::List)
    }

    const fn timeout(self) -> Duration {
        match self {
            Self::Screenshot => SCREENSHOT_TIMEOUT,
            Self::Create | Self::Export => HEAVY_TIMEOUT,
            _ => COMMAND_TIMEOUT,
        }
    }

    const fn snapshot_action(self) -> Option<SnapshotAction> {
        match self {
            Self::SnapshotCreate => Some(SnapshotAction::Create),
            Self::SnapshotList => Some(SnapshotAction::List),
            Self::SnapshotRestore => Some(SnapshotAction::Restore),
            Self::SnapshotDelete => Some(SnapshotAction::Delete),
            _ => None,
        }
    }
}

/// `vm.command` 的 `args`（各 op 取自己需要的字段）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VmCommandArgs {
    #[serde(default)]
    pub name: Option<String>,
    // create
    #[serde(default)]
    pub vcpus: Option<u32>,
    #[serde(default)]
    pub memory_mib: Option<u64>,
    #[serde(default)]
    pub disk_gb: Option<u64>,
    #[serde(default)]
    pub base_volume: Option<String>,
    #[serde(default)]
    pub base_path: Option<String>,
    #[serde(default)]
    pub gpu: Vec<String>,
    #[serde(default)]
    pub start: Option<bool>,
    // snapshot / export
    #[serde(default)]
    pub snapshot: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
}

/// 一条下行命令。
#[derive(Debug, Clone, Deserialize)]
pub struct VmCommand {
    pub request_id: Uuid,
    pub op: VmOp,
    #[serde(default)]
    pub args: VmCommandArgs,
}

impl VmCommand {
    /// 解析并做入参校验。
    pub fn parse(payload: &str) -> Result<Self> {
        let command: Self =
            serde_json::from_str(payload).context("vm.command payload 不是合法 JSON / 缺字段")?;
        command.validate()?;
        Ok(command)
    }

    /// 入参校验（所有 op 共用；错误信息给控制面直接展示）。
    pub fn validate(&self) -> Result<()> {
        if self.op.requires_name() {
            let name = self
                .args
                .name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| anyhow::anyhow!("op={} 需要 args.name", self.op.as_str()))?;
            validate_vm_name(name)?;
        }
        match self.op {
            VmOp::Create => {
                self.create_args()?;
            }
            VmOp::SnapshotCreate | VmOp::SnapshotRestore | VmOp::SnapshotDelete => {
                self.snapshot_tag()?;
            }
            VmOp::Export => {
                let format = self.export_format();
                if format != "qcow2" {
                    bail!("export 目前只支持 format=qcow2，收到 {format:?}");
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// `args.name`（已校验的调用方可用）。
    pub fn name(&self) -> &str {
        self.args.name.as_deref().map(str::trim).unwrap_or_default()
    }

    fn target_name(&self) -> String {
        self.name().to_owned()
    }

    fn timeout(&self) -> Duration {
        self.op.timeout()
    }

    fn export_format(&self) -> String {
        self.args
            .format
            .as_deref()
            .map(str::trim)
            .filter(|format| !format.is_empty())
            .unwrap_or("qcow2")
            .to_ascii_lowercase()
    }

    /// `snapshot.tag` 校验（qcow2 内部快照名，限制可打印安全字符）。
    fn snapshot_tag(&self) -> Result<String> {
        let tag = self
            .args
            .snapshot
            .as_deref()
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .ok_or_else(|| anyhow::anyhow!("op={} 需要 args.snapshot", self.op.as_str()))?;
        if tag.len() > 64
            || !tag
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            bail!("非法快照名: {tag:?}（允许 [A-Za-z0-9._-]{{1,64}}）");
        }
        Ok(tag.to_owned())
    }

    /// 把 `create` 的 args 归一化 + 校验成 [`CreateArgs`]。
    pub fn create_args(&self) -> Result<CreateArgs> {
        let name = self.name();
        validate_vm_name(name)?;
        let vcpus = self.args.vcpus.unwrap_or(2);
        if !(1..=64).contains(&vcpus) {
            bail!("vcpus 越界: {vcpus}（允许 1..=64）");
        }
        let memory_mib = self.args.memory_mib.unwrap_or(2048);
        if !(256..=262_144).contains(&memory_mib) {
            bail!("memory_mib 越界: {memory_mib}（允许 256..=262144）");
        }
        let disk_gb = self.args.disk_gb.unwrap_or(20);
        if !(1..=2048).contains(&disk_gb) {
            bail!("disk_gb 越界: {disk_gb}（允许 1..=2048）");
        }
        let base_volume = clean_optional(self.args.base_volume.as_deref());
        let base_path = clean_optional(self.args.base_path.as_deref());
        if base_volume.is_some() && base_path.is_some() {
            bail!("create 的 base_volume 与 base_path 只能二选一");
        }
        let mut gpu = Vec::new();
        for raw in &self.args.gpu {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            GpuPassthrough::new(raw)
                .with_context(|| format!("非法 --gpu {raw:?}（期望 dddd:bb:dd.f）"))?;
            gpu.push(raw.to_ascii_lowercase());
        }
        if gpu.len() > MAX_GPUS {
            bail!("gpu 数量 {} 超过上限 {MAX_GPUS}", gpu.len());
        }
        Ok(CreateArgs {
            name: name.to_owned(),
            vcpus,
            memory_mib,
            disk_gb,
            base_volume,
            base_path,
            gpu,
            start: self.args.start.unwrap_or(false),
        })
    }
}

fn clean_optional(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// VM 名只允许 `[A-Za-z0-9._-]{1,64}`（同时用作目录名，防路径注入）。
pub fn is_valid_vm_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        // `.` / `..` 是目录穿越特例
        && name != "."
        && name != ".."
}

fn validate_vm_name(name: &str) -> Result<()> {
    if is_valid_vm_name(name) {
        Ok(())
    } else {
        bail!("非法 VM 名: {name:?}（允许 [A-Za-z0-9._-]{{1,64}}）")
    }
}

/// `create` 归一化后的参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateArgs {
    pub name: String,
    pub vcpus: u32,
    pub memory_mib: u64,
    pub disk_gb: u64,
    pub base_volume: Option<String>,
    pub base_path: Option<String>,
    pub gpu: Vec<String>,
    pub start: bool,
}

/// qcow2 内部快照动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotAction {
    Create,
    List,
    Restore,
    Delete,
}

/// 命令执行结果（回传 device-pki `POST /machine/command/result`）。
#[derive(Debug, Clone)]
pub struct CommandResult {
    pub request_id: Uuid,
    pub op: VmOp,
    pub ok: bool,
    pub data: Option<Value>,
    pub error: Option<String>,
}

impl CommandResult {
    fn success(command: &VmCommand, data: Value) -> Self {
        Self {
            request_id: command.request_id,
            op: command.op,
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    fn failure(command: &VmCommand, error: impl Into<String>) -> Self {
        Self {
            request_id: command.request_id,
            op: command.op,
            ok: false,
            data: None,
            error: Some(error.into()),
        }
    }
}

/// 可 mock 的 VM 后端抽象（生产实现见 [`LibvirtExecutor`]）。
pub trait VmExecutor: Send + Sync {
    /// `list` 的 JSON（`{"vms":[...]}`）。
    fn list(&self) -> Result<Value>;
    /// 生命周期动作（已校验 name）。
    fn lifecycle(&self, op: VmOp, name: &str) -> Result<()>;
    /// 截图原始字节（PNG 或 PNM）。
    fn screenshot(&self, name: &str) -> Result<Vec<u8>>;
    /// 底包 + 差分盘创建并 define（可选启动）。
    fn create(&self, args: &CreateArgs) -> Result<Value>;
    /// qcow2 内部快照（要求域 shut off）。
    fn snapshot(
        &self,
        action: SnapshotAction,
        name: &str,
        snapshot: Option<&str>,
        description: Option<&str>,
    ) -> Result<Value>;
    /// 磁盘 backing chain。
    fn disk_info(&self, name: &str) -> Result<Value>;
    /// 导出完整 qcow2 + sha256（要求域 shut off）。
    fn export(&self, name: &str, format: &str) -> Result<Value>;
}

/// 生产实现：每条命令独立连接 libvirt（`virt-crate::Backend`），
/// 磁盘类操作走 `qemu-img` / `virsh`。
pub struct LibvirtExecutor {
    data_dir: PathBuf,
    /// 显式 libvirt URI；`None` 时走 system → session 自动探测。
    /// agent 常驻进程可用 `LIBVIRT_URI` 环境变量指定（CLI 也可用 `--uri` 覆盖）。
    uri: Option<String>,
}

impl LibvirtExecutor {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        let uri = std::env::var("LIBVIRT_URI")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        Self {
            data_dir: data_dir.into(),
            uri,
        }
    }

    /// 显式指定 URI（CLI 的 `--uri` / 测试用）。
    pub fn with_uri(data_dir: impl Into<PathBuf>, uri: Option<String>) -> Self {
        let uri = uri
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                std::env::var("LIBVIRT_URI")
                    .ok()
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty())
            });
        Self {
            data_dir: data_dir.into(),
            uri,
        }
    }

    fn connect_uri(uri: &str) -> Result<Backend> {
        Backend::libvirt_uri(uri)
            .map_err(|error| anyhow::anyhow!("连接 libvirt 失败（{uri}）: {error}"))
    }

    /// list/lifecycle/screenshot 用：libvirt 优先，KVM 兜底。
    fn backend(&self) -> Result<Backend> {
        if let Some(uri) = self.uri.as_deref() {
            return Self::connect_uri(uri);
        }
        Backend::auto().map_err(|error| {
            anyhow::anyhow!(
                "libvirt/KVM 不可用（确认 libvirtd 在运行、agent 有 qemu:///system 权限）: {error}"
            )
        })
    }

    /// create/snapshot/disk/export 必须落在真实 libvirt 域上。
    fn libvirt(&self) -> Result<Backend> {
        if let Some(uri) = self.uri.as_deref() {
            return Self::connect_uri(uri);
        }
        Backend::libvirt().map_err(|error| {
            anyhow::anyhow!(
                "libvirt 不可用（确认 libvirtd 在运行、可用 LIBVIRT_URI=qemu:///session 调试）: {error}"
            )
        })
    }

    /// 取域的第一块可写磁盘路径（qcow2 差分盘）。
    fn domain_disk(&self, name: &str) -> Result<(Backend, PathBuf)> {
        let hypervisor = self.libvirt()?;
        let domain = hypervisor
            .domain(name)
            .map_err(|error| anyhow::anyhow!("找不到虚拟机 {name:?}: {error}"))?;
        let disks = domain
            .disk_paths()
            .map_err(|error| anyhow::anyhow!("读取 {name:?} 磁盘列表失败: {error}"))?;
        let first = disks
            .first()
            .ok_or_else(|| anyhow::anyhow!("虚拟机 {name:?} 没有可操作磁盘"))?;
        let path = PathBuf::from(first);
        if !path.is_file() {
            bail!("虚拟机 {name:?} 的磁盘文件不存在: {}", path.display());
        }
        Ok((hypervisor, path))
    }

    /// 快照/导出要求域处于 shut off。
    fn ensure_shut_off(hypervisor: &Backend, name: &str) -> Result<()> {
        let domain = hypervisor
            .domain(name)
            .map_err(|error| anyhow::anyhow!("找不到虚拟机 {name:?}: {error}"))?;
        let state = domain
            .state()
            .map_err(|error| anyhow::anyhow!("读取 {name:?} 状态失败: {error}"))?;
        if state.is_active() {
            bail!(
                "虚拟机 {name:?} 正在运行（state={}）：快照/导出要求先 shutdown",
                state_str(state)
            );
        }
        if state != DomainState::ShutOff {
            bail!(
                "虚拟机 {name:?} 当前状态 {}，仅支持 shut off 状态下快照/导出",
                state_str(state)
            );
        }
        Ok(())
    }
}

impl VmExecutor for LibvirtExecutor {
    fn list(&self) -> Result<Value> {
        let hypervisor = self.backend()?;
        let domains = hypervisor
            .list_domains()
            .map_err(|error| anyhow::anyhow!("列出虚拟机失败（libvirt）: {error}"))?;
        let vms = domains
            .into_iter()
            .map(|domain| {
                let vnc = hypervisor
                    .graphics_of(&domain.name)
                    .ok()
                    .and_then(|graphics| graphics.into_iter().next())
                    .map(graphics_json);
                json!({
                    "name": domain.name,
                    "state": state_str(domain.state),
                    "vcpus": domain.vcpus,
                    "memory_bytes": domain.max_memory_kib.saturating_mul(1024),
                    "vnc": vnc,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({ "vms": vms }))
    }

    fn lifecycle(&self, op: VmOp, name: &str) -> Result<()> {
        let hypervisor = self.backend()?;
        let domain = hypervisor
            .domain(name)
            .map_err(|error| anyhow::anyhow!("找不到虚拟机 {name:?}: {error}"))?;
        let result = match op {
            VmOp::Start => domain.start(),
            VmOp::Shutdown => domain.shutdown(),
            VmOp::Reboot => domain.reboot(),
            VmOp::Destroy => domain.destroy(),
            _ => bail!("op={} 不是生命周期命令", op.as_str()),
        };
        result.map_err(|error| anyhow::anyhow!("{} {name:?} 失败: {error}", op.as_str()))
    }

    fn screenshot(&self, name: &str) -> Result<Vec<u8>> {
        let hypervisor = self.backend()?;
        hypervisor
            .screenshot_of(name)
            .map_err(|error| anyhow::anyhow!("截图 {name:?} 失败: {error}"))
    }

    fn create(&self, args: &CreateArgs) -> Result<Value> {
        let hypervisor = self.libvirt()?;
        // 重名先在列表里查，避免 define 阶段报 libvirt 原始错误。
        let exists = hypervisor
            .list_domains()
            .map(|domains| domains.iter().any(|domain| domain.name == args.name))
            .unwrap_or(false);
        if exists {
            bail!("虚拟机 {:?} 已存在；换名或先 destroy/undefine", args.name);
        }

        let disk_path = self
            .data_dir
            .join("vms")
            .join(&args.name)
            .join("disk.qcow2");
        let base = resolve_base_path(args.base_volume.as_deref(), args.base_path.as_deref())?;
        create_overlay_disk(&disk_path, base.as_deref(), args.disk_gb)?;

        let mut spec = VmSpec {
            name: args.name.clone(),
            vcpus: args.vcpus,
            memory_kib: args.memory_mib.saturating_mul(1024),
            disks: vec![disk_path.to_string_lossy().into_owned()],
            boot: virt_crate::BootDevice::Hd,
            accel: virt_crate::Accel::Auto,
            ..Default::default()
        };
        for raw in &args.gpu {
            spec.gpus.push(GpuPassthrough::new(raw.clone())?);
        }
        let rules = LoadRules {
            require_disks_exist: true,
            ..Default::default()
        };
        let domain = hypervisor
            .define(&spec, &rules)
            .with_context(|| format!("定义虚拟机 {:?} 失败", args.name))?;
        let started = if args.start {
            domain
                .start()
                .with_context(|| format!("启动虚拟机 {:?} 失败", args.name))?;
            true
        } else {
            false
        };

        tracing::info!(
            name = %args.name,
            disk = %disk_path.display(),
            base = ?base.as_ref().map(|path| path.display().to_string()),
            started,
            "VM create 完成（底包 + 差分盘）"
        );
        Ok(json!({
            "name": args.name,
            "disk_path": disk_path.to_string_lossy(),
            "base": base.as_ref().map(|path| path.to_string_lossy().into_owned()),
            "defined": true,
            "started": started,
        }))
    }

    fn snapshot(
        &self,
        action: SnapshotAction,
        name: &str,
        snapshot: Option<&str>,
        _description: Option<&str>,
    ) -> Result<Value> {
        let (hypervisor, disk) = self.domain_disk(name)?;
        // 所有快照动作（含 list）都要求 shut off，避免运行中元数据不一致。
        Self::ensure_shut_off(&hypervisor, name)?;
        match action {
            SnapshotAction::Create => {
                let tag =
                    snapshot.ok_or_else(|| anyhow::anyhow!("snapshot.create 缺少 snapshot"))?;
                qemu_img_snapshot(&disk, SnapshotAction::Create, Some(tag))
                    .with_context(|| format!("创建快照 {tag:?} 失败（{name}）"))?;
                Ok(json!({ "name": name, "snapshot": tag }))
            }
            SnapshotAction::List => {
                let snapshots = qemu_img_snapshot_list(&disk)?;
                Ok(json!({ "name": name, "snapshots": snapshots }))
            }
            SnapshotAction::Restore => {
                let tag =
                    snapshot.ok_or_else(|| anyhow::anyhow!("snapshot.restore 缺少 snapshot"))?;
                qemu_img_snapshot(&disk, SnapshotAction::Restore, Some(tag))
                    .with_context(|| format!("恢复快照 {tag:?} 失败（{name}）"))?;
                Ok(json!({ "name": name, "snapshot": tag }))
            }
            SnapshotAction::Delete => {
                let tag =
                    snapshot.ok_or_else(|| anyhow::anyhow!("snapshot.delete 缺少 snapshot"))?;
                qemu_img_snapshot(&disk, SnapshotAction::Delete, Some(tag))
                    .with_context(|| format!("删除快照 {tag:?} 失败（{name}）"))?;
                Ok(json!({ "name": name, "snapshot": tag }))
            }
        }
    }

    fn disk_info(&self, name: &str) -> Result<Value> {
        let (_hypervisor, disk) = self.domain_disk(name)?;
        qemu_img_info_chain(&disk)
    }

    fn export(&self, name: &str, format: &str) -> Result<Value> {
        if format != "qcow2" {
            bail!("export 目前只支持 format=qcow2，收到 {format:?}");
        }
        let (hypervisor, disk) = self.domain_disk(name)?;
        Self::ensure_shut_off(&hypervisor, name)?;
        let exports_dir = self.data_dir.join("exports");
        std::fs::create_dir_all(&exports_dir)
            .with_context(|| format!("创建导出目录失败: {}", exports_dir.display()))?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        let dest = exports_dir.join(format!("{name}-{timestamp}.qcow2"));
        let (sha256, size_bytes) = qemu_img_export(&disk, &dest, format)?;
        tracing::info!(
            name = %name,
            dest = %dest.display(),
            size_bytes,
            sha256 = %sha256,
            "VM 磁盘导出完成"
        );
        Ok(json!({
            "path": dest.to_string_lossy(),
            "sha256": sha256,
            "size_bytes": size_bytes,
        }))
    }
}

fn state_str(state: DomainState) -> &'static str {
    match state {
        DomainState::ShutOff => "shutoff",
        DomainState::Running => "running",
        DomainState::Paused => "paused",
        DomainState::ShuttingDown => "shutting_down",
        DomainState::Crashed => "crashed",
        DomainState::Unknown => "unknown",
        // `DomainState` 是 non_exhaustive：未来 libvirt 新增状态时保持向后兼容。
        _ => "unknown",
    }
}

fn graphics_json(info: GraphicsInfo) -> Value {
    json!({
        "host": info.host,
        "port": info.port,
        // 不主动暴露明文密码；控制面需要时由管理员从宿主机配置获取。
        "password": info.password_value,
    })
}

/// 解析下行 payload 并执行；payload 解析失败返回 `Err`（无法关联 request_id）。
pub async fn execute_payload(
    executor: Arc<dyn VmExecutor>,
    payload: &str,
) -> Result<CommandResult> {
    let command = VmCommand::parse(payload)?;
    Ok(execute(executor, command).await)
}

/// 执行一条已解析命令（带超时、panic 隔离）。
pub async fn execute(executor: Arc<dyn VmExecutor>, command: VmCommand) -> CommandResult {
    let timeout = command.timeout();
    execute_with_timeout(executor, command, timeout).await
}

/// 执行时真正搬进阻塞任务的计划（在 async 侧完成参数校验/默认值）。
enum PreparedOp {
    List,
    Screenshot(String),
    Lifecycle(VmOp, String),
    Create(CreateArgs),
    Snapshot(SnapshotAction, String, Option<String>),
    DiskInfo(String),
    Export(String, String),
}

fn prepare(command: &VmCommand) -> Result<PreparedOp> {
    let name = command.target_name();
    Ok(match command.op {
        VmOp::List => PreparedOp::List,
        VmOp::Screenshot => PreparedOp::Screenshot(name),
        VmOp::Start | VmOp::Shutdown | VmOp::Reboot | VmOp::Destroy => {
            PreparedOp::Lifecycle(command.op, name)
        }
        VmOp::Create => PreparedOp::Create(command.create_args()?),
        VmOp::SnapshotCreate
        | VmOp::SnapshotList
        | VmOp::SnapshotRestore
        | VmOp::SnapshotDelete => {
            let action = command
                .op
                .snapshot_action()
                .ok_or_else(|| anyhow::anyhow!("op={} 不是快照命令", command.op.as_str()))?;
            let tag = match action {
                SnapshotAction::List => None,
                _ => Some(command.snapshot_tag()?),
            };
            PreparedOp::Snapshot(action, name, tag)
        }
        VmOp::DiskInfo => PreparedOp::DiskInfo(name),
        VmOp::Export => PreparedOp::Export(name, command.export_format()),
    })
}

/// 同 [`execute`]，超时可注入（单测用短超时）。
pub async fn execute_with_timeout(
    executor: Arc<dyn VmExecutor>,
    command: VmCommand,
    timeout: Duration,
) -> CommandResult {
    let prepared = match prepare(&command) {
        Ok(prepared) => prepared,
        Err(error) => return CommandResult::failure(&command, format!("{error:#}")),
    };
    let fallback = command.clone();

    let task = tokio::task::spawn_blocking(move || -> Result<Value> {
        match prepared {
            PreparedOp::List => executor.list(),
            PreparedOp::Screenshot(name) => {
                let raw = executor.screenshot(&name)?;
                screenshot_payload(raw)
            }
            PreparedOp::Lifecycle(op, name) => {
                executor.lifecycle(op, &name)?;
                Ok(json!({ "name": name, "result": "ok" }))
            }
            PreparedOp::Create(args) => executor.create(&args),
            PreparedOp::Snapshot(action, name, tag) => {
                executor.snapshot(action, &name, tag.as_deref(), None)
            }
            PreparedOp::DiskInfo(name) => executor.disk_info(&name),
            PreparedOp::Export(name, format) => executor.export(&name, &format),
        }
    });

    match tokio::time::timeout(timeout, task).await {
        Err(_) => {
            CommandResult::failure(&fallback, format!("命令执行超时（{}s）", timeout.as_secs()))
        }
        Ok(Err(join_error)) if join_error.is_panic() => {
            // 阻塞任务 panic 已被隔离在这里，agent 主进程不受影响。
            CommandResult::failure(&fallback, format!("命令执行 panic（已隔离）: {join_error}"))
        }
        Ok(Err(join_error)) => {
            CommandResult::failure(&fallback, format!("命令执行任务失败: {join_error}"))
        }
        Ok(Ok(Ok(data))) => CommandResult::success(&fallback, data),
        Ok(Ok(Err(error))) => CommandResult::failure(&fallback, format!("{error:#}")),
    }
}

// ---------------- qemu-img / virsh 辅助（libvirt 无关，可单测/集成测） ----------------

/// `qemu-img` 可执行文件（默认 PATH，可用 `QE_QEMU_IMG` 覆盖）。
pub fn qemu_img_bin() -> String {
    std::env::var("QE_QEMU_IMG")
        .ok()
        .map(|bin| bin.trim().to_owned())
        .filter(|bin| !bin.is_empty())
        .unwrap_or_else(|| "qemu-img".to_owned())
}

/// 运行一个外部命令并返回 stdout；非 0 退出把 stderr 带进错误。
fn run_tool(program: &str, args: &[String]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("执行 {program} {args:?} 失败（命令不存在？）"))?;
    if !output.status.success() {
        bail!(
            "{program} {args:?} 失败（{}）: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `virsh` 的连接参数：优先 `LIBVIRT_URI`（与 qe-agent CLI 的 `--uri` 对齐）。
fn virsh_connect_args() -> Vec<String> {
    match std::env::var("LIBVIRT_URI")
        .ok()
        .map(|uri| uri.trim().to_owned())
        .filter(|uri| !uri.is_empty())
    {
        Some(uri) => vec!["--connect".to_owned(), uri],
        None => Vec::new(),
    }
}

/// 解析 `create` 的底包路径：`base_volume`（libvirt 卷）或 `base_path` 二选一。
pub fn resolve_base_path(
    base_volume: Option<&str>,
    base_path: Option<&str>,
) -> Result<Option<PathBuf>> {
    let base_volume = clean_optional(base_volume);
    let base_path = clean_optional(base_path);
    match (base_volume, base_path) {
        (Some(_), Some(_)) => bail!("base_volume 与 base_path 只能二选一"),
        (Some(volume), None) => {
            let pool = std::env::var("LIBVIRT_POOL")
                .ok()
                .map(|pool| pool.trim().to_owned())
                .filter(|pool| !pool.is_empty())
                .unwrap_or_else(|| "default".to_owned());
            let mut args = virsh_connect_args();
            args.extend([
                "vol-path".to_owned(),
                "--pool".to_owned(),
                pool.clone(),
                volume.clone(),
            ]);
            let stdout = run_tool("virsh", &args)
                .with_context(|| format!("用 virsh 解析底包卷 {volume:?}（pool={pool}）失败"))?;
            let path = PathBuf::from(stdout.trim());
            if !path.is_file() {
                bail!("底包卷解析出的路径不存在: {}", path.display());
            }
            Ok(Some(path))
        }
        (None, Some(path)) => {
            let path = PathBuf::from(path);
            if !path.is_file() {
                bail!("底包文件不存在: {}", path.display());
            }
            Ok(Some(path))
        }
        (None, None) => Ok(None),
    }
}

/// 创建 qcow2 盘：有底包时生成差分盘（backing file），否则普通空盘。
pub fn create_overlay_disk(disk: &Path, base: Option<&Path>, disk_gb: u64) -> Result<()> {
    create_disk_with_size(disk, base, &format!("{disk_gb}G"))
}

/// 同 [`create_overlay_disk`]，但大小用 qemu-img 原样接受的规格串（如 `20G`/`512M`）。
pub fn create_disk_with_size(disk: &Path, base: Option<&Path>, size_spec: &str) -> Result<()> {
    if let Some(parent) = disk.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建磁盘目录失败: {}", parent.display()))?;
    }
    let mut args = vec!["create".to_owned(), "-f".to_owned(), "qcow2".to_owned()];
    if let Some(base) = base {
        args.extend([
            "-b".to_owned(),
            base.to_string_lossy().into_owned(),
            "-F".to_owned(),
            "qcow2".to_owned(),
        ]);
    }
    args.push(disk.to_string_lossy().into_owned());
    args.push(size_spec.to_owned());
    run_tool(&qemu_img_bin(), &args)
        .with_context(|| format!("创建磁盘失败: {}", disk.display()))?;
    if !disk.is_file() {
        bail!("qemu-img create 后磁盘文件不存在: {}", disk.display());
    }
    Ok(())
}

/// qcow2 内部快照 -c/-a/-d（list 用 [`qemu_img_snapshot_list`]）。
pub fn qemu_img_snapshot(disk: &Path, action: SnapshotAction, tag: Option<&str>) -> Result<()> {
    let flag = match action {
        SnapshotAction::Create => "-c",
        SnapshotAction::Restore => "-a",
        SnapshotAction::Delete => "-d",
        SnapshotAction::List => bail!("list 请用 qemu_img_snapshot_list"),
    };
    let tag = tag
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .ok_or_else(|| anyhow::anyhow!("快照动作 {flag} 缺少 tag"))?;
    let args = vec![
        "snapshot".to_owned(),
        flag.to_owned(),
        tag.to_owned(),
        disk.to_string_lossy().into_owned(),
    ];
    run_tool(&qemu_img_bin(), &args)
        .with_context(|| format!("qemu-img snapshot {flag} {tag:?} 失败: {}", disk.display()))?;
    Ok(())
}

/// 一条 `qemu-img snapshot -l` 解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotEntry {
    pub id: String,
    pub tag: String,
    pub vm_size: String,
    /// `YYYY-MM-DD HH:MM:SS`。
    pub date: String,
    pub vm_clock: Option<String>,
    pub icount: Option<i64>,
}

/// 解析 `qemu-img snapshot -l <disk>` 的文本输出。
///
/// 现代 qemu 输出列：`ID TAG VM_SIZE DATE VM_CLOCK ICOUNT`；旧版本没有
/// `ICOUNT` 列，按时钟列位置兼容解析。
pub fn parse_snapshot_list(output: &str) -> Vec<SnapshotEntry> {
    let mut entries = Vec::new();
    for line in output.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 5 {
            continue;
        }
        let Ok(id) = tokens[0].parse::<u64>() else {
            continue;
        };
        let tag = tokens[1].to_owned();

        // 时钟 token 形如 `0000:00:00.000`；ICOUNT 是其后的整数。
        let clock_idx = tokens
            .iter()
            .rposition(|token| token.contains(':') && token.contains('.'));
        let (date_end, vm_clock, icount) = match clock_idx {
            Some(index) => {
                let icount = tokens
                    .get(index + 1)
                    .and_then(|raw| raw.parse::<i64>().ok());
                (index, Some(tokens[index].to_owned()), icount)
            }
            None => (tokens.len(), None, None),
        };
        if date_end < 4 {
            continue;
        }
        let date = format!("{} {}", tokens[date_end - 2], tokens[date_end - 1]);
        let vm_size = tokens[2..date_end - 2].join(" ");
        entries.push(SnapshotEntry {
            id: id.to_string(),
            tag,
            vm_size,
            date,
            vm_clock,
            icount,
        });
    }
    entries
}

/// `qemu-img snapshot -l` → JSON 快照列表。
pub fn qemu_img_snapshot_list(disk: &Path) -> Result<Vec<Value>> {
    let args = vec![
        "snapshot".to_owned(),
        "-l".to_owned(),
        disk.to_string_lossy().into_owned(),
    ];
    let output = run_tool(&qemu_img_bin(), &args)
        .with_context(|| format!("列出快照失败: {}", disk.display()))?;
    Ok(parse_snapshot_list(&output)
        .into_iter()
        .map(|entry| {
            json!({
                "id": entry.id,
                "tag": entry.tag,
                "vm_size": entry.vm_size,
                "date": entry.date,
                "vm_clock": entry.vm_clock,
                "icount": entry.icount,
            })
        })
        .collect())
}

/// `qemu-img info --backing-chain --output=json` → 归一化磁盘链 JSON。
pub fn qemu_img_info_chain(disk: &Path) -> Result<Value> {
    let args = vec![
        "info".to_owned(),
        "--backing-chain".to_owned(),
        "--output=json".to_owned(),
        disk.to_string_lossy().into_owned(),
    ];
    let stdout = run_tool(&qemu_img_bin(), &args)
        .with_context(|| format!("读取磁盘链失败: {}", disk.display()))?;
    let raw: Value = serde_json::from_str(&stdout).context("解析 qemu-img info JSON 失败")?;
    let chain = raw
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("qemu-img info --backing-chain 输出不是数组"))?;
    let disks = chain
        .iter()
        .map(|entry| {
            json!({
                "path": entry.get("filename").cloned().unwrap_or(Value::Null),
                "format": entry.get("format").cloned().unwrap_or(Value::Null),
                "virtual_size": entry.get("virtual-size").cloned().unwrap_or(Value::Null),
                "backing_path": entry.get("backing-filename").cloned().unwrap_or(Value::Null),
                "backing_format": entry
                    .get("backing-filename-format")
                    .cloned()
                    .unwrap_or(Value::Null),
            })
        })
        .collect::<Vec<_>>();
    let chain_paths = chain
        .iter()
        .filter_map(|entry| entry.get("filename").cloned())
        .collect::<Vec<_>>();
    Ok(json!({ "disks": disks, "chain": chain_paths }))
}

/// `qemu-img convert` 导出扁平 qcow2，返回 `(sha256 hex, 字节数)`。
pub fn qemu_img_export(disk: &Path, dest: &Path, format: &str) -> Result<(String, u64)> {
    if format != "qcow2" {
        bail!("qemu-img export 只支持 qcow2，收到 {format:?}");
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建导出目录失败: {}", parent.display()))?;
    }
    let args = vec![
        "convert".to_owned(),
        "-O".to_owned(),
        format.to_owned(),
        disk.to_string_lossy().into_owned(),
        dest.to_string_lossy().into_owned(),
    ];
    run_tool(&qemu_img_bin(), &args)
        .with_context(|| format!("导出磁盘失败: {} -> {}", disk.display(), dest.display()))?;
    let sha256 = sha256_file(dest)?;
    let size_bytes = std::fs::metadata(dest)?.len();
    Ok((sha256, size_bytes))
}

/// 流式计算文件 sha256（小写 hex）。
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("打开文件失败: {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

// ---------------- screenshot（PNG/PNM） ----------------

/// 截图：原生 PNG 直接透传，PNM 转 PNG；base64 后 >4 MiB 拒绝。
pub fn screenshot_payload(raw: Vec<u8>) -> Result<Value> {
    let png_bytes = if is_png(&raw) { raw } else { pnm_to_png(&raw)? };
    let image_base64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
    if image_base64.len() > MAX_SCREENSHOT_BASE64_BYTES {
        bail!("截图过大：base64 {} 字节 > 4 MiB 上限", image_base64.len());
    }
    Ok(json!({
        "format": "png",
        "image_base64": image_base64,
        "size_bytes": png_bytes.len(),
    }))
}

/// PNG 文件签名。
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

fn is_png(raw: &[u8]) -> bool {
    raw.len() >= PNG_MAGIC.len() && raw[..PNG_MAGIC.len()] == PNG_MAGIC
}

/// 把 QEMU 旧驱动返回的 PNM（P6 RGB / P5 灰度）转成 PNG。
fn pnm_to_png(raw: &[u8]) -> Result<Vec<u8>> {
    let (pixels, width, height, color) = parse_pnm(raw)?;
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .context("编码截图 PNG 失败（header）")?;
        writer
            .write_image_data(&pixels)
            .context("编码截图 PNG 失败（像素）")?;
    }
    Ok(out)
}

struct PnmCursor<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> PnmCursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    /// 跳过空白与 `#` 注释，返回下一个 token。
    fn next_token(&mut self) -> Result<&'a [u8]> {
        loop {
            while self
                .data
                .get(self.position)
                .is_some_and(u8::is_ascii_whitespace)
            {
                self.position += 1;
            }
            if self.data.get(self.position) == Some(&b'#') {
                while self
                    .data
                    .get(self.position)
                    .is_some_and(|byte| *byte != b'\n')
                {
                    self.position += 1;
                }
                continue;
            }
            break;
        }
        let start = self.position;
        while self
            .data
            .get(self.position)
            .is_some_and(|byte| !byte.is_ascii_whitespace())
        {
            self.position += 1;
        }
        if start == self.position {
            bail!("PNM 头被截断");
        }
        Ok(&self.data[start..self.position])
    }

    fn next_u32(&mut self, label: &str) -> Result<u32> {
        let token = self.next_token()?;
        let text = std::str::from_utf8(token)
            .with_context(|| format!("PNM {label} 不是 ASCII: {token:?}"))?;
        text.parse::<u32>()
            .with_context(|| format!("PNM {label} 不是整数: {text:?}"))
    }
}

/// 解析 PNM（P5/P6，8-bit）；返回 `(像素, 宽, 高, PNG 色彩类型)`。
fn parse_pnm(raw: &[u8]) -> Result<(Vec<u8>, u32, u32, png::ColorType)> {
    if !raw.starts_with(b"P5") && !raw.starts_with(b"P6") {
        bail!("截图不是 PNG，也不是受支持的 PNM（P5/P6）");
    }
    let is_rgb = raw.starts_with(b"P6");
    let mut cursor = PnmCursor::new(raw);
    let _magic = cursor.next_token()?;
    let width = cursor.next_u32("width")?;
    let height = cursor.next_u32("height")?;
    let max_value = cursor.next_u32("maxval")?;
    if width == 0 || height == 0 {
        bail!("PNM 尺寸非法: {width}x{height}");
    }
    if max_value == 0 || max_value > 255 {
        bail!("PNM maxval={max_value} 不支持（仅 8-bit）");
    }
    match raw.get(cursor.position) {
        Some(b'\r') => {
            cursor.position += 1;
            if raw.get(cursor.position) == Some(&b'\n') {
                cursor.position += 1;
            }
        }
        Some(byte) if byte.is_ascii_whitespace() => cursor.position += 1,
        _ => bail!("PNM 头与像素数据之间缺少空白"),
    }
    let channels = if is_rgb { 3usize } else { 1usize };
    let expected = usize::try_from(width)
        .ok()
        .and_then(|w| usize::try_from(height).ok().map(|h| (w, h)))
        .and_then(|(w, h)| w.checked_mul(h))
        .and_then(|pixels| pixels.checked_mul(channels))
        .ok_or_else(|| anyhow::anyhow!("PNM 尺寸溢出: {width}x{height}"))?;
    let raster = raw
        .get(cursor.position..)
        .ok_or_else(|| anyhow::anyhow!("PNM 像素数据被截断"))?;
    if raster.len() < expected {
        bail!(
            "PNM 像素数据不足：需要 {expected} 字节，实际 {}",
            raster.len()
        );
    }
    let color = if is_rgb {
        png::ColorType::Rgb
    } else {
        png::ColorType::Grayscale
    };
    Ok((raster[..expected].to_vec(), width, height, color))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // ---------------- payload 解析 ----------------

    fn request_id() -> &'static str {
        "0b6f9d4e-4a41-4b6e-9d0b-4f8b0f3c2a11"
    }

    fn parse(payload: &str) -> Result<VmCommand> {
        VmCommand::parse(payload)
    }

    #[test]
    fn parses_list_command() {
        let command = parse(&format!(
            r#"{{"request_id":"{}","op":"list"}}"#,
            request_id()
        ))
        .unwrap();
        assert_eq!(command.op, VmOp::List);
        assert!(command.args.name.is_none());
    }

    #[test]
    fn parses_lifecycle_command_with_name() {
        let command = parse(&format!(
            r#"{{"request_id":"{}","op":"start","args":{{"name":"vm-1"}}}}"#,
            request_id()
        ))
        .unwrap();
        assert_eq!(command.op, VmOp::Start);
        assert_eq!(command.name(), "vm-1");
    }

    #[test]
    fn parses_create_command_with_defaults_and_base() {
        let command = parse(&format!(
            r#"{{"request_id":"{}","op":"create","args":{{
                "name":"vm-2","base_volume":"base-win11","gpu":["0000:01:00.0"],"start":true}}}}"#,
            request_id()
        ))
        .unwrap();
        let args = command.create_args().unwrap();
        assert_eq!(args.name, "vm-2");
        assert_eq!(args.vcpus, 2);
        assert_eq!(args.memory_mib, 2048);
        assert_eq!(args.disk_gb, 20);
        assert_eq!(args.base_volume.as_deref(), Some("base-win11"));
        assert_eq!(args.gpu, vec!["0000:01:00.0"]);
        assert!(args.start);

        // base_path 也接受；与 base_volume 互斥
        assert!(
            parse(&format!(
                r#"{{"request_id":"{}","op":"create","args":{{"name":"v",
                    "base_path":"/tmp/base.qcow2"}}}}"#,
                request_id()
            ))
            .is_ok()
        );
        let both = parse(&format!(
            r#"{{"request_id":"{}","op":"create","args":{{"name":"v",
                "base_path":"/tmp/base.qcow2","base_volume":"base"}}}}"#,
            request_id()
        ));
        assert!(both.is_err(), "base 二选一应拒绝");
    }

    #[test]
    fn rejects_invalid_create_args() {
        for args in [
            r#"{"name":"v","vcpus":0}"#,
            r#"{"name":"v","vcpus":65}"#,
            r#"{"name":"v","memory_mib":255}"#,
            r#"{"name":"v","memory_mib":262145}"#,
            r#"{"name":"v","disk_gb":0}"#,
            r#"{"name":"v","disk_gb":2049}"#,
            r#"{"name":"v","gpu":["bad-pci"]}"#,
            r#"{"name":"v","gpu":["0000:01:00.0","0000:02:00.0","0000:03:00.0"]}"#,
        ] {
            let payload = format!(
                r#"{{"request_id":"{}","op":"create","args":{}}}"#,
                request_id(),
                args
            );
            assert!(parse(&payload).is_err(), "{args} 应被拒绝");
        }
        // name 规则
        for bad_name in ["", "a/b", "../x", "a b", &"x".repeat(65)] {
            let payload = format!(
                r#"{{"request_id":"{}","op":"create","args":{{"name":"{}"}}}}"#,
                request_id(),
                bad_name
            );
            assert!(parse(&payload).is_err(), "{bad_name:?} 应被拒绝");
        }
    }

    #[test]
    fn parses_snapshot_ops() {
        for op in ["snapshot.create", "snapshot.restore", "snapshot.delete"] {
            let payload = format!(
                r#"{{"request_id":"{}","op":"{}","args":{{"name":"vm-1","snapshot":"s1"}}}}"#,
                request_id(),
                op
            );
            let command = parse(&payload).unwrap();
            assert_eq!(command.snapshot_tag().unwrap(), "s1");
        }
        // list 不需要 snapshot
        assert!(
            parse(&format!(
                r#"{{"request_id":"{}","op":"snapshot.list","args":{{"name":"vm-1"}}}}"#,
                request_id()
            ))
            .is_ok()
        );
        // create/restore/delete 缺 snapshot 拒绝
        assert!(
            parse(&format!(
                r#"{{"request_id":"{}","op":"snapshot.create","args":{{"name":"vm-1"}}}}"#,
                request_id()
            ))
            .is_err()
        );
        // 非法快照名
        assert!(
            parse(&format!(
                r#"{{"request_id":"{}","op":"snapshot.create","args":{{"name":"vm-1","snapshot":"../x"}}}}"#,
                request_id()
            ))
            .is_err()
        );
    }

    #[test]
    fn parses_disk_info_and_export() {
        assert!(
            parse(&format!(
                r#"{{"request_id":"{}","op":"disk.info","args":{{"name":"vm-1"}}}}"#,
                request_id()
            ))
            .is_ok()
        );
        assert!(
            parse(&format!(
                r#"{{"request_id":"{}","op":"export","args":{{"name":"vm-1"}}}}"#,
                request_id()
            ))
            .is_ok()
        );
        let unsupported = parse(&format!(
            r#"{{"request_id":"{}","op":"export","args":{{"name":"vm-1","format":"vmdk"}}}}"#,
            request_id()
        ));
        assert!(unsupported.is_err(), "非 qcow2 导出应拒绝");
    }

    #[test]
    fn rejects_missing_name_for_lifecycle_ops() {
        let error = parse(&format!(
            r#"{{"request_id":"{}","op":"start"}}"#,
            request_id()
        ))
        .unwrap_err()
        .to_string();
        assert!(error.contains("name"), "{error}");
    }

    #[test]
    fn rejects_bad_payloads() {
        assert!(
            parse(&format!(
                r#"{{"request_id":"{}","op":"eject"}}"#,
                request_id()
            ))
            .is_err()
        );
        assert!(parse(r#"{"request_id":"nope","op":"list"}"#).is_err());
        assert!(parse("not-json").is_err());
        assert!(
            parse(&format!(
                r#"{{"request_id":"{}","op":"start","args":{{"name":"../etc/passwd"}}}}"#,
                request_id()
            ))
            .is_err()
        );
    }

    #[test]
    fn parses_snapshot_list_output() {
        let modern = "Snapshot list:\n\
ID      TAG               VM_SIZE                DATE        VM_CLOCK     ICOUNT\n\
1       snap1                 0 B 2026-11-07 13:22:21  0000:00:00.000          0\n\
2       snap2            12.5 MiB 2026-11-07 13:25:00  0000:01:02.345         42\n";
        let entries = parse_snapshot_list(modern);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "1");
        assert_eq!(entries[0].tag, "snap1");
        assert_eq!(entries[0].vm_size, "0 B");
        assert_eq!(entries[0].date, "2026-11-07 13:22:21");
        assert_eq!(entries[0].vm_clock.as_deref(), Some("0000:00:00.000"));
        assert_eq!(entries[0].icount, Some(0));
        assert_eq!(entries[1].tag, "snap2");
        assert_eq!(entries[1].vm_size, "12.5 MiB");
        assert_eq!(entries[1].icount, Some(42));

        // 旧版无 ICOUNT 列
        let legacy = "Snapshot list:\n\
ID      TAG               VM_SIZE                DATE        VM_CLOCK\n\
1       old                  0 B 2026-11-07 13:22:21  0000:00:00.000\n";
        let entries = parse_snapshot_list(legacy);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tag, "old");
        assert!(entries[0].icount.is_none());

        // 空/表头行不产生条目
        assert!(parse_snapshot_list("Snapshot list:\n").is_empty());
    }

    #[test]
    fn vm_name_rules() {
        assert!(is_valid_vm_name("vm-1"));
        assert!(is_valid_vm_name("vm_1.2"));
        assert!(!is_valid_vm_name(""));
        assert!(!is_valid_vm_name("a/b"));
        assert!(!is_valid_vm_name(".."));
        assert!(!is_valid_vm_name(&"x".repeat(65)));
    }

    // ---------------- 执行（mock 后端） ----------------

    #[derive(Default)]
    struct MockExecutor {
        panic_on_screenshot: bool,
        calls: Mutex<Vec<(String, String)>>,
        screenshot_bytes: Vec<u8>,
        list_json: Option<Value>,
        delay: Option<Duration>,
    }

    impl MockExecutor {
        fn record(&self, op: &str, detail: &str) {
            self.calls
                .lock()
                .unwrap()
                .push((op.to_owned(), detail.to_owned()));
        }
    }

    impl VmExecutor for MockExecutor {
        fn list(&self) -> Result<Value> {
            self.record("list", "");
            if let Some(delay) = self.delay {
                std::thread::sleep(delay);
            }
            Ok(self.list_json.clone().unwrap_or_else(|| json!({"vms": []})))
        }

        fn lifecycle(&self, op: VmOp, name: &str) -> Result<()> {
            self.record(op.as_str(), name);
            Ok(())
        }

        fn screenshot(&self, name: &str) -> Result<Vec<u8>> {
            self.record("screenshot", name);
            if self.panic_on_screenshot {
                panic!("mock backend panic");
            }
            Ok(self.screenshot_bytes.clone())
        }

        fn create(&self, args: &CreateArgs) -> Result<Value> {
            self.record("create", &args.name);
            Ok(json!({
                "name": args.name,
                "disk_path": format!("/data/vms/{}/disk.qcow2", args.name),
                "base": args.base_volume,
                "defined": true,
                "started": args.start,
            }))
        }

        fn snapshot(
            &self,
            action: SnapshotAction,
            name: &str,
            snapshot: Option<&str>,
            _description: Option<&str>,
        ) -> Result<Value> {
            self.record(
                match action {
                    SnapshotAction::Create => "snapshot.create",
                    SnapshotAction::List => "snapshot.list",
                    SnapshotAction::Restore => "snapshot.restore",
                    SnapshotAction::Delete => "snapshot.delete",
                },
                name,
            );
            match action {
                SnapshotAction::List => Ok(json!({ "name": name, "snapshots": [] })),
                _ => Ok(json!({ "name": name, "snapshot": snapshot })),
            }
        }

        fn disk_info(&self, name: &str) -> Result<Value> {
            self.record("disk.info", name);
            Ok(json!({ "disks": [], "chain": [] }))
        }

        fn export(&self, name: &str, format: &str) -> Result<Value> {
            self.record("export", name);
            Ok(json!({
                "path": format!("/data/exports/{name}.qcow2"),
                "sha256": "a".repeat(64),
                "size_bytes": 1024,
                "format": format,
            }))
        }
    }

    fn command(payload: &str) -> VmCommand {
        VmCommand::parse(payload).unwrap()
    }

    #[tokio::test]
    async fn list_returns_vms_json() {
        let executor = Arc::new(MockExecutor {
            list_json: Some(json!({"vms":[{"name":"vm-1","state":"running"}]})),
            ..Default::default()
        });
        let result = execute(
            executor,
            command(&format!(
                r#"{{"request_id":"{}","op":"list"}}"#,
                request_id()
            )),
        )
        .await;
        assert!(result.ok, "{:?}", result.error);
        assert_eq!(result.data.unwrap()["vms"][0]["name"], "vm-1");
        assert_eq!(result.op, VmOp::List);
    }

    #[tokio::test]
    async fn lifecycle_returns_name_and_ok() {
        let executor = Arc::new(MockExecutor::default());
        let result = execute(
            executor.clone(),
            command(&format!(
                r#"{{"request_id":"{}","op":"reboot","args":{{"name":"vm-7"}}}}"#,
                request_id()
            )),
        )
        .await;
        assert!(result.ok);
        let data = result.data.unwrap();
        assert_eq!(data["name"], "vm-7");
        assert_eq!(data["result"], "ok");
        assert_eq!(
            executor.calls.lock().unwrap().as_slice(),
            [("reboot".to_string(), "vm-7".to_string())]
        );
    }

    #[tokio::test]
    async fn create_command_dispatches_to_executor() {
        let executor = Arc::new(MockExecutor::default());
        let result = execute(
            executor.clone(),
            command(&format!(
                r#"{{"request_id":"{}","op":"create","args":{{"name":"vm-new",
                    "vcpus":4,"memory_mib":4096,"disk_gb":40,"base_volume":"base-win11",
                    "gpu":["0000:01:00.0"],"start":true}}}}"#,
                request_id()
            )),
        )
        .await;
        assert!(result.ok, "{:?}", result.error);
        let data = result.data.unwrap();
        assert_eq!(data["name"], "vm-new");
        assert_eq!(data["base"], "base-win11");
        assert_eq!(data["started"], true);
        assert_eq!(
            executor.calls.lock().unwrap().as_slice(),
            [("create".to_string(), "vm-new".to_string())]
        );
    }

    #[tokio::test]
    async fn snapshot_and_disk_ops_dispatch() {
        let executor = Arc::new(MockExecutor::default());
        let cases = [
            (
                r#""op":"snapshot.create","args":{"name":"vm-1","snapshot":"s1"}"#,
                "snapshot.create",
            ),
            (
                r#""op":"snapshot.list","args":{"name":"vm-1"}"#,
                "snapshot.list",
            ),
            (
                r#""op":"snapshot.restore","args":{"name":"vm-1","snapshot":"s1"}"#,
                "snapshot.restore",
            ),
            (
                r#""op":"snapshot.delete","args":{"name":"vm-1","snapshot":"s1"}"#,
                "snapshot.delete",
            ),
            (r#""op":"disk.info","args":{"name":"vm-1"}"#, "disk.info"),
            (
                r#""op":"export","args":{"name":"vm-1","format":"qcow2"}"#,
                "export",
            ),
        ];
        for (fragment, expected_call) in cases {
            let payload = format!(r#"{{"request_id":"{}",{}}}"#, request_id(), fragment);
            let result = execute(executor.clone(), command(&payload)).await;
            assert!(result.ok, "{expected_call}: {:?}", result.error);
            assert!(
                executor
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(op, _)| op == expected_call),
                "{expected_call} 未分发: {:?}",
                executor.calls.lock().unwrap()
            );
        }
    }

    #[tokio::test]
    async fn invalid_create_command_reports_error_without_executing() {
        let executor = Arc::new(MockExecutor::default());
        // parse 会先拒绝；直接构造 execute 校验分支
        let mut cmd = command(&format!(
            r#"{{"request_id":"{}","op":"create","args":{{"name":"vm-1"}}}}"#,
            request_id()
        ));
        cmd.args.vcpus = Some(0);
        let result = execute(executor.clone(), cmd).await;
        assert!(!result.ok);
        assert!(result.error.unwrap().contains("vcpus"));
    }

    #[tokio::test]
    async fn screenshot_pnm_is_converted_to_png_base64() {
        let mut pnm = Vec::new();
        pnm.extend_from_slice(b"P6\n2 2\n255\n");
        for _ in 0..4 {
            pnm.extend_from_slice(&[255, 0, 0]);
        }
        let executor = Arc::new(MockExecutor {
            screenshot_bytes: pnm,
            ..Default::default()
        });
        let result = execute(
            executor,
            command(&format!(
                r#"{{"request_id":"{}","op":"screenshot","args":{{"name":"vm-1"}}}}"#,
                request_id()
            )),
        )
        .await;
        assert!(result.ok, "{:?}", result.error);
        let data = result.data.unwrap();
        assert_eq!(data["format"], "png");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data["image_base64"].as_str().unwrap())
            .unwrap();
        assert!(is_png(&bytes));
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut buffer = vec![0; reader.output_buffer_size().expect("png buffer size")];
        let info = reader.next_frame(&mut buffer).unwrap();
        assert_eq!((info.width, info.height), (2, 2));
        assert_eq!(&buffer[..3], &[255, 0, 0]);
    }

    #[tokio::test]
    async fn screenshot_png_passthrough_and_size_limit() {
        let png_bytes = tiny_png();
        let executor = Arc::new(MockExecutor {
            screenshot_bytes: png_bytes.clone(),
            ..Default::default()
        });
        let result = execute(
            executor,
            command(&format!(
                r#"{{"request_id":"{}","op":"screenshot","args":{{"name":"vm-1"}}}}"#,
                request_id()
            )),
        )
        .await;
        assert!(result.ok);
        assert_eq!(
            result.data.unwrap()["image_base64"].as_str().unwrap(),
            base64::engine::general_purpose::STANDARD.encode(&png_bytes)
        );

        let mut big = PNG_MAGIC.to_vec();
        big.resize(MAX_SCREENSHOT_BASE64_BYTES, 0);
        let executor = Arc::new(MockExecutor {
            screenshot_bytes: big,
            ..Default::default()
        });
        let result = execute(
            executor,
            command(&format!(
                r#"{{"request_id":"{}","op":"screenshot","args":{{"name":"vm-1"}}}}"#,
                request_id()
            )),
        )
        .await;
        assert!(!result.ok);
        assert!(result.error.unwrap().contains("过大"));
    }

    #[tokio::test]
    async fn panic_is_isolated() {
        let executor = Arc::new(MockExecutor {
            panic_on_screenshot: true,
            screenshot_bytes: tiny_png(),
            ..Default::default()
        });
        let result = execute(
            executor,
            command(&format!(
                r#"{{"request_id":"{}","op":"screenshot","args":{{"name":"vm-1"}}}}"#,
                request_id()
            )),
        )
        .await;
        assert!(!result.ok);
        assert!(result.error.unwrap().contains("panic"));
    }

    #[tokio::test]
    async fn timeout_is_reported() {
        let executor = Arc::new(MockExecutor {
            delay: Some(Duration::from_millis(500)),
            ..Default::default()
        });
        let result = execute_with_timeout(
            executor,
            command(&format!(
                r#"{{"request_id":"{}","op":"list"}}"#,
                request_id()
            )),
            Duration::from_millis(20),
        )
        .await;
        assert!(!result.ok);
        assert!(result.error.unwrap().contains("超时"));
    }

    /// 1x1 黑色 PNG（测试用）。
    fn tiny_png() -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, 1, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0, 0, 0]).unwrap();
        }
        out
    }
}
