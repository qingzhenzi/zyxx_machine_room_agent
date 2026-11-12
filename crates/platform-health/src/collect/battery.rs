//! Battery state from `/sys/class/power_supply`.
//!
//! Linux exposes battery attributes as small sysfs files under each
//! power-supply entry. A desktop with no battery is a normal state, not a
//! failure: it reports `present: false`. Only non-Linux platforms are
//! unsupported (WinAPI/macOS IOKit wiring is not implemented yet).

use crate::Options;
use crate::model::Battery;
use crate::section::Collected;

#[cfg(target_os = "linux")]
const POWER_SUPPLY_DIR: &str = "/sys/class/power_supply";

#[allow(unused_variables)]
pub(super) fn collect(opts: &Options) -> Collected<Battery> {
    #[cfg(target_os = "linux")]
    {
        match first_battery_dir() {
            Some(dir) => Collected::ok(read_battery(&dir)),
            // No battery-type supply exists — a valid desktop, not an error.
            None => Collected::ok(Battery {
                charge_percent: None,
                status: "unknown".to_string(),
                health_percent: None,
                energy_full_wh: None,
                energy_now_wh: None,
                voltage_v: None,
                cycle_count: None,
                technology: None,
                present: false,
            }),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        Collected::unsupported("battery needs WinAPI/sysfs; Linux-only for now")
    }
}

/// First power-supply entry whose `type` file says `Battery` (usually `BAT0`).
#[cfg(target_os = "linux")]
fn first_battery_dir() -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(POWER_SUPPLY_DIR).ok()?.flatten() {
        let path = entry.path();
        if read_trimmed(path.join("type")).as_deref() == Some("Battery") {
            return Some(path);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn read_battery(dir: &std::path::Path) -> Battery {
    let energy_full = read_num(dir, "energy_full");
    let energy_full_wh = energy_full.map(|v| v / 1_000_000.0);

    let energy_design = read_num(dir, "energy_full_design");
    let mut health_percent = match (energy_full, energy_design) {
        (Some(full), Some(design)) if design > 0.0 => Some(full / design * 100.0),
        _ => None,
    };

    // Some firmwares only expose charge_* (microamp-hours); health still works.
    if health_percent.is_none() {
        health_percent = match (
            read_num(dir, "charge_full"),
            read_num(dir, "charge_full_design"),
        ) {
            (Some(full), Some(design)) if design > 0.0 => Some(full / design * 100.0),
            _ => None,
        };
    }

    Battery {
        charge_percent: read_num(dir, "capacity"),
        status: read_trimmed(dir.join("status"))
            .map(|s| s.to_ascii_lowercase().replace(' ', "_"))
            .unwrap_or_else(|| "unknown".to_string()),
        health_percent,
        energy_full_wh,
        energy_now_wh: read_num(dir, "energy_now").map(|v| v / 1_000_000.0),
        voltage_v: read_num(dir, "voltage_now").map(|v| v / 1_000_000.0),
        cycle_count: read_trimmed(dir.join("cycle_count")).and_then(|s| s.parse::<u32>().ok()),
        technology: read_trimmed(dir.join("technology")),
        present: true,
    }
}

/// Read a sysfs attribute and parse it as `f32`. Unreadable/malformed -> None.
#[cfg(target_os = "linux")]
fn read_num(dir: &std::path::Path, file: &str) -> Option<f32> {
    read_trimmed(dir.join(file))?.parse::<f32>().ok()
}

#[cfg(target_os = "linux")]
fn read_trimmed(path: impl AsRef<std::path::Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
