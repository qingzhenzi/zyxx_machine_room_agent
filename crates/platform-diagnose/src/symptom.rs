//! What the caller reports as wrong.
//!
//! The engine uses the symptom to decide which rules to weight: a `Timeout`
//! leans on network + app-liveness rules, a `Lag` leans on resource rules. A
//! [`Symptom::Generic`] runs everything for a periodic health sweep.

use serde::{Deserialize, Serialize};

/// A resource the caller suspects is the bottleneck.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Cpu,
    Memory,
    Io,
    Network,
}

/// The reported problem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum Symptom {
    /// UI/app feels sluggish (卡顿). Weights CPU/memory/IO/thermal rules.
    Lag,
    /// A request to a host/port/URL timed out (超时). Weights network + liveness.
    Timeout {
        /// `host:port`, a hostname, or a URL. Used for targeted probes.
        target: String,
    },
    /// General network slowness (slow page loads, choppy video).
    SlowNetwork,
    /// A named application stopped responding.
    AppUnresponsive { app: String },
    /// Caller knows some resource is pegged but not which app.
    HighResource { resource: ResourceKind },
    /// A filesystem is full or nearly full.
    DiskFull { mount: String },
    /// No specific symptom — run a full diagnostic sweep.
    #[default]
    Generic,
}

impl Symptom {
    /// Whether this symptom should prioritize network-dimension rules.
    pub fn network_priority(&self) -> bool {
        matches!(self, Symptom::Timeout { .. } | Symptom::SlowNetwork)
    }

    /// Whether this symptom should prioritize resource (CPU/mem/IO) rules.
    pub fn resource_priority(&self) -> bool {
        matches!(self, Symptom::Lag | Symptom::HighResource { .. })
    }

    /// A short human label for the report header.
    pub fn label(&self) -> String {
        match self {
            Symptom::Lag => "lag/stutter".to_string(),
            Symptom::Timeout { target } => format!("timeout reaching {target}"),
            Symptom::SlowNetwork => "slow network".to_string(),
            Symptom::AppUnresponsive { app } => format!("app not responding: {app}"),
            Symptom::HighResource { resource } => format!("high {resource:?} usage"),
            Symptom::DiskFull { mount } => format!("disk full: {mount}"),
            Symptom::Generic => "general health sweep".to_string(),
        }
    }
}
