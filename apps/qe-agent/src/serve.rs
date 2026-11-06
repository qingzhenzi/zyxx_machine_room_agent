use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};

use crate::ipc;
use crate::logging;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Role {
    Node,
    Machine,
    /// 同一进程同时跑 node + machine（单服务部署推荐）。
    Both,
}

/// `--report-mode`：与 room-agent 的 `REPORT_MODE` 一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReportModeArg {
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

#[derive(Debug, Args)]
pub struct ServeOpts {
    /// 角色：node / machine / both（systemd 推荐 both）
    #[arg(long, value_enum, default_value = "both")]
    pub role: Role,

    /// 本机 IPC socket 路径（GUI 用）
    #[arg(long, value_name = "PATH")]
    pub ipc_socket: Option<PathBuf>,

    /// machine 角色 fast 档上报间隔（秒）
    #[arg(long)]
    pub fast_interval: Option<u64>,

    /// machine 角色 full 档上报间隔（秒）
    #[arg(long)]
    pub full_interval: Option<u64>,

    /// 上报模式（仅 machine/both）
    #[arg(long, value_enum)]
    pub report_mode: Option<ReportModeArg>,

    /// 设备槽位覆盖（仅 machine/both）
    #[arg(long)]
    pub device_slot: Option<String>,
}

pub struct ServeCtx {
    pub opts: ServeOpts,
    pub data_dir: PathBuf,
    pub slot: Option<String>,
}

pub fn run(ctx: ServeCtx) -> Result<()> {
    let ServeCtx {
        opts,
        data_dir,
        slot,
    } = ctx;

    // 守护模式：全量初始化 + 长驻
    let logs = logging::LogBuffer::default();
    logging::init(logs.clone());

    let effective_slot = opts.device_slot.clone().or_else(|| slot.clone());

    let room_cli = || qe_room_agent::cli::Cli {
        once: false,
        print: false,
        interval_secs: None,
        fast_interval_secs: opts.fast_interval,
        full_interval_secs: opts.full_interval,
        report_mode: opts.report_mode.map(|m| m.as_str().to_owned()),
        device_slot: opts.device_slot.clone().or_else(|| effective_slot.clone()),
        data_dir: Some(data_dir.clone()),
        help: false,
    };

    let machine_info = || qe_room_agent::config::Config::from_env(&room_cli()).ok();

    match opts.role {
        Role::Node => {
            if let Some(socket) = opts.ipc_socket.as_deref() {
                start_ipc(
                    socket,
                    &data_dir,
                    effective_slot.clone(),
                    vec!["node"],
                    None,
                    None,
                    logs.clone(),
                )?;
            }
            let config = node_config(&data_dir, effective_slot.as_deref())
                .context("读取 agent 配置失败")?;
            block_on(qe_node_agent::run_with_config(config))
        }
        Role::Machine => {
            if let Some(socket) = opts.ipc_socket.as_deref() {
                let info = machine_info();
                let mode = info
                    .as_ref()
                    .map(|config| config.report_mode.as_str().to_owned());
                let intervals = info
                    .as_ref()
                    .map(|config| (config.fast_interval_secs, config.full_interval_secs));
                start_ipc(
                    socket,
                    &data_dir,
                    effective_slot.clone(),
                    vec!["machine"],
                    mode,
                    intervals,
                    logs.clone(),
                )?;
            }
            let code = qe_room_agent::run_with_cli(room_cli());
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        Role::Both => {
            let info = machine_info();
            let mode = info
                .as_ref()
                .map(|config| config.report_mode.as_str().to_owned());
            let intervals = info
                .as_ref()
                .map(|config| (config.fast_interval_secs, config.full_interval_secs));
            if let Some(socket) = opts.ipc_socket.as_deref() {
                start_ipc(
                    socket,
                    &data_dir,
                    effective_slot.clone(),
                    vec!["node", "machine"],
                    mode,
                    intervals,
                    logs.clone(),
                )?;
            }

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

            let config = node_config(&data_dir, effective_slot.as_deref())
                .context("读取 agent 配置失败")?;
            block_on(qe_node_agent::run_with_config(config))
        }
    }
}

fn start_ipc(
    socket: &std::path::Path,
    data_dir: &std::path::Path,
    slot: Option<String>,
    roles: Vec<&str>,
    report_mode: Option<String>,
    intervals: Option<(u64, u64)>,
    logs: logging::LogBuffer,
) -> Result<()> {
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

fn node_config(
    data_dir: &std::path::Path,
    slot: Option<&str>,
) -> Result<qe_node_agent::AgentConfig> {
    let data_dir = data_dir.to_path_buf();
    let slot = slot.map(ToOwned::to_owned);
    qe_node_agent::AgentConfig::from_lookup(move |key| match key {
        "DEVICE_DATA_DIR" => Some(data_dir.display().to_string()),
        "DEVICE_SLOT" => slot.clone().or_else(|| std::env::var("DEVICE_SLOT").ok()),
        other => std::env::var(other).ok(),
    })
}

use crate::block_on;
