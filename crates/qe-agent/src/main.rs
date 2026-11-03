//! QEStudio 统一机器 agent 客户端入口（P0 骨架）。
//!
//! 一个二进制、三种用法：
//!
//! - `qe-agent enroll <注册链接>`：走 device-pki 注册，拿 mTLS 设备证书；
//! - `qe-agent run --role node|machine`：常驻运行（镜像导入 / 机器遥测）；
//! - `qe-agent status`：查看本地凭证与自检结果。
//!
//! 本阶段只做**入口收口**：`node` / `machine` 两个角色分别复用
//! `qe-node-agent` / `qe-room-agent` 的库实现，行为与独立二进制一致。
//! `machine` 角色已支持两档遥测（`--fast-interval` / `--full-interval`）与
//! mTLS 默认上报（`--report-mode mtls|bearer`）；`--role both` 在同一个进程里同时跑
//! node + machine（单 systemd 服务、单 IPC socket）。
//!
//! `run --ipc-socket <PATH>` 会额外启动本机 Unix socket IPC（GUI 状态/注册/日志），
//! 协议见 `docs/agent-ipc.md`。
//!
//! 守护进程能力库（进程内直调，不再依赖外部二进制）：`qeaudit` 安全核查、
//! `qe-container` 容器、`virt-*` 虚拟化、`qep2p` P2P 传输，见 [`caps`]。

#![deny(unsafe_code)]

mod caps;
mod ipc;
mod logging;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use qe_device_pki_client::creds::{self, Credentials};
use qe_device_pki_client::enroll::{self, EnrollOptions, EnrollOutcome};

/// 数据目录默认值：`$DEVICE_DATA_DIR`，否则 `./data`（与 `qe-node-agent` 一致）。
const DEFAULT_DATA_DIR: &str = "./data";

#[derive(Debug, Parser)]
#[command(
    name = "qe-agent",
    version,
    about = "QEStudio 机器 agent：device-pki mTLS 注册 / 镜像导入 / 机器遥测"
)]
struct Cli {
    /// 数据目录（默认 `$DEVICE_DATA_DIR`，否则 `./data`）
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// 设备槽位（默认取环境变量 `DEVICE_SLOT`）
    #[arg(long, global = true, value_name = "SLOT")]
    slot: Option<String>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// 用注册链接完成一次性注册（证书由 device-pki 签发）
    Enroll {
        /// 注册链接；传 `-` 表示从 stdin 读取
        link: String,
        /// approval 型：等待管理员批准
        #[arg(long)]
        wait: bool,
        /// 批准等待上限（秒，配合 --wait）
        #[arg(long, default_value_t = 300)]
        wait_secs: u64,
        /// 首次连接：确认服务端证书指纹（TOFU）
        #[arg(long)]
        trust: bool,
    },

    /// 常驻运行
    Run {
        /// 角色：node=镜像/策略（主节点），machine=机器遥测（子机）
        #[arg(long, value_enum, default_value = "machine")]
        role: Role,
        /// 只跑一轮后退出（仅 machine）
        #[arg(long)]
        once: bool,
        /// 把 payload 打印到 stdout（仅 machine）
        #[arg(long)]
        print: bool,
        /// fast 档上报间隔秒（仅 machine，旧参数别名）
        #[arg(long)]
        interval: Option<u64>,
        /// fast 档上报间隔秒（仅 machine，覆盖 ROOM_AGENT_FAST_INTERVAL_SECS）
        #[arg(long)]
        fast_interval: Option<u64>,
        /// full 档上报间隔秒（仅 machine，覆盖 ROOM_AGENT_FULL_INTERVAL_SECS）
        #[arg(long)]
        full_interval: Option<u64>,
        /// 上报模式（仅 machine）：mtls=默认，bearer=兼容旧部署
        #[arg(long, value_enum)]
        report_mode: Option<ReportModeArg>,
        /// 设备槽位覆盖（仅 machine）
        #[arg(long)]
        device_slot: Option<String>,
        /// 本机 IPC socket 路径（GUI 用；守护模式才生效）
        #[arg(long, value_name = "PATH")]
        ipc_socket: Option<PathBuf>,
    },

    /// 查看本地凭证与自检结果
    Status,

