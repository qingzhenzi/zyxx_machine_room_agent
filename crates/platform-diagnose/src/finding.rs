//! Diagnosis output model: what the engine concludes and why.
//!
//! A [`Finding`] is one hypothesis ("memory is thrashing") with the evidence
//! that produced it, the processes to blame, and a remediation hint. The engine
//! ranks findings by confidence; consumers render them by [`Dimension`].

use serde::{Deserialize, Serialize};

/// Which layer of the stack a finding implicates.
///
/// This is the primary way a human triages: start at the dimension the symptom
/// points to, then read the evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dimension {
    /// A specific process/application is responsible (or is the victim).
    App,
    /// OS resource pressure (CPU/memory/IO/swap) independent of one app.
    System,
    /// Underlying hardware or firmware (disk, memory DIMMs, thermals).
    Host,
    /// Connectivity, latency, loss, DNS, or bandwidth.
    Network,
}

impl Dimension {
    pub fn as_str(self) -> &'static str {
        match self {
            Dimension::App => "app",
            Dimension::System => "system",
            Dimension::Host => "host",
            Dimension::Network => "network",
        }
    }
}

impl std::fmt::Display for Dimension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A concrete root cause. Stable identifiers for alert routing / i18n.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    // ---- System ----
    MemoryThrashing,
    MemoryExhausted,
    SwapThrashing,
    CpuSaturated,
    IoBound,
    VmSteal,
    OomActivity,
    ZombieAccumulation,
    // ---- Host ----
    ThermalThrottling,
    DiskFilling,
    DiskFailing,
    MemoryHardwareErrors,
    // ---- Network ----
    HighLatency,
    PacketLoss,
    DnsSlow,
    DnsFailing,
    RemoteUnreachable,
    LocalNetworkErrors,
    TcpRetransmits,
    BandwidthSaturated,
    // ---- App ----
    ProcessCpuHog,
    ProcessMemoryHog,
    ProcessIoHog,
    ProcessBlocked,
    ProcessFdExhaustion,
    ProcessConnectionStorm,
    AppUnresponsive,
    AppRestartLoop,
}

impl Cause {
    /// Stable snake_case code, e.g. `memory.thrashing`.
    pub fn code(&self) -> &'static str {
        match self {
            Cause::MemoryThrashing => "memory.thrashing",
            Cause::MemoryExhausted => "memory.exhausted",
            Cause::SwapThrashing => "swap.thrashing",
            Cause::CpuSaturated => "cpu.saturated",
            Cause::IoBound => "io.bound",
            Cause::VmSteal => "vm.steal",
            Cause::OomActivity => "memory.oom",
            Cause::ZombieAccumulation => "process.zombie",
            Cause::ThermalThrottling => "thermal.throttling",
            Cause::DiskFilling => "disk.filling",
            Cause::DiskFailing => "disk.failing",
            Cause::MemoryHardwareErrors => "memory.hardware_errors",
            Cause::HighLatency => "network.latency",
            Cause::PacketLoss => "network.packet_loss",
            Cause::DnsSlow => "network.dns_slow",
            Cause::DnsFailing => "network.dns_failing",
            Cause::RemoteUnreachable => "network.remote_unreachable",
            Cause::LocalNetworkErrors => "network.local_errors",
            Cause::TcpRetransmits => "network.tcp_retransmits",
            Cause::BandwidthSaturated => "network.bandwidth",
            Cause::ProcessCpuHog => "app.cpu_hog",
            Cause::ProcessMemoryHog => "app.memory_hog",
            Cause::ProcessIoHog => "app.io_hog",
            Cause::ProcessBlocked => "app.blocked",
            Cause::ProcessFdExhaustion => "app.fd_exhaustion",
            Cause::ProcessConnectionStorm => "app.connection_storm",
            Cause::AppUnresponsive => "app.unresponsive",
            Cause::AppRestartLoop => "app.restart_loop",
        }
    }

    fn dimension(&self) -> Dimension {
        match self {
            Cause::MemoryThrashing
            | Cause::MemoryExhausted
            | Cause::SwapThrashing
            | Cause::CpuSaturated
            | Cause::IoBound
            | Cause::VmSteal
            | Cause::OomActivity
            | Cause::ZombieAccumulation => Dimension::System,
            Cause::ThermalThrottling
            | Cause::DiskFilling
            | Cause::DiskFailing
            | Cause::MemoryHardwareErrors => Dimension::Host,
            Cause::HighLatency
            | Cause::PacketLoss
            | Cause::DnsSlow
            | Cause::DnsFailing
            | Cause::RemoteUnreachable
            | Cause::LocalNetworkErrors
            | Cause::TcpRetransmits
            | Cause::BandwidthSaturated => Dimension::Network,
            Cause::ProcessCpuHog
            | Cause::ProcessMemoryHog
            | Cause::ProcessIoHog
            | Cause::ProcessBlocked
            | Cause::ProcessFdExhaustion
            | Cause::ProcessConnectionStorm
            | Cause::AppUnresponsive
            | Cause::AppRestartLoop => Dimension::App,
        }
    }
}

/// A single measured signal that contributed to a finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    /// What was measured, e.g. `mem.available_percent`, `net.rtt_ms`.
    pub metric: String,
    pub observed: f64,
    /// The value that crossed the rule's threshold.
    pub threshold: f64,
    /// `%`, `ms`, `count`, `bytes`, `ratio`, ...
    pub unit: String,
    /// Human note tying this signal to the cause.
    pub note: String,
}

/// A process (or app) implicated as responsible or victim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Offender {
    pub pid: Option<u32>,
    pub name: String,
    /// Why this process is named: `cpu`, `memory`, `io`, `blocked`, `fd`, ...
    pub role: String,
    /// The measured magnitude that implicates it (CPU %, bytes, count...).
    pub observed: f64,
    pub unit: String,
}

/// One ranked hypothesis produced by the engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub cause: Cause,
    pub dimension: Dimension,
    /// 0.0..=1.0. Higher = more certain this is the root cause.
    pub confidence: f32,
    /// One-line summary for logs / a triage dashboard.
    pub summary: String,
    pub evidence: Vec<Evidence>,
    /// Processes to look at first (empty for host/network-only causes).
    pub offenders: Vec<Offender>,
    /// Practical next step.
    pub remediation: String,
}

impl Finding {
    /// Build a finding, deriving `dimension` from the cause.
    pub fn new(cause: Cause, confidence: f32, summary: impl Into<String>) -> Self {
        let dimension = cause.dimension();
        Finding {
            cause,
            dimension,
            confidence: confidence.clamp(0.0, 1.0),
            summary: summary.into(),
            evidence: Vec::new(),
            offenders: Vec::new(),
            remediation: String::new(),
        }
    }

    pub fn with_evidence(mut self, evidence: Evidence) -> Self {
        self.evidence.push(evidence);
        self
    }

    pub fn with_offender(mut self, offender: Offender) -> Self {
        self.offenders.push(offender);
        self
    }

    pub fn with_remediation(mut self, remediation: impl Into<String>) -> Self {
        self.remediation = remediation.into();
        self
    }
}
