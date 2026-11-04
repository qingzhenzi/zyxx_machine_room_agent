//! QEStudio 机器 agent 入口：serve（守护）/ cli（工具）两条独立路径。

#![deny(unsafe_code)]

mod caps;
mod cli;
mod ipc;
mod logging;
mod serve;
mod vm;

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};

/// 数据目录默认值：`$DEVICE_DATA_DIR`，否则 `./data`。
const DEFAULT_DATA_DIR: &str = "./data";

#[derive(Debug, Parser)]
#[command(
    name = "qe-agent",
    version,
    about = "QEStudio 机器 agent：serve（常驻守护） / cli（单次工具）"
)]
struct Cli {
    /// 数据目录（默认 `$DEVICE_DATA_DIR`，否则 `./data`）
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// 设备槽位（默认取环境变量 `DEVICE_SLOT`）
    #[arg(long, global = true, value_name = "SLOT")]
    slot: Option<String>,

    #[command(subcommand)]
    mode: Mode,
}

#[derive(Debug, Subcommand)]
enum Mode {
    /// 常驻守护：全量初始化 + 长驻运行
    Serve(serve::ServeOpts),
    /// 单次交互工具：最小初始化 + 即走即退
    Cli { #[command(subcommand)] cmd: cli::CliCmd },
}

pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("创建 Tokio runtime 失败")
        .block_on(future)
}

fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cli = Cli::parse();

    let data_dir = resolve_data_dir(cli.data_dir.as_deref());
    let slot = cli
        .slot
        .clone()
        .or_else(|| std::env::var("DEVICE_SLOT").ok());

    match cli.mode {
        Mode::Serve(opts) => serve::run(serve::ServeCtx {
            opts,
            data_dir,
            slot,
        }),
        Mode::Cli { cmd } => cli::run(cli::CliCtx {
            cmd,
            data_dir,
            slot,
        }),
    }
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
