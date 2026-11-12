//! Runtime signal model: one point-in-time capture of live system state.
//!
//! Unlike the point-in-time [`platform_health`] snapshot (inventory + a single
//! metric reading), a [`Sample`] is built for **trend** analysis: cumulative
//! counters are already reduced to per-second rates, so rules can reason about
//! "is the system swapping hard right now" rather than "swap is 40% used".
//!
//! The engine keeps a window of samples; a single sample is a snapshot of the
//! *dynamics*, not just the levels.

use serde::{Deserialize, Serialize};

/// One capture. Produced by [`crate::sampler::Sampler`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sample {
    /// Unix seconds.
    pub at_unix: u64,
    /// Milliseconds the sampler spent capturing this sample.
    pub capture_ms: u64,
    pub cpu: CpuSample,
    pub mem: MemSample,
    pub io: IoSample,
    pub net: NetSample,
    pub thermal: ThrottleSample,
    /// Snapshot of processes, sorted by the sampler (top consumers first).
    pub procs: Vec<ProcSample>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CpuSample {
    /// Aggregate usage 0..=100 across all cores.
    pub usage_percent: f32,
    /// Split of the last interval, 0..=100 each (they sum to ~100).
    pub user_percent: f32,
    pub system_percent: f32,
    /// iowait — the CPU was idle waiting on IO. High = IO pressure.
    pub iowait_percent: f32,
    /// Steal time — the hypervisor took our slice. High = VM host contention.
    pub steal_percent: f32,
    pub idle_percent: f32,
    pub logical_cores: usize,
    /// 1-minute load average.
    pub load_one: f64,
    /// Tasks on the run queue (runnable + running). > cores = oversubscribed.
    pub run_queue: u64,
    /// Context switches per second.
    pub ctx_switches_per_sec: f64,
    /// Interrupts per second (softirq included).
    pub interrupts_per_sec: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemSample {
    pub total_bytes: u64,
    pub available_bytes: u64,
    /// 0..=100.
    pub available_percent: f32,
    pub free_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
    /// Swap pages swapped OUT per second (memory pressure escaping to disk).
    pub swap_out_pages_per_sec: f64,
    pub swap_in_pages_per_sec: f64,
    /// Major (disk-backed) page faults per second. High = working set > RAM.
    pub major_faults_per_sec: f64,
    /// Total RAM in use for buffers/cache (not "used" in the alarming sense).
    pub cached_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IoSample {
    pub disks: Vec<DiskIoSample>,
    /// Aggregate device utilization 0..=100 across disks.
    pub utilization_percent: f32,
    /// Aggregate average await (service time) in ms across busy disks.
    pub await_ms: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiskIoSample {
    pub device: String,
    pub read_bytes_per_sec: f64,
    pub write_bytes_per_sec: f64,
    /// Average time per IO in milliseconds.
    pub await_ms: f32,
    /// Device utilization 0..=100 (portion of time the device was busy).
    pub util_percent: f32,
    /// Requests queued awaiting service.
    pub queue_depth: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetSample {
    /// Passive: interface error/drop counters, summed across interfaces.
    pub rx_errors: u64,
    pub tx_errors: u64,
    pub rx_dropped: u64,
    pub tx_dropped: u64,
    /// Passive: TCP segments retransmitted since boot (delta per window).
    pub tcp_retransmits: u64,
    /// Passive: socket-state counts (e.g. `time_wait`, `estab`, `syn_sent`).
    pub sockets: std::collections::BTreeMap<String, u64>,
    /// Active probes (only run when enabled): RTT to well-known targets.
    pub probes: Vec<ProbeResult>,
    /// Active: DNS resolution timing for a reference name.
    pub dns: Option<DnsProbe>,
}

/// Result of one connectivity probe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    /// `gateway`, `internet`, or the caller's `target`.
    pub label: String,
    pub target: String,
    /// Round-trip time in milliseconds. `None` if unreachable.
    pub rtt_ms: Option<f64>,
    /// Whether the TCP connect succeeded.
    pub reachable: bool,
}

/// DNS resolution timing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsProbe {
    pub name: String,
    /// Resolution time in milliseconds.
    pub elapsed_ms: f64,
    pub resolved: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThrottleSample {
    /// Hottest sensor in °C.
    pub max_temp_c: Option<f32>,
    /// Hottest sensor's label.
    pub hottest_label: Option<String>,
    /// Whether the CPU is currently frequency-limited by thermals.
    pub throttled: bool,
    /// Current CPU frequency in MHz (averaged across cores).
    pub freq_mhz: Option<f64>,
    /// The maximum (non-turbo/base) frequency in MHz, if known.
    pub max_freq_mhz: Option<f64>,
}

/// Per-process snapshot. Counters are already reduced to rates/levels.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProcSample {
    pub pid: u32,
    pub name: String,
    /// Single-letter Linux state: `R` run, `S` sleep, `D` uninterruptible
    /// (usually blocked on IO), `Z` zombie, `T` stopped.
    pub state: String,
    /// CPU usage 0..=100 for this process over the last interval.
    pub cpu_percent: f32,
    /// Resident set size in bytes.
    pub memory_bytes: u64,
    /// Bytes read/written per second over the last interval.
    pub read_bytes_per_sec: f64,
    pub write_bytes_per_sec: f64,
    /// Number of threads.
    pub threads: u32,
    /// Open file descriptors, when readable.
    pub fds: Option<u32>,
    /// Established TCP connections, when readable.
    pub connections: Option<u32>,
    /// Owning username, when resolvable.
    pub user: Option<String>,
}
