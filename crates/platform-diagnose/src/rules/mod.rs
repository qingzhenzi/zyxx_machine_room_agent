//! Rule engine core: the [`Rule`] trait, evaluation context, tunable
//! thresholds, and the system-dimension rules (CPU / memory / IO / thermal /
//! VM / zombies).
//!
//! A rule inspects the sample window plus the baseline snapshot and emits zero
//! or more [`Finding`]s. Confidence reflects both magnitude (how far past the
//! threshold) and persistence (was it high across the whole window, not just a
//! single spike), which keeps the engine from crying wolf on one noisy sample.

use crate::finding::{Cause, Evidence, Finding};
use crate::sample::Sample;
use crate::symptom::Symptom;

pub mod app;
pub mod host;
pub mod network;

/// Every built-in rule, in evaluation order. The engine runs them all and
/// ranks the combined findings.
pub fn all_rules() -> Vec<Box<dyn Rule>> {
    vec![
        // System
        Box::new(MemoryRule),
        Box::new(CpuRule),
        Box::new(IoRule),
        Box::new(ThermalRule),
        Box::new(ZombieRule),
        // Host
        Box::new(host::DiskCapacityRule),
        Box::new(host::HardwareRule),
        // Network
        Box::new(network::PassiveNetRule),
        Box::new(network::ProbeRule),
        Box::new(network::DnsRule),
        Box::new(network::NetVsLocalRule),
        // App
        Box::new(app::ProcessHogRule),
        Box::new(app::BlockedProcRule),
    ]
}

/// Everything a rule may inspect.
pub struct Ctx<'a> {
    /// Captures ordered oldest → newest.
    pub window: &'a [Sample],
    /// The reported symptom.
    pub symptom: &'a Symptom,
    /// Baseline inventory snapshot (disks, SMART, firmware...), when taken.
    pub baseline: Option<&'a platform_health::Snapshot>,
    pub thresholds: &'a Thresholds,
}

impl<'a> Ctx<'a> {
    /// Most recent sample.
    pub fn latest(&self) -> &'a Sample {
        // window is never empty (the engine guarantees at least one sample).
        &self.window[self.window.len() - 1]
    }

    /// Fraction of samples in the window (excluding the newest) where
    /// `predicate` holds. Used to distinguish sustained pressure from a spike.
    pub fn sustained(&self, predicate: impl Fn(&Sample) -> bool) -> f32 {
        if self.window.len() <= 1 {
            // No history: treat the single sample as fully "sustained".
            return predicate(self.latest()) as u32 as f32;
        }
        let hist = &self.window[..self.window.len() - 1];
        let hits = hist.iter().filter(|s| predicate(s)).count();
        hits as f32 / hist.len() as f32
    }

    /// Average a value across the whole window.
    pub fn avg(&self, f: impl Fn(&Sample) -> f64) -> f64 {
        if self.window.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.window.iter().map(f).sum();
        sum / self.window.len() as f64
    }
}

