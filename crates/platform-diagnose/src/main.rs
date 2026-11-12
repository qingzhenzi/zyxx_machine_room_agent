//! Command-line diagnostic tool.
//!
//! Samples the machine for a short window, runs the rule engine, and prints a
//! ranked root-cause report. Exit code: 0 = healthy, 1 = findings, 2 = a
//! high-confidence critical cause.

use std::process::ExitCode;
use std::time::Duration;

use platform_diagnose::{Diagnoser, Options, ProbeConfig, ResourceKind, Symptom};

const USAGE: &str = "\
platform-diagnose — rule-based device fault diagnosis

USAGE:
    platform-diagnose [OPTIONS]

OPTIONS:
    -s, --symptom KIND      lag | timeout | network | app | resource | disk | generic
                            (default: generic)
    -t, --target HOST:PORT  Target for --symptom timeout (also enables a probe)
        --probes LIST       Comma-separated host:port targets to probe (RTT)
        --dns NAME          Reference hostname to time DNS resolution for
    -d, --duration SECS     Sampling window duration (default: 6)
    -i, --interval SECS     Interval between samples (default: 1)
    -w, --window N          Max samples kept for trend analysis (default: 10)
        --no-baseline       Skip the platform-health snapshot (host rules off)
        --json              Print the diagnosis as JSON instead of text
    -h, --help              Show this help
    -V, --version           Show version

EXAMPLES:
    platform-diagnose -s lag
    platform-diagnose -s timeout -t api.internal:443 --dns api.internal
    platform-diagnose -s network --probes 1.1.1.1:443,10.0.0.1:80 --json
";

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(msg) => {
            eprintln!("error: {msg}\n\n{USAGE}");
            ExitCode::from(64)
        }
    }
}

fn run() -> Result<u8, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut symptom_kind = "generic".to_string();
    let mut target: Option<String> = None;
    let mut probes: Vec<String> = Vec::new();
    let mut dns: Option<String> = None;
    let mut duration = 6u64;
    let mut interval = 1u64;
    let mut window = 10usize;
    let mut baseline = true;
    let mut json = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let next = |i: &mut usize| -> Result<String, String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        match arg {
            "-s" | "--symptom" => symptom_kind = next(&mut i)?,
            "-t" | "--target" => target = Some(next(&mut i)?),
            "--probes" => {
                probes = next(&mut i)?
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "--dns" => dns = Some(next(&mut i)?),
            "-d" | "--duration" => {
                duration = next(&mut i)?
                    .parse()
                    .map_err(|_| "invalid --duration".to_string())?
            }
            "-i" | "--interval" => {
                interval = next(&mut i)?
                    .parse()
                    .map_err(|_| "invalid --interval".to_string())?
            }
            "-w" | "--window" => {
                window = next(&mut i)?
                    .parse()
                    .map_err(|_| "invalid --window".to_string())?
            }
            "--no-baseline" => baseline = false,
            "--json" => json = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("platform-diagnose {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => return Err(format!("unexpected argument `{other}`")),
        }
        i += 1;
    }

    let symptom = build_symptom(&symptom_kind, target.as_deref())?;

    // Probes: explicit list + the timeout target (if any).
    let mut probe_targets = probes;
    if let Some(t) = &target
        && matches!(symptom, Symptom::Timeout { .. })
        && !probe_targets.contains(t)
    {
        probe_targets.push(t.clone());
    }
    let probe = if probe_targets.is_empty() && dns.is_none() {
        None
    } else {
        Some(ProbeConfig {
            targets: probe_targets,
            dns_name: dns,
        })
    };

    let opts = Options {
        window_size: window,
        probe,
        collect_baseline: baseline,
        ..Options::default()
    };

    let mut d = Diagnoser::new(opts);
    let report = d.sweep(
        &symptom,
        Duration::from_secs(duration.max(1)),
        Duration::from_secs(interval.max(1)),
    );

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        print_text(&report);
    }

    Ok(exit_code(&report))
}

fn build_symptom(kind: &str, target: Option<&str>) -> Result<Symptom, String> {
    Ok(match kind {
        "lag" => Symptom::Lag,
        "timeout" => Symptom::Timeout {
            target: target.unwrap_or("unknown:0").to_string(),
        },
        "network" => Symptom::SlowNetwork,
        "app" => Symptom::AppUnresponsive {
            app: "unknown".to_string(),
        },
        "resource" => Symptom::HighResource {
            resource: ResourceKind::Cpu,
        },
        "disk" => Symptom::DiskFull {
            mount: "/".to_string(),
        },
        "generic" => Symptom::Generic,
        other => return Err(format!("unknown symptom `{other}`")),
    })
}

fn print_text(report: &platform_diagnose::Diagnosis) {
    println!("symptom : {}", report.symptom);
    println!("samples : {}", report.samples);
    println!("verdict : {}", report.summary);
    if report.findings.is_empty() {
        return;
    }
    println!();
    for (n, f) in report.findings.iter().enumerate() {
        let marker = if n == 0 { ">>" } else { "  " };
        println!(
            "{marker} [{:<7}] {:<22} conf {:>3.0}%",
            f.dimension.as_str(),
            f.cause.code(),
            f.confidence * 100.0
        );
        println!("     {}", f.summary);
        for e in &f.evidence {
            println!(
                "       - {}: {:.2} {} (threshold {:.2}) — {}",
                e.metric, e.observed, e.unit, e.threshold, e.note
            );
        }
        for o in &f.offenders {
            println!(
                "       * offender: {} (pid {:?}, role {}, {:.0} {})",
                o.name, o.pid, o.role, o.observed, o.unit
            );
        }
        if !f.remediation.is_empty() {
            println!("       -> {}", f.remediation);
        }
        println!();
    }
}

fn exit_code(report: &platform_diagnose::Diagnosis) -> u8 {
    if report.findings.is_empty() {
        return 0;
    }
    // A high-confidence primary cause is treated as "critical".
    if report
        .primary()
        .map(|f| f.confidence >= 0.7)
        .unwrap_or(false)
    {
        2
    } else {
        1
    }
}
