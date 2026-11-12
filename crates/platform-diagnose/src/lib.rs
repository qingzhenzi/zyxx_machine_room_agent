//! `platform-diagnose` — rule-based device fault diagnosis.
//!
//! Where [`platform_health`] answers *"what is this device and is it healthy"*,
//! this crate answers *"why is it misbehaving"*. It samples live signals (CPU,
//! memory, IO, thermals, network, per-process) over a sliding window, runs a
//! deterministic rule engine, and ranks root-cause hypotheses across four
//! dimensions — **app / system / host / network** — with evidence, the offending
//! processes, and a remediation hint.
//!
//! ```no_run
//! use platform_diagnose::{Diagnoser, Options, Symptom};
//! use std::time::Duration;
//!
//! let mut d = Diagnoser::new(Options::default());
//! let report = d.sweep(&Symptom::Lag, Duration::from_secs(10), Duration::from_secs(1));
//! println!("{}", report.summary);
//! for f in &report.findings {
//!     println!("  [{}] {} ({:.0}%)", f.dimension, f.summary, f.confidence * 100.0);
//! }
//! ```
//!
//! Design notes:
//!
//! * **Deterministic, not ML.** Rules + confidence scoring are explainable and
//!   work offline with no training data. Every finding carries the evidence that
//!   produced it.
//! * **Trend, not snapshots.** Persistence across the sample window separates a
//!   transient spike from sustained pressure, so the engine doesn't cry wolf.
//! * **Never fails.** Missing `/proc`, absent sensors, or a missing baseline all
//!   degrade to "no finding" — a diagnosis always comes back.

pub mod engine;
pub mod finding;
pub mod rules;
pub mod sample;
pub mod sampler;
pub mod symptom;

pub use engine::{Diagnoser, Diagnosis, Options};
pub use finding::{Cause, Dimension, Evidence, Finding, Offender};
pub use rules::Thresholds;
pub use sample::Sample;
pub use sampler::{ProbeConfig, Sampler};
pub use symptom::{ResourceKind, Symptom};

/// Format a byte count for human-readable messages (e.g. "1.0 GiB").
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