/// A single diagnostic rule.
pub trait Rule: Send + Sync {
    /// Stable identifier for the rule.
    fn name(&self) -> &'static str;
    /// Produce findings for the current context (empty if nothing wrong).
    fn evaluate(&self, ctx: &Ctx<'_>) -> Vec<Finding>;
}

/// Tunable detection thresholds. Defaults suit a typical server/desktop; tune
/// per fleet. Deserialization is partial (missing fields keep defaults).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Thresholds {
    // CPU
    pub cpu_saturated_percent: f32,
    pub cpu_saturated_runqueue_factor: f64,
    // Memory
    pub mem_exhausted_avail_percent: f32,
    pub mem_thrash_swap_out_pages: f64,
    pub mem_thrash_major_faults: f64,
    pub swap_thrash_out_pages: f64,
    pub swap_thrash_used_percent: f32,
    // IO
    pub io_iowait_percent: f32,
    pub io_await_ms: f32,
    // VM
    pub vm_steal_percent: f32,
    // Thermal
    pub thermal_temp_c: f32,
    pub thermal_freq_ratio: f64,
    // Process
    pub zombie_count: usize,
    pub proc_cpu_hog_percent: f32,
    pub proc_mem_hog_percent: f32,
    pub proc_io_hog_bytes: f64,
    // Network
    pub net_retransmits: u64,
    pub net_latency_warn_ms: f64,
    pub net_latency_crit_ms: f64,
    pub net_dns_warn_ms: f64,
    pub net_dns_crit_ms: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            cpu_saturated_percent: 90.0,
            cpu_saturated_runqueue_factor: 1.5,
            mem_exhausted_avail_percent: 8.0,
            mem_thrash_swap_out_pages: 100.0,
            mem_thrash_major_faults: 500.0,
            swap_thrash_out_pages: 200.0,
            swap_thrash_used_percent: 20.0,
            io_iowait_percent: 20.0,
            io_await_ms: 50.0,
            vm_steal_percent: 10.0,
            thermal_temp_c: 90.0,
            thermal_freq_ratio: 0.85,
            zombie_count: 20,
            proc_cpu_hog_percent: 80.0,
            proc_mem_hog_percent: 30.0,
            proc_io_hog_bytes: 20_000_000.0,
            net_retransmits: 10,
            net_latency_warn_ms: 100.0,
            net_latency_crit_ms: 300.0,
            net_dns_warn_ms: 200.0,
            net_dns_crit_ms: 1000.0,
        }
    }
}

