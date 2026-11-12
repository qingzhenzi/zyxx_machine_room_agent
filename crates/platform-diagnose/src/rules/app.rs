//! App-dimension rules: name the specific process responsible for a bottleneck
//! (CPU/memory/IO hog) or one that is blocked (uninterruptible sleep, usually
//! stuck on IO). These turn "the system is slow" into "X is why".

use super::{Ctx, Rule, confidence};
use crate::finding::{Cause, Evidence, Finding, Offender};

/// A single process monopolising CPU, memory, or IO.
pub struct ProcessHogRule;

impl Rule for ProcessHogRule {
    fn name(&self) -> &'static str {
        "app.hog"
    }

    fn evaluate(&self, ctx: &Ctx<'_>) -> Vec<Finding> {
        let mut out = Vec::new();
        let procs = &ctx.latest().procs;
        let t = ctx.thresholds;

        // CPU hog.
        if let Some(top) = procs.iter().max_by(|a, b| {
            a.cpu_percent
                .partial_cmp(&b.cpu_percent)
                .unwrap_or(std::cmp::Ordering::Equal)
        }) && top.cpu_percent >= t.proc_cpu_hog_percent
        {
            let sustained = ctx.sustained(|s| {
                s.procs
                    .iter()
                    .any(|p| p.name == top.name && p.cpu_percent >= t.proc_cpu_hog_percent)
            });
            out.push(
                Finding::new(
                    Cause::ProcessCpuHog,
                    confidence(
                        t.proc_cpu_hog_percent as f64,
                        top.cpu_percent as f64,
                        sustained,
                        0.5,
                    ),
                    format!("`{}` is using {:.0}% CPU", top.name, top.cpu_percent),
                )
                .with_evidence(Evidence {
                    metric: "proc.cpu_percent".into(),
                    observed: top.cpu_percent as f64,
                    threshold: t.proc_cpu_hog_percent as f64,
                    unit: "%".into(),
                    note: "top CPU consumer".into(),
                })
                .with_offender(hog(top.pid, &top.name, "cpu", top.cpu_percent as f64, "%"))
                .with_remediation(format!(
                    "Profile or throttle `{}`; it is the primary CPU consumer.",
                    top.name
                )),
            );
        }

        // Memory hog (share of TOTAL system RAM — not of the process list, which
        // would make a lone process read 100%).
        let total_ram = ctx.latest().mem.total_bytes;
        if let Some(top) = procs.iter().max_by_key(|p| p.memory_bytes) {
            let share = if total_ram > 0 {
                top.memory_bytes as f64 / total_ram as f64 * 100.0
            } else {
                0.0
            };
            if share >= t.proc_mem_hog_percent as f64 {
                out.push(
                    Finding::new(
                        Cause::ProcessMemoryHog,
                        confidence(t.proc_mem_hog_percent as f64, share, 1.0, 0.45),
                        format!("`{}` holds {:.0}% of process memory", top.name, share),
                    )
                    .with_evidence(Evidence {
                        metric: "proc.memory_share".into(),
                        observed: share,
                        threshold: t.proc_mem_hog_percent as f64,
                        unit: "%".into(),
                        note: "share of total resident process memory".into(),
                    })
                    .with_offender(hog(
                        top.pid,
                        &top.name,
                        "memory",
                        top.memory_bytes as f64,
                        "bytes",
                    ))
                    .with_remediation(format!(
                        "`{}` is the largest memory holder; cap or reduce it.",
                        top.name
                    )),
                );
            }
        }

        // IO hog.
        if let Some(top) = procs.iter().max_by(|a, b| {
            (a.read_bytes_per_sec + a.write_bytes_per_sec)
                .partial_cmp(&(b.read_bytes_per_sec + b.write_bytes_per_sec))
                .unwrap_or(std::cmp::Ordering::Equal)
        }) {
            let io_rate = top.read_bytes_per_sec + top.write_bytes_per_sec;
            if io_rate >= t.proc_io_hog_bytes {
                out.push(
                    Finding::new(
                        Cause::ProcessIoHog,
                        confidence(t.proc_io_hog_bytes, io_rate, 1.0, 0.5),
                        format!(
                            "`{}` is doing {:.1} MB/s of disk IO",
                            top.name,
                            io_rate / 1e6
                        ),
                    )
                    .with_evidence(Evidence {
                        metric: "proc.io_bytes_per_sec".into(),
                        observed: io_rate,
                        threshold: t.proc_io_hog_bytes,
                        unit: "bytes/s".into(),
                        note: "top disk IO consumer".into(),
                    })
                    .with_offender(hog(top.pid, &top.name, "io", io_rate, "bytes/s"))
                    .with_remediation(format!(
                        "`{}` is hammering storage; rate-limit or move its workload.",
                        top.name
                    )),
                );
            }
        }

        out
    }
}

/// Processes stuck in uninterruptible sleep (`D` state) — usually blocked on
/// IO. A handful briefly is normal; sustained D-state points at storage.
pub struct BlockedProcRule;

impl Rule for BlockedProcRule {
    fn name(&self) -> &'static str {
        "app.blocked"
    }

    fn evaluate(&self, ctx: &Ctx<'_>) -> Vec<Finding> {
        let blocked: Vec<_> = ctx
            .latest()
            .procs
            .iter()
            .filter(|p| p.state == "D")
            .collect();
        // Sustained D-state across the window is the real signal.
        let sustained_count = {
            let hist = &ctx.window[..ctx.window.len().saturating_sub(1)];
            if hist.is_empty() {
                blocked.len()
            } else {
                let avg: f64 = hist
                    .iter()
                    .map(|s| s.procs.iter().filter(|p| p.state == "D").count() as f64)
                    .sum::<f64>()
                    / hist.len() as f64;
                avg.round() as usize
            }
        };
        if sustained_count >= 2 {
            let names: Vec<&str> = blocked.iter().take(3).map(|p| p.name.as_str()).collect();
            let mut f = Finding::new(
                Cause::ProcessBlocked,
                0.55,
                format!("{sustained_count} processes stuck in D-state (blocked on IO)"),
            )
            .with_evidence(Evidence {
                metric: "procs.d_state".into(),
                observed: sustained_count as f64,
                threshold: 2.0,
                unit: "count".into(),
                note: "uninterruptible sleep — usually waiting on storage".into(),
            })
            .with_remediation(
                "Sustained D-state usually means the disk is the bottleneck; check IO await.",
            );
            for p in blocked.iter().take(3) {
                f = f.with_offender(hog(p.pid, &p.name, "blocked", 1.0, "bool"));
            }
            let _ = names;
            return vec![f];
        }
        Vec::new()
    }
}

fn hog(pid: u32, name: &str, role: &str, observed: f64, unit: &str) -> Offender {
    Offender {
        pid: Some(pid),
        name: name.to_string(),
        role: role.to_string(),
        observed,
        unit: unit.to_string(),
    }
}
