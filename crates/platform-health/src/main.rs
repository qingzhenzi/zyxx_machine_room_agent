//! Command line front-end: `collect` (machine-readable) and `check` (human +
//! exit code, for scripts and schedulers).

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use platform_health::{Options, Privacy, SCHEMA_VERSION, Thresholds, health, snapshot_with};

const USAGE: &str = "\
platform-health — device health snapshot

USAGE:
    platform-health [collect] [OPTIONS]
    platform-health check   [OPTIONS]
    platform-health --help | --version

COMMANDS:
    collect   Print a JSON snapshot (default)
    check     Print a one-line verdict; exit 0 healthy, 1 degraded, 2 critical

OPTIONS:
    -p, --pretty            Indent JSON output
        --redact            Mask hostname, MAC addresses, serials and asset tags
        --processes [N]     Include the process table, keeping the top N (default 10)
        --core-only         Skip optional inventory domains (battery/GPU/Wi-Fi/...)
        --thresholds FILE   JSON file overriding the default limits (partial is fine)
        --watch SECS        Repeat collection every SECS seconds (collect only)
    -o, --output FILE       Write to FILE instead of stdout
    -h, --help              Show this help
    -V, --version           Show version
";

#[derive(PartialEq)]
enum Command {
    Collect,
    Check,
}

struct Cli {
    command: Command,
    opts: Options,
    pretty: bool,
    watch: Option<u64>,
    output: Option<PathBuf>,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("error: {message}\n\n{USAGE}");
            ExitCode::from(64)
        }
    }
}

fn run() -> Result<u8, String> {
    let cli = parse_args()?;

    match cli.command {
        Command::Collect => {
            let mut sink: Box<dyn Write> = match &cli.output {
                Some(path) => Box::new(
                    std::fs::File::create(path)
                        .map_err(|e| format!("cannot create {}: {e}", path.display()))?,
                ),
                None => Box::new(std::io::stdout().lock()),
            };

            loop {
                let snap = snapshot_with(&cli.opts);
                let json = if cli.pretty {
                    serde_json::to_string_pretty(&snap)
                } else {
                    serde_json::to_string(&snap)
                }
                .map_err(|e| e.to_string())?;

                writeln!(sink, "{json}").map_err(|e| e.to_string())?;
                sink.flush().map_err(|e| e.to_string())?;

                match cli.watch {
                    Some(secs) => std::thread::sleep(std::time::Duration::from_secs(secs)),
                    None => return Ok(0),
                }
            }
        }
        Command::Check => {
            let snap = snapshot_with(&cli.opts);
            let report = &snap.health;
            println!(
                "{:<8} score {:>3}  {} issue(s)",
                report.status.as_str(),
                report.score,
                report.issues.len()
            );
            for issue in &report.issues {
                println!(
                    "  [{:<8}] {:<18} {:<16} {}",
                    issue.severity.as_str(),
                    issue.code,
                    issue.subject,
                    issue.message
                );
            }
            Ok(match report.status {
                health::Status::Healthy => 0,
                health::Status::Degraded => 1,
                health::Status::Critical => 2,
            })
        }
    }
}

fn parse_args() -> Result<Cli, String> {
    let mut args = std::env::args().skip(1).peekable();
    let mut cli = Cli {
        command: Command::Collect,
        opts: Options::default(),
        pretty: false,
        watch: None,
        output: None,
    };

    // First positional argument, if any, is the command.
    let command = args.peek().filter(|arg| !arg.starts_with('-')).cloned();
    if let Some(command) = command {
        args.next();
        cli.command = match command.as_str() {
            "collect" => Command::Collect,
            "check" => Command::Check,
            "help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown command `{other}`")),
        };
    }

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-p" | "--pretty" => cli.pretty = true,
            "--redact" => cli.opts.privacy = Privacy::Redacted,
            "--core-only" => cli.opts.include_optional = false,
            "--processes" => {
                cli.opts.include_processes = true;
                if let Some(value) = args.peek().and_then(|v| v.parse::<usize>().ok()) {
                    cli.opts.top_processes = value;
                    args.next();
                }
            }
            "--thresholds" => {
                let path = args
                    .next()
                    .ok_or_else(|| "--thresholds needs a path".to_string())?;
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("cannot read {path}: {e}"))?;
                let parsed: Thresholds = serde_json::from_str(&text)
                    .map_err(|e| format!("invalid thresholds in {path}: {e}"))?;
                cli.opts.thresholds = parsed;
            }
            "--watch" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--watch needs a number of seconds".to_string())?;
                let secs: u64 = value
                    .parse()
                    .map_err(|_| format!("invalid --watch value `{value}`"))?;
                if secs == 0 {
                    return Err("--watch must be at least 1 second".to_string());
                }
                cli.watch = Some(secs);
            }
            "-o" | "--output" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--output needs a path".to_string())?;
                cli.output = Some(PathBuf::from(value));
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!(
                    "platform-health {} ({SCHEMA_VERSION})",
                    env!("CARGO_PKG_VERSION")
                );
                std::process::exit(0);
            }
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }

    if cli.command == Command::Check && cli.watch.is_some() {
        return Err("--watch is only supported by `collect`".to_string());
    }

    Ok(cli)
}