/// Confidence from magnitude + persistence. `severity_base` is the floor for
/// merely crossing the line; magnitude and persistence push it higher.
fn confidence(threshold: f64, observed: f64, sustained: f32, severity_base: f32) -> f32 {
    if threshold <= 0.0 {
        return severity_base;
    }
    // 0.0 at threshold, +0.3 at 2× threshold, capped.
    let magnitude = ((observed / threshold) - 1.0).clamp(0.0, 1.0) as f32 * 0.3;
    let persist = sustained.clamp(0.0, 1.0) * 0.3;
    (severity_base + magnitude + persist).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// System-dimension rules
// ---------------------------------------------------------------------------

/// Memory pressure: exhaustion, thrashing (swap-out + major faults).
pub struct MemoryRule;

impl Rule for MemoryRule {
    fn name(&self) -> &'static str {
        "system.memory"
    }

    fn evaluate(&self, ctx: &Ctx<'_>) -> Vec<Finding> {
        let mut out = Vec::new();
        let mem = &ctx.latest().mem;
        let t = ctx.thresholds;

        // Thrashing: sustained swap-out and/or major faults (working set > RAM).
        let swapping = mem.swap_out_pages_per_sec >= t.mem_thrash_swap_out_pages;
        let faulting = mem.major_faults_per_sec >= t.mem_thrash_major_faults;
        if swapping || faulting {
            let sustained = ctx.sustained(|s| {
                s.mem.swap_out_pages_per_sec >= t.mem_thrash_swap_out_pages
                    || s.mem.major_faults_per_sec >= t.mem_thrash_major_faults
            });
            let obs = mem.swap_out_pages_per_sec.max(mem.major_faults_per_sec);
            let thr = t.mem_thrash_swap_out_pages.max(t.mem_thrash_major_faults);
            let mut f = Finding::new(
                Cause::MemoryThrashing,
                confidence(thr, obs, sustained, 0.55),
                format!(
                    "Memory thrashing: {:.0} swap-out pages/s, {:.0} major faults/s",
                    mem.swap_out_pages_per_sec, mem.major_faults_per_sec
                ),
            )
            .with_evidence(Evidence {
                metric: "mem.swap_out_pages_per_sec".into(),
                observed: mem.swap_out_pages_per_sec,
                threshold: t.mem_thrash_swap_out_pages,
                unit: "pages".into(),
                note: "memory pressure escaping to swap".into(),
            })
            .with_evidence(Evidence {
                metric: "mem.major_faults_per_sec".into(),
                observed: mem.major_faults_per_sec,
                threshold: t.mem_thrash_major_faults,
                unit: "faults".into(),
                note: "disk-backed page faults (working set exceeds RAM)".into(),
            })
            .with_remediation(
                "Add RAM, reduce the working set, or cap the biggest memory consumer.",
            );
            // Name the biggest memory consumer as the likely cause.
            if let Some(top) = ctx.latest().procs.iter().max_by_key(|p| p.memory_bytes) {
                f = f.with_offender(crate::finding::Offender {
                    pid: Some(top.pid),
                    name: top.name.clone(),
                    role: "memory".into(),
                    observed: top.memory_bytes as f64,
                    unit: "bytes".into(),
                });
            }
            out.push(f);
        }

        // Exhaustion: very little available memory left.
        if mem.available_percent <= t.mem_exhausted_avail_percent {
            let sustained =
                ctx.sustained(|s| s.mem.available_percent <= t.mem_exhausted_avail_percent);
            out.push(
                Finding::new(
                    Cause::MemoryExhausted,
                    confidence(
                        t.mem_exhausted_avail_percent as f64,
                        mem.available_percent as f64,
                        sustained,
                        0.5,
                    ),
                    format!("Only {:.1}% memory available", mem.available_percent),
                )
                .with_evidence(Evidence {
                    metric: "mem.available_percent".into(),
                    observed: mem.available_percent as f64,
                    threshold: t.mem_exhausted_avail_percent as f64,
                    unit: "%".into(),
                    note: "free+reclaimable memory is nearly gone".into(),
                })
                .with_remediation(
                    "Free memory, add RAM, or lower the memory limit on the workload.",
                ),
            );
        }

        // Swap thrashing specifically: swap in use AND actively paging out.
        let swap_used_pct = if mem.swap_total_bytes > 0 {
            mem.swap_used_bytes as f64 / mem.swap_total_bytes as f64 * 100.0
        } else {
            0.0
        };
        if mem.swap_total_bytes > 0
            && swap_used_pct >= t.swap_thrash_used_percent as f64
            && mem.swap_out_pages_per_sec >= t.swap_thrash_out_pages
        {
            out.push(
                Finding::new(
                    Cause::SwapThrashing,
                    0.6,
                    format!(
                        "Swap {swap_used_pct:.0}% used with {:.0} pages/s paging out",
                        mem.swap_out_pages_per_sec
                    ),
                )
                .with_evidence(Evidence {
                    metric: "mem.swap_out_pages_per_sec".into(),
                    observed: mem.swap_out_pages_per_sec,
                    threshold: t.swap_thrash_out_pages,
                    unit: "pages".into(),
                    note: "actively pushing memory to swap".into(),
                })
                .with_remediation("Memory is oversubscribed; add RAM or shrink the workload."),
            );
        }

        out
    }
}

/// CPU saturation and hypervisor steal time.
pub struct CpuRule;

