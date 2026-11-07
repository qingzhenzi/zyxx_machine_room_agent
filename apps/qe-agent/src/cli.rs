use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use qe_device_pki_client::creds::{self, Credentials};
use qe_device_pki_client::enroll::{self, EnrollOptions, EnrollOutcome};

use crate::vm;

#[derive(Debug, Subcommand)]
pub enum CliCmd {
    /// 用注册链接完成一次性注册
    Enroll {
        /// 注册链接；传 `-` 表示从 stdin 读取
        link: String,
        /// approval 型：等待管理员批准
        #[arg(long)]
        wait: bool,
        /// 批准等待上限（秒）
        #[arg(long, default_value_t = 300)]
        wait_secs: u64,
        /// 首次连接：确认服务端证书指纹（TOFU）
        #[arg(long)]
        trust: bool,
    },

    /// 本机虚拟机管理（libvirt）
    Vm {
        #[command(flatten)]
        args: vm::VmArgs,
    },

    /// 宿主机 GPU / PCI 显示设备（GPU 直通辅助）
    Gpu {
        #[command(subcommand)]
        cmd: GpuCmd,
    },

    /// 查看本地凭证与宿主机能力
    Status,
}

#[derive(Debug, Subcommand)]
pub enum GpuCmd {
    /// 列出可直通的显示设备（读 /sys/bus/pci/devices）
    List,
}

pub struct CliCtx {
    pub cmd: CliCmd,
    pub data_dir: PathBuf,
    pub slot: Option<String>,
}

pub fn run(ctx: CliCtx) -> Result<()> {
    let CliCtx {
        cmd,
        data_dir,
        slot,
    } = ctx;

    match cmd {
        CliCmd::Enroll {
            link,
            wait,
            wait_secs,
            trust,
        } => block_on(enroll_cmd(&link, wait, wait_secs, trust, &data_dir)),
        CliCmd::Vm { args } => vm::run(args, &data_dir, slot.as_deref()),
        CliCmd::Gpu { cmd } => match cmd {
            GpuCmd::List => crate::gpu::list(),
        },
        CliCmd::Status => status_cmd(slot.as_deref(), &data_dir),
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("创建 Tokio runtime 失败")
        .block_on(future)
}

async fn enroll_cmd(
    raw_link: &str,
    wait: bool,
    wait_secs: u64,
    trust: bool,
    data_dir: &Path,
) -> Result<()> {
    let link = read_link(raw_link)?;
    println!("注册链接: {}", enroll::link_summary(&link)?);

    let opts = EnrollOptions {
        approval_wait: wait.then(|| Duration::from_secs(wait_secs.max(1))),
        trust_confirmed: trust,
        data_dir: Some(data_dir.to_path_buf()),
    };

    match enroll::enroll_link(&link, &opts).await? {
        EnrollOutcome::Issued(creds) => {
            let dir = enroll::creds_dir(&creds.device_slot, Some(data_dir))?;
            println!("✓ 注册成功");
            println!("  slot     : {}", creds.device_slot);
            println!("  serial   : {}", creds.serial);
            println!("  mTLS     : {}", creds.mtls_endpoint);
            println!("  凭证目录 : {}", dir.display());
            Ok(())
        }
        EnrollOutcome::PendingApproval { enroll_id } => {
            let slot_hint = enroll::EnrollTarget::parse(&link)
                .map(|target| target.slot)
                .unwrap_or_default();
            println!("◷ 等待管理员批准 (enroll_id = {enroll_id})");
            println!("  批准后重跑并等待：qe-agent cli enroll \"$LINK\" --wait");
            println!("  或：qe-device-pki-client poll --slot {slot_hint} --wait-secs 300");
            Ok(())
        }
    }
}

fn read_link(raw: &str) -> Result<String> {
    if raw != "-" {
        return Ok(raw.to_owned());
    }
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("从 stdin 读取注册链接")?;
    let link = input.trim();
    if link.is_empty() {
        bail!("stdin 中没有注册链接");
    }
    Ok(link.to_owned())
}

fn status_cmd(slot: Option<&str>, data_dir: &Path) -> Result<()> {
    let creds_outcome = status_creds(slot, data_dir);
    if let Err(err) = &creds_outcome {
        println!("凭证     : 未就绪（{err}）");
        println!("建议     : qe-agent cli enroll <注册链接>");
    }
    crate::caps::print(data_dir);
    creds_outcome
}

fn status_creds(slot: Option<&str>, data_dir: &Path) -> Result<()> {
    let dir = match slot {
        Some(slot) => creds::locate(slot, Some(data_dir))?,
        None => find_creds_dir(data_dir)?,
    };
    let (creds, cn) = Credentials::load_with_cn(&dir)?;
    println!("凭证目录 : {}", dir.display());
    // CN 可能是 device_id（UUID）或旧的 slot
    if uuid::Uuid::parse_str(&cn).is_ok() {
        println!("device_id : {}", cn);
        println!("slot      : {}", creds.device_slot);
    } else {
        println!("slot      : {}", cn);
        println!("device_id : 未知（旧证书）");
    }
    println!("serial    : {}", creds.serial);
    println!("mTLS      : {}", creds.mtls_endpoint);
    match creds.verify() {
        Ok(()) => println!("自检      : ok"),
        Err(issue) => {
            println!("自检      : {issue}");
            println!("建议      : {}", issue.advice());
        }
    }
    Ok(())
}

fn find_creds_dir(data_dir: &Path) -> Result<PathBuf> {
    let root = data_dir.join("creds");
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            if entry.file_type().map(|file| file.is_dir()).unwrap_or(false) {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    match found.len() {
        0 => bail!(
            "在 {} 下没有找到凭证；请先运行 `qe-agent cli enroll <注册链接>`",
            root.display()
        ),
        1 => Ok(found.remove(0)),
        _ => {
            let slots: Vec<String> = found
                .iter()
                .map(|path| {
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            bail!("找到多个凭证目录（{}），请用 --slot 指定", slots.join(", "));
        }
    }
}
