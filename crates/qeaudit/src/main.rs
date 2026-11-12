//! qeaudit CLI 入口（薄壳）：解析命令行后调用 [`qeaudit`] 库执行扫描。
//!
//! 核查逻辑全部在库侧（`src/lib.rs`），守护进程（`qe-agent`）进程内调用
//! 与本 CLI 走同一条代码路径，行为不会漂移。

use clap::{Parser, Subcommand, ValueEnum};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use qeaudit::{Config, FailOn, ScanOptions};

#[derive(Parser, Debug)]
#[command(
    name = "qeaudit",
    version,
    about = "Linux 主机安全基线核查：引导、虚拟化、设备、磁盘、网络、root、进程"
)]
struct Cli {
    /// 基线策略文件（TOML）。缺省使用内置默认策略。
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// 启动时对自身进程加固（no_new_privs / 禁止 dump / mlockall）。
    #[arg(long, global = true)]
    harden: bool,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// 执行一轮核查并输出报告。
    Scan {
        /// 输出格式。
        #[arg(short, long, value_enum, default_value_t = Format::Table)]
        format: Format,

        /// 写入文件（缺省写 stdout）。
        #[arg(short, long)]
        out: Option<PathBuf>,

        /// 退出码判定策略。
        #[arg(long, value_enum, default_value_t = FailOn::Fail)]
        fail_on: FailOn,

        /// 只跑指定检查器，逗号分隔（grub,kernel,virt,space,device,network,root,process）。
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
    },
    /// 列出全部检查项。
    List,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Format {
    Table,
    Json,
}

fn main() -> ExitCode {
    let mut cli = Cli::parse();

    let cmd = cli.cmd.take().unwrap_or(Cmd::Scan {
        format: Format::Table,
        out: None,
        fail_on: FailOn::Fail,
        only: Vec::new(),
    });

    match cmd {
        Cmd::List => {
            for c in qeaudit::check::registry::all() {
                println!("{:<10} {}", c.id(), c.title());
            }
            ExitCode::SUCCESS
        }
        Cmd::Scan {
            format,
            out,
            fail_on,
            only,
        } => scan(&cli, format, out, fail_on, only),
    }
}

fn scan(
    cli: &Cli,
    format: Format,
    out: Option<PathBuf>,
    fail_on: FailOn,
    only: Vec<String>,
) -> ExitCode {
    if cli.harden {
        let steps = qeaudit::harden::apply();
        qeaudit::harden::print_steps(&steps);
    }

    let cfg = match &cli.config {
        Some(path) => match Config::load(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("配置错误: {e}");
                return ExitCode::from(3);
            }
        },
        None => Config::default(),
    };

    let report = qeaudit::scan_report(&cfg, &ScanOptions { only });

    let text = match format {
        Format::Table => report.render(),
        Format::Json => match serde_json::to_string_pretty(&report) {
            Ok(s) => s + "\n",
            Err(e) => {
                eprintln!("序列化失败: {e}");
                return ExitCode::from(3);
            }
        },
    };

    match out {
        Some(path) => {
            if let Err(e) = std::fs::write(&path, &text) {
                eprintln!("写入 {} 失败: {e}", path.display());
                return ExitCode::from(3);
            }
            // 表格模式写文件时仍打一行摘要，避免交互式使用「没反应」。
            if matches!(format, Format::Table) {
                eprintln!(
                    "报告已写入 {}（fail={} warn={}）",
                    path.display(),
                    report.summary.fail,
                    report.summary.warn
                );
            }
        }
        None => {
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
    }

    ExitCode::from(report.exit_code(fail_on))
}