impl Rule for CpuRule {
    fn name(&self) -> &'static str {
        "system.cpu"
    }

    fn evaluate(&self, ctx: &Ctx<'_>) -> Vec<Finding> {
        let mut out = Vec::new();
        let cpu = &ctx.latest().cpu;
        let t = ctx.thresholds;

        let rq_threshold =
            (cpu.logical_cores.max(1) as f64 * t.cpu_saturated_runqueue_factor).max(1.0);
        let saturated =
            cpu.usage_percent >= t.cpu_saturated_percent || cpu.run_queue as f64 >= rq_threshold;
        if saturated {
            let sustained = ctx.sustained(|s| {
                s.cpu.usage_percent >= t.cpu_saturated_percent
                    || s.cpu.run_queue as f64
                        >= (s.cpu.logical_cores.max(1) as f64 * t.cpu_saturated_runqueue_factor)
                            .max(1.0)
            });
            // Distinguish compute-bound (user high) from kernel/syscall-bound.
            let kind = if cpu.user_percent >= cpu.system_percent {
                "user"
            } else {
                "system"
            };
            let mut f = Finding::new(
                Cause::CpuSaturated,
                confidence(
                    t.cpu_saturated_percent as f64,
                    cpu.usage_percent as f64,
                    sustained,
                    0.55,
                ),
                format!(
                    "CPU saturated ({kind}-heavy): {:.0}% used, run queue {}",
                    cpu.usage_percent, cpu.run_queue
                ),
            )
            .with_evidence(Evidence {
                metric: "cpu.usage_percent".into(),
                observed: cpu.usage_percent as f64,
                threshold: t.cpu_saturated_percent as f64,
                unit: "%".into(),
                note: "aggregate CPU utilisation".into(),
            })
            .with_evidence(Evidence {
                metric: "cpu.run_queue".into(),
                observed: cpu.run_queue as f64,
                threshold: rq_threshold,
                unit: "tasks".into(),
                note: "runnable tasks vs core count".into(),
            })
            .with_remediation("Find the busiest process below; scale out or reduce concurrency.");
            if let Some(top) = ctx.latest().procs.iter().max_by(|a, b| {
                a.cpu_percent
                    .partial_cmp(&b.cpu_percent)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }) {
                f = f.with_offender(crate::finding::Offender {
                    pid: Some(top.pid),
                    name: top.name.clone(),
                    role: "cpu".into(),
                    observed: top.cpu_percent as f64,
                    unit: "%".into(),
                });
            }
            out.push(f);
        }

        // VM steal: the host took our CPU.
        if cpu.steal_percent >= t.vm_steal_percent {
            let sustained = ctx.sustained(|s| s.cpu.steal_percent >= t.vm_steal_percent);
            out.push(
                Finding::new(
                    Cause::VmSteal,
                    confidence(
                        t.vm_steal_percent as f64,
                        cpu.steal_percent as f64,
                        sustained,
                        0.5,
                    ),
                    format!(
                        "Hypervisor stealing {:.0}% of CPU (noisy neighbour?)",
                        cpu.steal_percent
                    ),
                )
                .with_evidence(Evidence {
                    metric: "cpu.steal_percent".into(),
                    observed: cpu.steal_percent as f64,
                    threshold: t.vm_steal_percent as f64,
                    unit: "%".into(),
                    note: "CPU time taken by the host/hypervisor".into(),
                })
                .with_remediation("Move to a less contended host or request dedicated cores."),
            );
        }

        out
    }
}

/// IO-bound: high iowait or device await/utilisation.
pub struct IoRule;

impl Rule for IoRule {
    fn name(&self) -> &'static str {
        "system.io"
    }

    fn evaluate(&self, ctx: &Ctx<'_>) -> Vec<Finding> {
        let mut out = Vec::new();
        let io = &ctx.latest().io;
        let cpu = &ctx.latest().cpu;
        let t = ctx.thresholds;

        let high_iowait = cpu.iowait_percent >= t.io_iowait_percent;
        let high_await = io.await_ms >= t.io_await_ms;
        if high_iowait || high_await {
            let sustained = ctx.sustained(|s| {
                s.cpu.iowait_percent >= t.io_iowait_percent || s.io.await_ms >= t.io_await_ms
            });
            let obs = cpu.iowait_percent.max(io.await_ms);
            let thr = t.io_iowait_percent.max(t.io_await_ms);
            let mut f = Finding::new(
                Cause::IoBound,
                confidence(thr as f64, obs as f64, sustained, 0.5),
                format!(
                    "IO-bound: {:.0}% iowait, device await {:.0} ms",
                    cpu.iowait_percent, io.await_ms
                ),
            )
            .with_evidence(Evidence {
                metric: "cpu.iowait_percent".into(),
                observed: cpu.iowait_percent as f64,
                threshold: t.io_iowait_percent as f64,
                unit: "%".into(),
                note: "CPU idle waiting on block devices".into(),
            })
            .with_remediation(
                "Profile the busiest disk writer/reader below; consider faster storage.",
            );
            // Attribute to the top IO consumer.
            if let Some(top) = ctx.latest().procs.iter().max_by(|a, b| {
                (a.read_bytes_per_sec + a.write_bytes_per_sec)
                    .partial_cmp(&(b.read_bytes_per_sec + b.write_bytes_per_sec))
                    .unwrap_or(std::cmp::Ordering::Equal)
            }) {
                let io_rate = top.read_bytes_per_sec + top.write_bytes_per_sec;
                if io_rate > 0.0 {
                    f = f.with_offender(crate::finding::Offender {
                        pid: Some(top.pid),
                        name: top.name.clone(),
                        role: "io".into(),
                        observed: io_rate,
                        unit: "bytes/s".into(),
                    });
                }
            }
            out.push(f);
        }

        out
    }
}

