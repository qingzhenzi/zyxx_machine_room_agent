//! 命令行参数解析（`--once` / `--print` / 两档间隔 / `--device-slot` 等）。
//!
//! 只做参数收集与语法校验；取值校验（区间、必填等）在 [`crate::config`]。
//! 显式 CLI 参数优先于同名环境变量，环境变量优先于内置默认值。

use std::path::PathBuf;

use anyhow::{Result, bail};

/// 支持的帮助文本。
pub const USAGE: &str = "\
qe-agent-telemetry —— QEStudio 房间机器遥测 agent

用法:
  qe-agent-telemetry [--once] [--print] [--fast-interval <秒>] [--full-interval <秒>]
                [--report-mode <mtls|bearer>] [--device-slot <槽位>] [--data-dir <目录>]

选项:
  --once                  采集/上报一份 full 档后退出；失败时退出码非 0
  --print                 把 payload 打印到 stdout（不发送；不需要证书/token）
  --fast-interval <秒>    fast 档间隔（内置指标），覆盖 ROOM_AGENT_FAST_INTERVAL_SECS（5..=60，默认 10）
  --full-interval <秒>    full 档间隔（含外部工具），覆盖 ROOM_AGENT_FULL_INTERVAL_SECS（60..=3600，默认 300）
  --interval <秒>         旧参数，等价于 --fast-interval（兼容）
  --report-mode <模式>    上报模式：mtls（默认）或 bearer，覆盖 REPORT_MODE
  --device-slot <槽位>    设备槽位，覆盖 DEVICE_SLOT
  --data-dir <目录>       数据目录（凭证在 <目录>/creds/<槽位>），覆盖 DEVICE_DATA_DIR（默认 ./data）
  -h, --help              打印本帮助

必填环境变量（mtls 模式，默认）:
  DEVICE_SLOT            设备槽位（也可用 --device-slot 传入）
  DEVICE_PKI_MTLS_URL    device-pki mTLS 端点，如 https://node.example.com:4441
  （证书从 <DEVICE_DATA_DIR>/creds/<DEVICE_SLOT> 读取；先用 qe-agent cli enroll 注册）

必填环境变量（bearer 模式，兼容旧部署）:
  MACHINE_AGENT_TOKEN    上报 Bearer token（--print 模式下可省略）
  MACHINE_API_BASE       qe-api 基础地址（默认 http://127.0.0.1:8080/api/v1）

示例（统一入口）:
  DEVICE_PKI_MTLS_URL=https://node:4441 DEVICE_SLOT=room-101 qe-agent serve --role machine
  REPORT_MODE=bearer MACHINE_AGENT_TOKEN=fleet-token DEVICE_SLOT=room-101 qe-agent serve --role machine
";

/// 解析后的命令行选项。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cli {
    /// `--once`：只跑一份 full 后退出。
    pub once: bool,
    /// `--print`：只打印 payload，不发送。
    pub print: bool,
    /// `--interval`（兼容旧参数）的原始值；映射到 fast 档。
    pub interval_secs: Option<u64>,
    /// `--fast-interval` 的原始值（交给 config 做区间校验）。
    pub fast_interval_secs: Option<u64>,
    /// `--full-interval` 的原始值（交给 config 做区间校验）。
    pub full_interval_secs: Option<u64>,
    /// `--report-mode`：`mtls`（默认）/ `bearer`。
    pub report_mode: Option<String>,
    /// `--device-slot`：覆盖 `DEVICE_SLOT`。
    pub device_slot: Option<String>,
    /// `--data-dir`：覆盖 `DEVICE_DATA_DIR`。
    pub data_dir: Option<PathBuf>,
    /// `-h` / `--help`。
    pub help: bool,
}

/// 解析 `std::env::args().skip(1)` 得到的参数序列。
pub fn parse<I>(args: I) -> Result<Cli>
where
    I: IntoIterator<Item = String>,
{
    let mut cli = Cli::default();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--once" => cli.once = true,
            "--print" => cli.print = true,
            "-h" | "--help" => cli.help = true,
            "--interval" => {
                let raw = next_value(&mut iter, "--interval")?;
                cli.interval_secs = Some(parse_interval("--interval", &raw)?);
            }
            "--fast-interval" => {
                let raw = next_value(&mut iter, "--fast-interval")?;
                cli.fast_interval_secs = Some(parse_interval("--fast-interval", &raw)?);
            }
            "--full-interval" => {
                let raw = next_value(&mut iter, "--full-interval")?;
                cli.full_interval_secs = Some(parse_interval("--full-interval", &raw)?);
            }
            "--report-mode" => {
                let raw = next_value(&mut iter, "--report-mode")?;
                cli.report_mode = Some(parse_report_mode(&raw)?);
            }
            "--device-slot" => {
                let raw = next_value(&mut iter, "--device-slot")?;
                cli.device_slot = Some(parse_slot(&raw)?);
            }
            "--data-dir" => {
                let raw = next_value(&mut iter, "--data-dir")?;
                cli.data_dir = Some(parse_dir(&raw)?);
            }
            other => {
                if let Some(raw) = other.strip_prefix("--interval=") {
                    cli.interval_secs = Some(parse_interval("--interval", raw)?);
                } else if let Some(raw) = other.strip_prefix("--fast-interval=") {
                    cli.fast_interval_secs = Some(parse_interval("--fast-interval", raw)?);
                } else if let Some(raw) = other.strip_prefix("--full-interval=") {
                    cli.full_interval_secs = Some(parse_interval("--full-interval", raw)?);
                } else if let Some(raw) = other.strip_prefix("--report-mode=") {
                    cli.report_mode = Some(parse_report_mode(raw)?);
                } else if let Some(raw) = other.strip_prefix("--device-slot=") {
                    cli.device_slot = Some(parse_slot(raw)?);
                } else if let Some(raw) = other.strip_prefix("--data-dir=") {
                    cli.data_dir = Some(parse_dir(raw)?);
                } else {
                    bail!("未知参数 `{other}`（可用 --help 查看用法）");
                }
            }
        }
    }
    Ok(cli)
}

