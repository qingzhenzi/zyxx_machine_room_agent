use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
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

    // 物理键盘调试解锁（Wayland 下 Tauri 全局快捷键不一定收得到）。
    crate::debug_hotkey::spawn();

    let effective_slot = opts.device_slot.clone().or_else(|| slot.clone());

    let room_cli = || qe_agent_telemetry::cli::Cli {
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

    let machine_info = || qe_agent_telemetry::config::Config::from_env(&room_cli()).ok();

    // P2P 节点由 boot services 在独立线程懒启动；node 角色每次 image.pull 时再取，
    // 避免启动时序竞争（未启用 QE_P2P_ENABLED 时该 provider 不会被使用）。
    let p2p_provider: qe_agent_node::P2pNodeProvider = Arc::new(crate::boot::services::p2p_node);

    // 引导流水线：IPC 已就绪（各角色上方 start_ipc），防火墙/展示层/检查/
    // 注册/服务检测顺序执行；阶段失败不阻塞 serve。
    let boot = crate::boot::pipeline::spawn(data_dir.clone(), effective_slot.clone());

    // 远端 config.update 下发成功后联动 boot 周期检查：full 档间隔即
    // "周期性深度检查"节奏，直接作为 checks 复检间隔。
    qe_agent_node::on_remote_policy_applied({
        let boot_handle = boot.clone();
        move |full_interval_secs| boot_handle.set_checks_interval(full_interval_secs)
    });

    match opts.role {
        Role::Node => {
            if let Some(socket) = opts.ipc_socket.as_deref() {
                start_ipc(IpcArgs {
                    socket,
                    data_dir: &data_dir,
                    slot: effective_slot.clone(),
                    roles: vec!["node"],
                    report_mode: None,
                    intervals: None,
                    logs: logs.clone(),
                    boot: Some(boot.clone()),
                })?;
            }
            let config =
                node_config(&data_dir, effective_slot.as_deref()).context("读取 agent 配置失败")?;
            block_on(qe_agent_node::run_with_p2p(
                config,
                Some(Arc::clone(&p2p_provider)),
            ))
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
                start_ipc(IpcArgs {
                    socket,
                    data_dir: &data_dir,
                    slot: effective_slot.clone(),
                    roles: vec!["machine"],
                    report_mode: mode,
                    intervals,
                    logs: logs.clone(),
                    boot: Some(boot.clone()),
                })?;
            }
            let code = qe_agent_telemetry::run_with_cli(room_cli());
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
                start_ipc(IpcArgs {
                    socket,
                    data_dir: &data_dir,
                    slot: effective_slot.clone(),
                    roles: vec!["node", "machine"],
                    report_mode: mode,
                    intervals,
                    logs: logs.clone(),
                    boot: Some(boot.clone()),
                })?;
            }

            let machine_cli = room_cli();
            std::thread::Builder::new()
                .name("qe-agent-machine".to_owned())
                .spawn(move || {
                    let code = qe_agent_telemetry::run_with_cli(machine_cli);
                    if code != 0 {
                        std::process::exit(code);
                    }
                })
                .context("启动 machine 角色线程失败")?;

            let config =
                node_config(&data_dir, effective_slot.as_deref()).context("读取 agent 配置失败")?;
            block_on(qe_agent_node::run_with_p2p(
                config,
                Some(Arc::clone(&p2p_provider)),
            ))
        }
    }
}

struct IpcArgs<'a> {
    socket: &'a std::path::Path,
    data_dir: &'a std::path::Path,
    slot: Option<String>,
    roles: Vec<&'a str>,
    report_mode: Option<String>,
    intervals: Option<(u64, u64)>,
    logs: logging::LogBuffer,
    boot: Option<crate::boot::PipelineHandle>,
}

fn start_ipc(args: IpcArgs<'_>) -> Result<()> {
    let IpcArgs {
        socket,
        data_dir,
        slot,
        roles,
        report_mode,
        intervals,
        logs,
        boot,
    } = args;
    let mut state = ipc::IpcState::new(
        socket.to_path_buf(),
        data_dir.to_path_buf(),
        slot,
        roles.into_iter().map(ToOwned::to_owned).collect(),
        report_mode,
        intervals,
        logs,
    );
    state.boot = boot;
    ipc::spawn(state)
}

fn node_config(
    data_dir: &std::path::Path,
    slot: Option<&str>,
) -> Result<qe_agent_node::AgentConfig> {
    let data_dir = data_dir.to_path_buf();
    let slot = slot.map(ToOwned::to_owned);
    qe_agent_node::AgentConfig::from_lookup(move |key| match key {
        "DEVICE_DATA_DIR" => Some(data_dir.display().to_string()),
        "DEVICE_SLOT" => slot.clone().or_else(|| std::env::var("DEVICE_SLOT").ok()),
        other => std::env::var(other).ok(),
    })
}

use crate::block_on;