/// Thermal throttling: hot AND frequency capped (or the throttle flag trips).
pub struct ThermalRule;

impl Rule for ThermalRule {
    fn name(&self) -> &'static str {
        "host.thermal"
    }

    fn evaluate(&self, ctx: &Ctx<'_>) -> Vec<Finding> {
        let th = &ctx.latest().thermal;
        let t = ctx.thresholds;
        let hot = th
            .max_temp_c
            .map(|c| c >= t.thermal_temp_c)
            .unwrap_or(false);
        let freq_capped = match (th.freq_mhz, th.max_freq_mhz) {
            (Some(cur), Some(max)) if max > 0.0 => cur < max * t.thermal_freq_ratio,
            _ => false,
        };
        if th.throttled || (hot && freq_capped) {
            let sustained = ctx.sustained(|s| s.thermal.throttled);
            let temp = th.max_temp_c.unwrap_or(0.0) as f64;
            let label = th.hottest_label.clone().unwrap_or_else(|| "cpu".into());
            return vec![Finding::new(
                Cause::ThermalThrottling,
                confidence(t.thermal_temp_c as f64, temp, sustained, 0.6),
                format!(
                    "Thermal throttling: {label} at {:.0}°C, frequency capped",
                    temp
                ),
            )
            .with_evidence(Evidence {
                metric: "thermal.max_temp_c".into(),
                observed: temp,
                threshold: t.thermal_temp_c as f64,
                unit: "°C".into(),
                note: "hottest sensor".into(),
            })
            .with_remediation(
                "Check cooling/dust/thermal paste; sustained throttling implies a heat problem.",
            )];
        }
        Vec::new()
    }
}

/// Zombie process accumulation.
pub struct ZombieRule;

impl Rule for ZombieRule {
    fn name(&self) -> &'static str {
        "system.zombie"
    }

    fn evaluate(&self, ctx: &Ctx<'_>) -> Vec<Finding> {
        let zombies = ctx.latest().procs.iter().filter(|p| p.state == "Z").count();
        let t = ctx.thresholds;
        if zombies > t.zombie_count {
            let sustained = ctx
                .sustained(|s| s.procs.iter().filter(|p| p.state == "Z").count() > t.zombie_count);
            return vec![
                Finding::new(
                    Cause::ZombieAccumulation,
                    confidence(t.zombie_count as f64, zombies as f64, sustained, 0.4),
                    format!("{zombies} zombie processes (a parent isn't reaping)"),
                )
                .with_evidence(Evidence {
                    metric: "procs.zombies".into(),
                    observed: zombies as f64,
                    threshold: t.zombie_count as f64,
                    unit: "count".into(),
                    note: "terminated but unreaped processes".into(),
                })
                .with_remediation("Find the parent that isn't calling wait(); restart it."),
            ];
        }
        Vec::new()
    }
}