fn next_value<I>(iter: &mut I, flag: &str) -> Result<String>
where
    I: Iterator<Item = String>,
{
    match iter.next() {
        Some(value) if !value.starts_with("--") => Ok(value),
        Some(value) => bail!("{flag} 缺少取值，实际拿到 `{value}`"),
        None => bail!("{flag} 缺少取值"),
    }
}

fn parse_interval(flag: &str, raw: &str) -> Result<u64> {
    raw.trim()
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("{flag} 不是合法整数: `{raw}`"))
}

fn parse_slot(raw: &str) -> Result<String> {
    let slot = raw.trim();
    if slot.is_empty() {
        bail!("--device-slot 不能为空");
    }
    Ok(slot.to_owned())
}

fn parse_report_mode(raw: &str) -> Result<String> {
    let mode = raw.trim().to_ascii_lowercase();
    if !matches!(mode.as_str(), "mtls" | "bearer") {
        bail!("--report-mode 只能是 mtls / bearer，实际 `{raw}`");
    }
    Ok(mode)
}

fn parse_dir(raw: &str) -> Result<PathBuf> {
    let dir = raw.trim();
    if dir.is_empty() {
        bail!("--data-dir 不能为空");
    }
    Ok(PathBuf::from(dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Cli> {
        parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn no_args_is_default() {
        assert_eq!(parse_args(&[]).unwrap(), Cli::default());
    }

    #[test]
    fn flags_and_values_are_parsed() {
        let cli = parse_args(&[
            "--once",
            "--print",
            "--fast-interval",
            "20",
            "--full-interval",
            "600",
            "--report-mode",
            "bearer",
            "--device-slot",
            " room-101 ",
            "--data-dir",
            "/var/lib/qe",
        ])
        .unwrap();
        assert!(cli.once);
        assert!(cli.print);
        assert_eq!(cli.fast_interval_secs, Some(20));
        assert_eq!(cli.full_interval_secs, Some(600));
        assert_eq!(cli.report_mode.as_deref(), Some("bearer"));
        assert_eq!(cli.device_slot.as_deref(), Some("room-101"));
        assert_eq!(
            cli.data_dir.as_deref(),
            Some(std::path::Path::new("/var/lib/qe"))
        );
    }

    #[test]
    fn legacy_interval_flag_is_kept() {
        let cli = parse_args(&["--interval", "15"]).unwrap();
        assert_eq!(cli.interval_secs, Some(15));
        assert!(cli.fast_interval_secs.is_none());
    }

    #[test]
    fn equals_form_is_supported() {
        let cli = parse_args(&[
            "--interval=45",
            "--fast-interval=30",
            "--full-interval=900",
            "--report-mode=MTLS",
            "--device-slot=room-102",
            "--data-dir=./data",
        ])
        .unwrap();
        assert_eq!(cli.interval_secs, Some(45));
        assert_eq!(cli.fast_interval_secs, Some(30));
        assert_eq!(cli.full_interval_secs, Some(900));
        assert_eq!(cli.report_mode.as_deref(), Some("mtls"));
        assert_eq!(cli.device_slot.as_deref(), Some("room-102"));
        assert_eq!(
            cli.data_dir.as_deref(),
            Some(std::path::Path::new("./data"))
        );
    }

    #[test]
    fn help_is_recognized() {
        assert!(parse_args(&["-h"]).unwrap().help);
        assert!(parse_args(&["--help"]).unwrap().help);
    }

    #[test]
    fn unknown_argument_is_rejected() {
        let err = parse_args(&["--wat"]).unwrap_err();
        assert!(err.to_string().contains("未知参数"), "{err}");
    }

    #[test]
    fn missing_value_is_rejected() {
        for args in [
            vec!["--interval"],
            vec!["--fast-interval"],
            vec!["--full-interval"],
            vec!["--report-mode"],
            vec!["--device-slot"],
            vec!["--data-dir"],
        ] {
            let err = parse_args(&args).unwrap_err();
            assert!(err.to_string().contains("缺少取值"), "{err}");
        }
        let err = parse_args(&["--interval", "--print"]).unwrap_err();
        assert!(err.to_string().contains("缺少取值"), "{err}");
    }

    #[test]
    fn malformed_values_are_rejected() {
        assert!(parse_args(&["--interval", "abc"]).is_err());
        assert!(parse_args(&["--fast-interval", "-5"]).is_err());
        assert!(parse_args(&["--full-interval", "x"]).is_err());
        assert!(parse_args(&["--device-slot", "   "]).is_err());
        assert!(parse_args(&["--report-mode", "ftp"]).is_err());
        assert!(parse_args(&["--data-dir", "  "]).is_err());
    }
}
