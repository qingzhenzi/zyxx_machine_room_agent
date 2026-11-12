//! Host-dimension rules: hardware/firmware state read from the baseline
//! [`platform_health::Snapshot`] (disk capacity, SMART health, ECC counters).
//!
//! These need the inventory snapshot the caller collected separately via
//! `platform-health`; the live sampler doesn't re-read SMART/ECC.

use super::{Ctx, Rule, confidence};
use crate::finding::{Cause, Evidence, Finding};

/// Disk filling up (capacity), from the baseline snapshot.
pub struct DiskCapacityRule;

impl Rule for DiskCapacityRule {
    fn name(&self) -> &'static str {
        "host.disk_capacity"
    }

    fn evaluate(&self, ctx: &Ctx<'_>) -> Vec<Finding> {
        let Some(baseline) = ctx.baseline else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for disk in &baseline.disks {
            if disk.usage_percent >= 95.0
                || (disk.total_bytes >= 4 << 30 && disk.available_bytes < 1 << 30)
            {
                let crit = disk.usage_percent >= 98.0 || disk.available_bytes < (512 << 20);
                out.push(
                    Finding::new(
                        Cause::DiskFilling,
                        confidence(
                            95.0,
                            disk.usage_percent as f64,
                            1.0,
                            if crit { 0.7 } else { 0.5 },
                        ),
                        format!(
                            "Disk {} nearly full: {:.0}% used ({} free)",
                            disk.mount_point,
                            disk.usage_percent,
                            crate::human_bytes(disk.available_bytes)
                        ),
                    )
                    .with_evidence(Evidence {
                        metric: format!("disk.{}.usage_percent", disk.mount_point),
                        observed: disk.usage_percent as f64,
                        threshold: 95.0,
                        unit: "%".into(),
                        note: "filesystem capacity".into(),
                    })
                    .with_remediation(format!(
                        "Free space on {} or grow the volume; a full root disk breaks services.",
                        disk.mount_point
                    )),
                );
            }
            if disk.read_only {
                out.push(
                    Finding::new(
                        Cause::DiskFailing,
                        0.85,
                        format!(
                            "{} is mounted read-only (kernel hit an IO error)",
                            disk.mount_point
                        ),
                    )
                    .with_evidence(Evidence {
                        metric: format!("disk.{}.read_only", disk.mount_point),
                        observed: 1.0,
                        threshold: 0.0,
                        unit: "bool".into(),
                        note: "remounted ro after an IO error".into(),
                    })
                    .with_remediation(
                        "The filesystem failed; back up and check the underlying disk health.",
                    ),
                );
            }
        }
        out
    }
}

/// Failing disk (SMART) and ECC memory errors, from the baseline snapshot.
pub struct HardwareRule;

impl Rule for HardwareRule {
    fn name(&self) -> &'static str {
        "host.hardware"
    }

    fn evaluate(&self, ctx: &Ctx<'_>) -> Vec<Finding> {
        let Some(baseline) = ctx.baseline else {
            return Vec::new();
        };
        let mut out = Vec::new();

        // SMART failure / heavy wear.
        if let Some(disks) = &baseline.smart {
            for d in disks {
                if d.health.eq_ignore_ascii_case("failed")
                    || d.health.eq_ignore_ascii_case("failing")
                {
                    out.push(
                        Finding::new(
                            Cause::DiskFailing,
                            0.9,
                            format!("SMART reports {} as FAILED", d.device),
                        )
                        .with_evidence(Evidence {
                            metric: format!("smart.{}.health", d.device),
                            observed: 1.0,
                            threshold: 0.0,
                            unit: "verdict".into(),
                            note: "self-assessment failed".into(),
                        })
                        .with_remediation("Replace the disk; failure is imminent. Back up now."),
                    );
                }
                if let Some(wear) = d.percentage_used
                    && wear >= 90.0
                {
                    out.push(
                        Finding::new(
                            Cause::DiskFailing,
                            0.6,
                            format!("SSD {} at {:.0}% endurance used", d.device, wear),
                        )
                        .with_evidence(Evidence {
                            metric: format!("smart.{}.percentage_used", d.device),
                            observed: wear as f64,
                            threshold: 90.0,
                            unit: "%".into(),
                            note: "wear indicator".into(),
                        })
                        .with_remediation("The SSD is near end-of-life; plan a replacement."),
                    );
                }
            }
        }

        // ECC memory errors.
        if let Some(edac) = &baseline.edac {
            if edac.uncorrectable_total > 0 {
                out.push(
                    Finding::new(
                        Cause::MemoryHardwareErrors,
                        0.85,
                        format!(
                            "{} uncorrectable ECC memory errors",
                            edac.uncorrectable_total
                        ),
                    )
                    .with_evidence(Evidence {
                        metric: "edac.uncorrectable_total".into(),
                        observed: edac.uncorrectable_total as f64,
                        threshold: 1.0,
                        unit: "count".into(),
                        note: "data-loss memory errors".into(),
                    })
                    .with_remediation("Failing DIMM; run memtest and replace the faulty module."),
                );
            } else if edac.correctable_total > 1000 {
                out.push(
                    Finding::new(
                        Cause::MemoryHardwareErrors,
                        0.55,
                        format!("{} correctable ECC memory errors", edac.correctable_total),
                    )
                    .with_evidence(Evidence {
                        metric: "edac.correctable_total".into(),
                        observed: edac.correctable_total as f64,
                        threshold: 1000.0,
                        unit: "count".into(),
                        note: "recovered bit flips — early DIMM failure sign".into(),
                    })
                    .with_remediation(
                        "Rising correctable errors precede failure; monitor and plan a swap.",
                    ),
                );
            }
        }

        out
    }
}
