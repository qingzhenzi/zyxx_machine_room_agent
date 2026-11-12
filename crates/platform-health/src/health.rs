//! Rule engine that turns raw metrics into a verdict.
//!
//! Collection is cheap and noisy; the verdict is what a fleet dashboard or a
//! support agent actually consumes. Rules are threshold-driven so operators can
//! tune them per environment without touching the collectors.

use serde::{Deserialize, Serialize};

use crate::model::Snapshot;

/// Overall device state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Healthy,
    Degraded,
    Critical,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Healthy => "healthy",
            Status::Degraded => "degraded",
            Status::Critical => "critical",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Status::Healthy => 0,
            Status::Degraded => 1,
            Status::Critical => 2,
        }
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Critical => "critical",
        }
    }

    fn penalty(self) -> u8 {
        match self {
            Severity::Info => 2,
            Severity::Warning => 10,
            Severity::Critical => 30,
        }
    }
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One finding. `code` is stable and meant for i18n / alert routing; `message`
/// is an English fallback for logs and quick inspection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    /// Machine-readable identifier, e.g. `disk.usage`.
    pub code: String,
    pub severity: Severity,
    /// What the finding is about: `cpu`, `memory`, a mount point, a sensor label…
    pub subject: String,
    pub message: String,
    pub observed: f64,
    pub threshold: f64,
    /// `%`, `°C`, `bytes` or `count`.
    pub unit: String,
}

/// Verdict attached to every snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    pub status: Status,
    /// 0..=100. 100 means no findings at all.
    pub score: u8,
    pub issues: Vec<Issue>,
}

impl HealthReport {
    pub fn is_healthy(&self) -> bool {
        self.status == Status::Healthy
    }

    /// Highest severity present, or `None` when clean.
    pub fn worst(&self) -> Option<Severity> {
        self.issues
            .iter()
            .map(|i| i.severity)
            .max_by_key(|s| match s {
                Severity::Info => 0,
                Severity::Warning => 1,
                Severity::Critical => 2,
            })
    }
}

/// Tunable limits. Percentages are 0..=100.
///
/// Deserialization is partial: missing fields fall back to [`Thresholds::default`],
/// so a site can override just `disk_usage_warn` and forget the rest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Thresholds {
    pub cpu_usage_warn: f32,
    pub cpu_usage_crit: f32,
    /// 1-minute load average per logical core.
    pub load_per_core_warn: f64,
    pub load_per_core_crit: f64,
    pub memory_usage_warn: f32,
    pub memory_usage_crit: f32,
    pub swap_usage_warn: f32,
    pub swap_usage_crit: f32,
    pub disk_usage_warn: f32,
    pub disk_usage_crit: f32,
    /// Absolute free-space floors, checked even when the percentage is fine.
    pub disk_free_warn_bytes: u64,
    pub disk_free_crit_bytes: u64,
    pub temperature_warn: f32,
    pub temperature_crit: f32,
    /// Process count above which the device is considered busy.
    pub process_count_warn: usize,
    // ---- Extended domains ----
    /// Battery charge below this while discharging.
    pub battery_low_percent: f32,
    /// Battery health (full/design capacity) below this.
    pub battery_health_warn: f32,
    pub gpu_temperature_warn: f32,
    pub gpu_temperature_crit: f32,
    /// SSD wear (`percentage_used`) above this.
    pub ssd_wear_warn: f32,
    /// Correctable (ECC/EDAC) memory errors above this suggest failing RAM.
    pub edac_correctable_warn: u64,
    /// Wall-clock offset from the time source beyond this (milliseconds).
    pub clock_offset_warn_ms: f64,
    /// Zombie processes above this.
    pub zombie_warn: usize,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            cpu_usage_warn: 85.0,
            cpu_usage_crit: 95.0,
            load_per_core_warn: 1.5,
            load_per_core_crit: 3.0,
            memory_usage_warn: 85.0,
            memory_usage_crit: 95.0,
            swap_usage_warn: 50.0,
            swap_usage_crit: 80.0,
            disk_usage_warn: 85.0,
            disk_usage_crit: 95.0,
            disk_free_warn_bytes: 5 * 1024 * 1024 * 1024,
            disk_free_crit_bytes: 1024 * 1024 * 1024,
            temperature_warn: 80.0,
            temperature_crit: 90.0,
            process_count_warn: 1000,
            battery_low_percent: 15.0,
            battery_health_warn: 60.0,
            gpu_temperature_warn: 85.0,
            gpu_temperature_crit: 95.0,
            ssd_wear_warn: 90.0,
            edac_correctable_warn: 1000,
            clock_offset_warn_ms: 5000.0,
            zombie_warn: 10,
        }
    }
}