    /// 只启动本机 IPC（调试 / 无角色机器；不参与镜像与遥测）
    Serve {
        /// IPC socket 路径
        #[arg(long, value_name = "PATH")]
        ipc_socket: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Role {
    Node,
    Machine,
    /// 同一进程同时跑 node + machine（单服务部署推荐）。
    Both,
}

/// `--report-mode`：与 room-agent 的 `REPORT_MODE` 一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ReportModeArg {
    Mtls,
    Bearer,
}

impl ReportModeArg {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Mtls => "mtls",
            Self::Bearer => "bearer",
        }
    }
}

fn main() -> Result<()> {
    // rustls 依赖树里同时存在 aws-lc-rs 与 ring 时无法自动选 provider，必须显式安装。
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cli = Cli::parse();
    let data_dir = resolve_data_dir(cli.data_dir.as_deref());

    match cli.cmd {
        Cmd::Enroll {
            link,
            wait,
            wait_secs,
            trust,
        } => block_on(enroll_cmd(&link, wait, wait_secs, trust, &data_dir)),
        Cmd::Run {
            role,
            once,
            print,
            interval,
            fast_interval,
            full_interval,
            report_mode,
            device_slot,
            ipc_socket,
        } => {
            let slot = cli
                .slot
                .clone()
                .or_else(|| std::env::var("DEVICE_SLOT").ok());
            let room_cli = || qe_room_agent::cli::Cli {
                once,
                print,
                interval_secs: interval,
                fast_interval_secs: fast_interval,
                full_interval_secs: full_interval,
                report_mode: report_mode.map(|mode| mode.as_str().to_owned()),
                device_slot: device_slot.clone().or_else(|| cli.slot.clone()),
                data_dir: Some(data_dir.clone()),
                help: false,
            };
            // machine 配置解析失败时只影响状态展示；真正报错留给角色启动。
            let machine_info = || qe_room_agent::config::Config::from_env(&room_cli()).ok();

            match role {
                Role::Node => {
                    if once
                        || print
                        || interval.is_some()
                        || fast_interval.is_some()
                        || full_interval.is_some()
                        || report_mode.is_some()
                        || device_slot.is_some()
                    {
                        bail!(
                            "node 角色是常驻下行循环，不支持 --once/--print/--interval/--fast-interval/--full-interval/--report-mode/--device-slot"
                        );
                    }
                    if let Some(socket) = ipc_socket.as_deref() {
                        start_ipc(socket, &data_dir, slot, vec!["node"], None, None)?;
                    }
                    let config = node_config(&data_dir, cli.slot.as_deref())
                        .context("读取 agent 配置失败")?;
                    block_on(qe_node_agent::run_with_config(config))
                }
                Role::Machine => {
                    if let Some(socket) = ipc_socket.as_deref() {
                        if once || print {
                            eprintln!("提示：--once/--print 下不启动 IPC（进程会退出）");
                        } else {
                            let info = machine_info();
                            let mode = info
                                .as_ref()
                                .map(|config| config.report_mode.as_str().to_owned());
                            let intervals = info.as_ref().map(|config| {
                                (config.fast_interval_secs, config.full_interval_secs)
                            });
                            start_ipc(socket, &data_dir, slot, vec!["machine"], mode, intervals)?;
                        }
                    }
                    let code = qe_room_agent::run_with_cli(room_cli());
                    if code != 0 {
                        std::process::exit(code);
                    }
                    Ok(())
                }
                Role::Both => {
                    if once || print {
                        bail!("both 角色是常驻循环，不支持 --once / --print");
                    }
                    let info = machine_info();
                    let mode = info
                        .as_ref()
                        .map(|config| config.report_mode.as_str().to_owned());
                    let intervals = info
                        .as_ref()
                        .map(|config| (config.fast_interval_secs, config.full_interval_secs));
                    if let Some(socket) = ipc_socket.as_deref() {
                        start_ipc(
                            socket,
                            &data_dir,
                            slot,
                            vec!["node", "machine"],
                            mode,
                            intervals,
                        )?;
                    }

                    // machine 角色是 blocking 循环，放到独立线程；node 角色留在主线程。
                    let machine_cli = room_cli();
                    std::thread::Builder::new()
                        .name("qe-agent-machine".to_owned())
                        .spawn(move || {
                            let code = qe_room_agent::run_with_cli(machine_cli);
                            if code != 0 {
                                std::process::exit(code);
                            }
                        })
                        .context("启动 machine 角色线程失败")?;

                    let config = node_config(&data_dir, cli.slot.as_deref())
                        .context("读取 agent 配置失败")?;
                    block_on(qe_node_agent::run_with_config(config))
                }
            }
        }
        Cmd::Status => status_cmd(cli.slot.as_deref(), &data_dir),
        Cmd::Serve { ipc_socket } => {
            let slot = cli
                .slot
                .clone()
                .or_else(|| std::env::var("DEVICE_SLOT").ok());
            start_ipc(&ipc_socket, &data_dir, slot, vec![], None, None)?;
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
    }
}

/// 启动本机 IPC：安装日志环形缓冲（角色自己的 `init_tracing` 会静默失败），再拉起 UDS 服务。
fn start_ipc(
    socket: &Path,
    data_dir: &Path,
    slot: Option<String>,
    roles: Vec<&str>,
    report_mode: Option<String>,
    intervals: Option<(u64, u64)>,
) -> Result<()> {
    let logs = logging::LogBuffer::default();
    logging::init(logs.clone());
    let state = ipc::IpcState::new(
        socket.to_path_buf(),
        data_dir.to_path_buf(),
        slot,
        roles.into_iter().map(ToOwned::to_owned).collect(),
        report_mode,
        intervals,
        logs,
    );
    ipc::spawn(state)
}

/// 数据目录：显式 `--data-dir` > `$DEVICE_DATA_DIR` > `./data`。
fn resolve_data_dir(explicit: Option<&Path>) -> PathBuf {
    if let Some(dir) = explicit {
        return dir.to_path_buf();
    }
    if let Some(raw) = std::env::var_os("DEVICE_DATA_DIR") {
        let path = PathBuf::from(raw);
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    PathBuf::from(DEFAULT_DATA_DIR)
}

/// 构造 node 角色的运行配置：只覆盖数据目录与槽位，其余仍读环境变量。
fn node_config(data_dir: &Path, slot: Option<&str>) -> Result<qe_node_agent::AgentConfig> {
    let data_dir = data_dir.to_path_buf();
    let slot = slot.map(ToOwned::to_owned);
    qe_node_agent::AgentConfig::from_lookup(move |key| match key {
        "DEVICE_DATA_DIR" => Some(data_dir.display().to_string()),
        // 显式 --slot 优先；否则继续读环境变量，不能被 None 覆盖掉。
        "DEVICE_SLOT" => slot.clone().or_else(|| std::env::var("DEVICE_SLOT").ok()),
        other => std::env::var(other).ok(),
    })
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
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
            let slot = enroll::EnrollTarget::parse(&link)
                .map(|target| target.slot)
                .unwrap_or_default();
            println!("◷ 等待管理员批准 (enroll_id = {enroll_id})");
            println!("  批准后重跑并等待：qe-agent enroll \"$LINK\" --wait");
            println!("  或：qe-device-pki-client poll --slot {slot} --wait-secs 300");
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
    // 凭证缺失/损坏不阻塞能力展示：两段都输出，凭证问题最后以非 0 退出码表达。
    let creds_outcome = status_creds(slot, data_dir);
    if let Err(err) = &creds_outcome {
        println!("凭证     : 未就绪（{err}）");
        println!("建议     : qe-agent enroll <注册链接>");
    }
    caps::print(data_dir);
    creds_outcome
}

/// 输出凭证段（目录/slot/serial/mTLS/自检）。
fn status_creds(slot: Option<&str>, data_dir: &Path) -> Result<()> {
    let dir = match slot {
        Some(slot) => creds::locate(slot, Some(data_dir))?,
        None => find_creds_dir(data_dir)?,
    };
    let creds = Credentials::load(&dir)?;
    println!("凭证目录 : {}", dir.display());
    println!("slot     : {}", creds.device_slot);
    println!("serial   : {}", creds.serial);
    println!("mTLS     : {}", creds.mtls_endpoint);
    match creds.verify() {
        Ok(()) => println!("自检     : ok"),
        Err(issue) => {
            println!("自检     : {issue}");
            println!("建议     : {}", issue.advice());
        }
    }
    Ok(())
}

/// `status` 未指定槽位时：凭证目录下恰好一个就选它，多个则要求 `--slot`。
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
            "在 {} 下没有找到凭证；请先运行 `qe-agent enroll <注册链接>`",
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
