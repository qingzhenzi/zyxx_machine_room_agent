//! `platform-health` — collect device facts and turn them into a health verdict.
//!
//! ```no_run
//! let snap = platform_health::snapshot();
//! println!("{} — score {}", snap.health.status, snap.health.score);
//! ```
//!
//! Design notes:
//!
//! * **Collection never fails.** Missing sensors, permissions or kernel
//!   features degrade to empty/`None` fields; a caller always gets a snapshot.
//!   [`Snapshot::sections`] records *why* each optional domain is absent.
//! * **Only permissive dependencies.** `sysinfo` (MIT) + `serde` by default;
//!   no GPL/AGPL code is linked, so the crate can be embedded in a closed-source
//!   binary. Optional features (GPU, SMART, containers) add only MIT/Apache-2.0
//!   crates or shell out to separately-licensed tools.
//! * **Stable JSON.** [`SCHEMA_VERSION`] identifies the shape; consumers should
//!   ignore unknown fields and branch on `code` in [`health::Issue`].

pub mod collect;
pub mod error;
pub mod health;
pub mod model;
pub mod privacy;
pub mod section;
pub mod time;

pub use health::{HealthInput, HealthReport, Issue, Severity, Status, Thresholds};
pub use model::Snapshot;
pub use privacy::Privacy;
pub use section::SectionMeta;

/// Schema identifier emitted in [`Snapshot::schema`].
pub const SCHEMA_VERSION: &str = "platform-health/v2";

/// Collection knobs.
#[derive(Debug, Clone)]
pub struct Options {
    /// Whether to mask identifying data. Defaults to [`Privacy::Full`].
    pub privacy: Privacy,
    /// Collect the process table. Slower on busy machines, off by default.
    pub include_processes: bool,
    /// How many processes to keep in [`model::Processes::top`].
    pub top_processes: usize,
    /// Collect the optional inventory domains (displays, battery, firmware,
    /// Wi-Fi, GPU, SMART, containers, ...). Set `false` for a fast core-only
    /// snapshot (CPU/memory/disk/network/thermal). Defaults to `true`.
    pub include_optional: bool,
    /// Limits used by the verdict. See [`Thresholds`].
    pub thresholds: Thresholds,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            privacy: Privacy::Full,
            include_processes: false,
            top_processes: 10,
            include_optional: true,
            thresholds: Thresholds::default(),
        }
    }
}

/// Collect a snapshot with default [`Options`].
pub fn snapshot() -> Snapshot {
    snapshot_with(&Options::default())
}

/// Collect a snapshot with explicit [`Options`].
pub fn snapshot_with(opts: &Options) -> Snapshot {
    let mut snap = collect::collect(opts);
    snap.health = health::evaluate(&HealthInput::from(&snap), &opts.thresholds);
    snap
}

/// Collect and serialize in one step.
pub fn snapshot_json(opts: &Options) -> error::Result<String> {
    Ok(serde_json::to_string(&snapshot_with(opts))?)
}