/// The subset of a snapshot that rules look at.
///
/// Kept separate from [`Snapshot`] so rules can be tested (and reused for
/// historical replays) without fabricating a whole device.
#[derive(Debug, Clone, Default)]
pub struct HealthInput {
    pub cpu_usage_percent: f32,
    pub load_one: f64,
    pub logical_cores: usize,
    pub memory_usage_percent: f32,
    pub swap_total_bytes: u64,
    pub swap_usage_percent: f32,
    pub disks: Vec<DiskFacts>,
    pub components: Vec<TempFacts>,
    pub process_count: Option<usize>,
    // ---- Extended domains (all optional) ----
    /// Zombie process count, when the process table was collected.
    pub zombie_count: Option<usize>,
    pub battery_charge_percent: Option<f32>,
    pub battery_status: Option<String>,
    pub battery_health_percent: Option<f32>,
    /// (label, temperature) per GPU.
    pub gpu_temperatures: Vec<(String, Option<f32>)>,
    /// (device, health) per SMART disk.
    pub smart_health: Vec<(String, String)>,
    /// (device, wear percent) per SSD.
    pub ssd_wear: Vec<(String, f32)>,
    pub edac_correctable: u64,
    pub edac_uncorrectable: u64,
    pub secure_boot: Option<bool>,
    pub clock_offset_ms: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct DiskFacts {
    pub mount_point: String,
    pub usage_percent: f32,
    pub available_bytes: u64,
    pub total_bytes: u64,
    pub read_only: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TempFacts {
    pub label: String,
    pub temperature_c: Option<f32>,
    pub critical_c: Option<f32>,
}

impl From<&Snapshot> for HealthInput {
    fn from(s: &Snapshot) -> Self {
        HealthInput {
            cpu_usage_percent: s.cpu.usage_percent,
            load_one: s.cpu.load_average.one,
            logical_cores: s.cpu.logical_cores,
            memory_usage_percent: s.memory.usage_percent,
            swap_total_bytes: s.swap.total_bytes,
            swap_usage_percent: s.swap.usage_percent,
            disks: s
                .disks
                .iter()
                .map(|d| DiskFacts {
                    mount_point: d.mount_point.clone(),
                    usage_percent: d.usage_percent,
                    available_bytes: d.available_bytes,
                    total_bytes: d.total_bytes,
                    read_only: d.read_only,
                })
                .collect(),
            components: s
                .components
                .iter()
                .map(|c| TempFacts {
                    label: c.label.clone(),
                    temperature_c: c.temperature_c,
                    critical_c: c.critical_c,
                })
                .collect(),
            process_count: s.processes.as_ref().map(|p| p.total),
            zombie_count: s
                .processes
                .as_ref()
                .map(|p| *p.by_state.get("zombie").unwrap_or(&0)),
            battery_charge_percent: s.battery.as_ref().and_then(|b| b.charge_percent),
            battery_status: s.battery.as_ref().map(|b| b.status.clone()),
            battery_health_percent: s.battery.as_ref().and_then(|b| b.health_percent),
            gpu_temperatures: s
                .gpus
                .as_ref()
                .map(|g| {
                    g.iter()
                        .map(|g| (format!("gpu{}", g.index), g.temperature_c))
                        .collect()
                })
                .unwrap_or_default(),
            smart_health: s
                .smart
                .as_ref()
                .map(|d| {
                    d.iter()
                        .map(|d| (d.device.clone(), d.health.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            ssd_wear: s
                .smart
                .as_ref()
                .map(|d| {
                    d.iter()
                        .filter_map(|d| d.percentage_used.map(|w| (d.device.clone(), w)))
                        .collect()
                })
                .unwrap_or_default(),
            edac_correctable: s.edac.as_ref().map(|e| e.correctable_total).unwrap_or(0),
            edac_uncorrectable: s.edac.as_ref().map(|e| e.uncorrectable_total).unwrap_or(0),
            secure_boot: s.firmware.as_ref().and_then(|f| f.secure_boot),
            clock_offset_ms: s.clock.as_ref().and_then(|c| c.offset_ms),
        }
    }
}

/// Partitions smaller than this are excluded from absolute free-space rules.
const ABSOLUTE_FLOOR_MIN_TOTAL: u64 = 4 * 1024 * 1024 * 1024;

/// Evaluate `input` against `t`.
pub fn evaluate(input: &HealthInput, t: &Thresholds) -> HealthReport {
    let mut issues = Vec::new();

    check_percent(
        &mut issues,
        Limits {
            code: "cpu.usage",
            subject: "cpu",
            label: "CPU usage",
            unit: "%",
            warn: t.cpu_usage_warn,
            crit: t.cpu_usage_crit,
        },
        input.cpu_usage_percent,
    );

    if input.logical_cores > 0 && input.load_one >= 0.0 {
        let per_core = input.load_one / input.logical_cores as f64;
        if per_core >= t.load_per_core_crit {
            issues.push(issue(
                "cpu.load",
                Severity::Critical,
                "cpu",
                format!(
                    "Load average {:.2} is {:.2} per core",
                    input.load_one, per_core
                ),
                per_core,
                t.load_per_core_crit,
                "count",
            ));
        } else if per_core >= t.load_per_core_warn {
            issues.push(issue(
                "cpu.load",
                Severity::Warning,
                "cpu",
                format!(
                    "Load average {:.2} is {:.2} per core",
                    input.load_one, per_core
                ),
                per_core,
                t.load_per_core_warn,
                "count",
            ));
        }
    }

    check_percent(
        &mut issues,
        Limits {
            code: "memory.usage",
            subject: "memory",
            label: "Memory usage",
            unit: "%",
            warn: t.memory_usage_warn,
            crit: t.memory_usage_crit,
        },
        input.memory_usage_percent,
    );

    if input.swap_total_bytes > 0 {
        check_percent(
            &mut issues,
            Limits {
                code: "swap.usage",
                subject: "swap",
                label: "Swap usage",
                unit: "%",
                warn: t.swap_usage_warn,
                crit: t.swap_usage_crit,
            },
            input.swap_usage_percent,
        );
    }

    for disk in &input.disks {
        check_percent(
            &mut issues,
            Limits {
                code: "disk.usage",
                subject: &disk.mount_point,
                label: &format!("Disk {} usage", disk.mount_point),
                unit: "%",
                warn: t.disk_usage_warn,
                crit: t.disk_usage_crit,
            },
            disk.usage_percent,
        );

        // Tiny partitions (EFI, /boot) always look "almost full" in absolute
        // terms; only apply the byte floors to sizes where it means something.
        let floor_applies = disk.total_bytes >= ABSOLUTE_FLOOR_MIN_TOTAL;
        if floor_applies && disk.available_bytes < t.disk_free_crit_bytes {
            issues.push(issue(
                "disk.free",
                Severity::Critical,
                &disk.mount_point,
                format!(
                    "Only {} free on {}",
                    human_bytes(disk.available_bytes),
                    disk.mount_point
                ),
                disk.available_bytes as f64,
                t.disk_free_crit_bytes as f64,
                "bytes",
            ));
        } else if floor_applies && disk.available_bytes < t.disk_free_warn_bytes {
            issues.push(issue(
                "disk.free",
                Severity::Warning,
                &disk.mount_point,
                format!(
                    "Only {} free on {}",
                    human_bytes(disk.available_bytes),
                    disk.mount_point
                ),
                disk.available_bytes as f64,
                t.disk_free_warn_bytes as f64,
                "bytes",
            ));
        }
    }

    for sensor in &input.components {
        let Some(temp) = sensor.temperature_c else {
            continue;
        };
        // Firmware-reported critical point wins when it is more aggressive.
        let crit = sensor
            .critical_c
            .map(|c| c.min(t.temperature_crit))
            .unwrap_or(t.temperature_crit);
        if temp >= crit {
            issues.push(issue(
                "thermal.temperature",
                Severity::Critical,
                &sensor.label,
                format!("{} at {:.0}°C", sensor.label, temp),
                temp as f64,
                crit as f64,
                "°C",
            ));
        } else if temp >= t.temperature_warn {
            issues.push(issue(
                "thermal.temperature",
                Severity::Warning,
                &sensor.label,
                format!("{} at {:.0}°C", sensor.label, temp),
                temp as f64,
                t.temperature_warn as f64,
                "°C",
            ));
        }
    }

    if let Some(count) = input.process_count
        && count > t.process_count_warn
    {
        issues.push(issue(
            "process.count",
            Severity::Warning,
            "processes",
            format!("{count} processes running"),
            count as f64,
            t.process_count_warn as f64,
            "count",
        ));
    }

    // ---- Extended-domain rules ----

    // A filesystem remounted read-only means the kernel hit an I/O error.
    for disk in &input.disks {
        if disk.read_only {
            issues.push(issue(
                "disk.read_only",
                Severity::Critical,
                &disk.mount_point,
                format!("{} is mounted read-only (I/O error?)", disk.mount_point),
                1.0,
                1.0,
                "count",
            ));
        }
    }

    if let (Some(charge), Some(status)) = (
        input.battery_charge_percent,
        input.battery_status.as_deref(),
    ) && status == "discharging"
        && charge <= t.battery_low_percent
    {
        issues.push(issue(
            "battery.low",
            Severity::Warning,
            "battery",
            format!("Battery at {charge:.0}% and discharging"),
            charge as f64,
            t.battery_low_percent as f64,
            "%",
        ));
    }

    if let Some(health) = input.battery_health_percent
        && health <= t.battery_health_warn
    {
        issues.push(issue(
            "battery.health",
            Severity::Warning,
            "battery",
            format!("Battery health at {health:.0}% of design capacity"),
            health as f64,
            t.battery_health_warn as f64,
            "%",
        ));
    }

    for (label, temp) in &input.gpu_temperatures {
        let Some(temp) = temp else { continue };
        if *temp >= t.gpu_temperature_crit {
            issues.push(issue(
                "gpu.temperature",
                Severity::Critical,
                label,
                format!("{label} at {temp:.0}°C"),
                *temp as f64,
                t.gpu_temperature_crit as f64,
                "°C",
            ));
        } else if *temp >= t.gpu_temperature_warn {
            issues.push(issue(
                "gpu.temperature",
                Severity::Warning,
                label,
                format!("{label} at {temp:.0}°C"),
                *temp as f64,
                t.gpu_temperature_warn as f64,
                "°C",
            ));
        }
    }

    for (device, health) in &input.smart_health {
        if health.eq_ignore_ascii_case("failed") || health.eq_ignore_ascii_case("failing") {
            issues.push(issue(
                "disk.smart",
                Severity::Critical,
                device,
                format!("SMART health FAILED on {device}"),
                1.0,
                1.0,
                "count",
            ));
        }
    }

    for (device, wear) in &input.ssd_wear {
        if *wear >= t.ssd_wear_warn {
            issues.push(issue(
                "disk.wear",
                Severity::Warning,
                device,
                format!("SSD {device} at {wear:.0}% endurance used"),
                *wear as f64,
                t.ssd_wear_warn as f64,
                "%",
            ));
        }
    }

    if input.edac_uncorrectable > 0 {
        issues.push(issue(
            "memory.ecc",
            Severity::Critical,
            "memory",
            format!(
                "{} uncorrectable memory errors (data loss)",
                input.edac_uncorrectable
            ),
            input.edac_uncorrectable as f64,
            1.0,
            "count",
        ));
    } else if input.edac_correctable > t.edac_correctable_warn {
        issues.push(issue(
            "memory.ecc",
            Severity::Warning,
            "memory",
            format!(
                "{} correctable memory errors (failing RAM likely)",
                input.edac_correctable
            ),
            input.edac_correctable as f64,
            t.edac_correctable_warn as f64,
            "count",
        ));
    }

    // Compliance posture, not an outage: secure boot disabled is informational.
    if input.secure_boot == Some(false) {
        issues.push(issue(
            "firmware.secure_boot",
            Severity::Info,
            "firmware",
            "Secure Boot is disabled".to_string(),
            0.0,
            1.0,
            "count",
        ));
    }

    if let Some(offset) = input.clock_offset_ms
        && offset.abs() >= t.clock_offset_warn_ms
    {
        issues.push(issue(
            "clock.skew",
            Severity::Warning,
            "clock",
            format!("Clock offset {offset:.0} ms from time source"),
            offset.abs(),
            t.clock_offset_warn_ms,
            "ms",
        ));
    }

    if let Some(zombies) = input.zombie_count
        && zombies > t.zombie_warn
    {
        issues.push(issue(
            "process.zombie",
            Severity::Warning,
            "processes",
            format!("{zombies} zombie processes"),
            zombies as f64,
            t.zombie_warn as f64,
            "count",
        ));
    }

    // A critical finding dominates a warning regardless of insertion order.
    let status = issues
        .iter()
        .map(|i| match i.severity {
            Severity::Critical => Status::Critical,
            Severity::Warning => Status::Degraded,
            Severity::Info => Status::Healthy,
        })
        .max_by_key(|s| s.rank())
        .unwrap_or(Status::Healthy);

    let mut score: i32 = 100;
    for i in &issues {
        score -= i.severity.penalty() as i32;
    }
    // Several disks at 99% should not be able to produce a negative score.
    let score = score.clamp(0, 100) as u8;

    HealthReport {
        status,
        score,
        issues,
    }
}

struct Limits<'a> {
    code: &'a str,
    subject: &'a str,
    label: &'a str,
    unit: &'a str,
    warn: f32,
    crit: f32,
}

fn check_percent(issues: &mut Vec<Issue>, limits: Limits<'_>, observed: f32) {
    let Limits {
        code,
        subject,
        label,
        unit,
        warn,
        crit,
    } = limits;
    if observed >= crit {
        issues.push(issue(
            code,
            Severity::Critical,
            subject,
            format!("{label} at {observed:.0}%"),
            observed as f64,
            crit as f64,
            unit,
        ));
    } else if observed >= warn {
        issues.push(issue(
            code,
            Severity::Warning,
            subject,
            format!("{label} at {observed:.0}%"),
            observed as f64,
            warn as f64,
            unit,
        ));
    }
}

fn issue(
    code: &str,
    severity: Severity,
    subject: &str,
    message: String,
    observed: f64,
    threshold: f64,
    unit: &str,
) -> Issue {
    Issue {
        code: code.to_string(),
        severity,
        subject: subject.to_string(),
        message,
        observed: round2(observed),
        threshold,
        unit: unit.to_string(),
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// 1073741824 -> "1.0 GiB". Used in messages only.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> HealthInput {
        HealthInput {
            cpu_usage_percent: 10.0,
            load_one: 0.5,
            logical_cores: 8,
            memory_usage_percent: 40.0,
            swap_total_bytes: 0,
            swap_usage_percent: 0.0,
            disks: vec![DiskFacts {
                mount_point: "/".into(),
                usage_percent: 30.0,
                available_bytes: 100 * 1024 * 1024 * 1024,
                total_bytes: 500 * 1024 * 1024 * 1024,
                ..Default::default()
            }],
            components: vec![],
            process_count: None,
            ..Default::default()
        }
    }

    #[test]
    fn healthy_device_scores_100() {
        let report = evaluate(&input(), &Thresholds::default());
        assert_eq!(report.status, Status::Healthy);
        assert_eq!(report.score, 100);
        assert!(report.issues.is_empty());
    }

    #[test]
    fn warning_and_critical_escalate() {
        let mut i = input();
        i.memory_usage_percent = 88.0;
        let report = evaluate(&i, &Thresholds::default());
        assert_eq!(report.status, Status::Degraded);
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].code, "memory.usage");
        assert_eq!(report.score, 90);

        i.memory_usage_percent = 99.0;
        let report = evaluate(&i, &Thresholds::default());
        assert_eq!(report.status, Status::Critical);
        assert_eq!(report.score, 70);
    }

    #[test]
    fn absolute_free_space_floor_applies_below_percentage_limit() {
        let mut i = input();
        i.disks[0].usage_percent = 10.0;
        i.disks[0].available_bytes = 512 * 1024 * 1024;
        let report = evaluate(&i, &Thresholds::default());
        assert_eq!(report.status, Status::Critical);
        assert_eq!(report.issues[0].code, "disk.free");
    }

    #[test]
    fn firmware_critical_temp_overrides_default() {
        let mut i = input();
        i.components.push(TempFacts {
            label: "Package id 0".into(),
            temperature_c: Some(72.0),
            critical_c: Some(70.0),
        });
        let report = evaluate(&i, &Thresholds::default());
        assert_eq!(report.status, Status::Critical);
    }

    #[test]
    fn swap_rules_skip_when_unconfigured() {
        let mut i = input();
        i.swap_usage_percent = 100.0;
        i.swap_total_bytes = 0;
        let report = evaluate(&i, &Thresholds::default());
        assert!(report.issues.is_empty());
    }

    #[test]
    fn many_findings_clamp_at_zero() {
        let mut i = input();
        i.disks = (0..6)
            .map(|n| DiskFacts {
                mount_point: format!("/m{n}"),
                usage_percent: 99.0,
                available_bytes: 0,
                total_bytes: 100 * 1024 * 1024 * 1024,
                ..Default::default()
            })
            .collect();
        let report = evaluate(&i, &Thresholds::default());
        assert_eq!(report.status, Status::Critical);
        assert_eq!(report.score, 0);
    }

    #[test]
    fn read_only_disk_is_critical() {
        let mut i = input();
        i.disks[0].read_only = true;
        let report = evaluate(&i, &Thresholds::default());
        assert_eq!(report.status, Status::Critical);
        assert!(report.issues.iter().any(|x| x.code == "disk.read_only"));
    }

    #[test]
    fn low_battery_while_discharging_warns() {
        let mut i = input();
        i.battery_charge_percent = Some(10.0);
        i.battery_status = Some("discharging".into());
        let report = evaluate(&i, &Thresholds::default());
        assert_eq!(report.status, Status::Degraded);
        assert!(report.issues.iter().any(|x| x.code == "battery.low"));
    }

    #[test]
    fn charging_battery_low_is_not_flagged() {
        let mut i = input();
        i.battery_charge_percent = Some(10.0);
        i.battery_status = Some("charging".into());
        let report = evaluate(&i, &Thresholds::default());
        assert!(!report.issues.iter().any(|x| x.code == "battery.low"));
    }

    #[test]
    fn gpu_over_temp_crit() {
        let mut i = input();
        i.gpu_temperatures.push(("gpu0".into(), Some(97.0)));
        let report = evaluate(&i, &Thresholds::default());
        assert_eq!(report.status, Status::Critical);
        assert!(report.issues.iter().any(|x| x.code == "gpu.temperature"));
    }

    #[test]
    fn smart_failure_is_critical() {
        let mut i = input();
        i.smart_health.push(("/dev/sda".into(), "FAILED".into()));
        let report = evaluate(&i, &Thresholds::default());
        assert_eq!(report.status, Status::Critical);
        assert!(report.issues.iter().any(|x| x.code == "disk.smart"));
    }

    #[test]
    fn uncorrectable_ecc_is_critical_but_correctable_only_warns() {
        let mut i = input();
        i.edac_uncorrectable = 1;
        let report = evaluate(&i, &Thresholds::default());
        assert_eq!(report.status, Status::Critical);

        let mut i = input();
        i.edac_uncorrectable = 0;
        i.edac_correctable = 5000;
        let report = evaluate(&i, &Thresholds::default());
        assert_eq!(report.status, Status::Degraded);
        assert!(report.issues.iter().any(|x| x.code == "memory.ecc"));
    }

    #[test]
    fn secure_boot_off_is_informational_not_degrading() {
        let mut i = input();
        i.secure_boot = Some(false);
        let report = evaluate(&i, &Thresholds::default());
        // Info severity does not degrade status or cut the score below 100-band.
        assert_eq!(report.status, Status::Healthy);
        assert!(
            report
                .issues
                .iter()
                .any(|x| x.code == "firmware.secure_boot")
        );
    }

    #[test]
    fn clock_skew_beyond_threshold_warns() {
        let mut i = input();
        i.clock_offset_ms = Some(9000.0);
        let report = evaluate(&i, &Thresholds::default());
        assert_eq!(report.status, Status::Degraded);
        assert!(report.issues.iter().any(|x| x.code == "clock.skew"));
    }

    #[test]
    fn ssd_wear_high_warns() {
        let mut i = input();
        i.ssd_wear.push(("/dev/nvme0n1".into(), 95.0));
        let report = evaluate(&i, &Thresholds::default());
        assert_eq!(report.status, Status::Degraded);
        assert!(report.issues.iter().any(|x| x.code == "disk.wear"));
    }
}
