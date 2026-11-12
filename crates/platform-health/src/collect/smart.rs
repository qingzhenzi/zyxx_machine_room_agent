//! SMART health collector — shells out to `smartctl` (smartmontools) and
//! parses the ATA/NVMe attribute table for temperature, power-on hours,
//! wear and reallocated sectors.
//!
//! Degrades per device: a disk that denies raw access still appears in the
//! list with `supported = false` and an explanatory `error`.
use crate::Options;
use crate::model::SmartDisk;
use crate::section::Collected;

#[cfg(not(target_os = "linux"))]
pub(super) fn collect(_opts: &Options) -> Collected<Vec<SmartDisk>> {
    Collected::unsupported("smart via smartctl is Linux-only")
}

#[cfg(target_os = "linux")]
pub(super) fn collect(_opts: &Options) -> Collected<Vec<SmartDisk>> {
    // smartctl must at least be present; a missing binary means the whole
    // domain is unavailable (there is no fallback implementation).
    if std::process::Command::new("smartctl")
        .arg("--version")
        .output()
        .is_err()
    {
        return Collected::missing_tool("smartctl not installed");
    }

    // Physical disks only (skip partitions, loop, rom, ...).
    let lsblk = match std::process::Command::new("lsblk")
        .args(["-dn", "-o", "NAME,TYPE"])
        .output()
    {
        Ok(out) => out,
        Err(_) => return Collected::missing_tool("lsblk unavailable"),
    };
    // lsblk failed outright (e.g. stripped-down environment): treat like absent.
    if !lsblk.status.success() && lsblk.stdout.is_empty() {
        return Collected::missing_tool("lsblk unavailable");
    }
    let listing = String::from_utf8_lossy(&lsblk.stdout);
    let devices: Vec<String> = listing
        .lines()
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let name = cols.next()?;
            let kind = cols.next()?;
            if kind == "disk" && !name.is_empty() {
                Some(name.to_string())
            } else {
                None
            }
        })
        .collect();

    let disks: Vec<SmartDisk> = devices
        .iter()
        .map(|name| probe_device(&format!("/dev/{name}")))
        .collect();
    Collected::ok(disks)
}

/// Run `smartctl -H -A -i` against one device and parse whatever it reports.
#[cfg(target_os = "linux")]
fn probe_device(dev: &str) -> SmartDisk {
    let mut disk = SmartDisk {
        device: dev.to_string(),
        health: "UNKNOWN".to_string(),
        temperature_c: None,
        power_on_hours: None,
        percentage_used: None,
        reallocated_sectors: None,
        supported: false,
        error: None,
    };

    // smartctl uses a bit-coded exit status (permission, media error, ...),
    // so a non-zero exit is normal and the output is still parsed.
    let out = match std::process::Command::new("smartctl")
        .args(["-H", "-A", "-i", dev])
        .output()
    {
        Ok(out) => out,
        Err(e) => {
            disk.error = Some(format!("smartctl failed to run: {e}"));
            return disk;
        }
    };

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let text = format!("{stdout}{stderr}");

    // Overall health verdict, e.g. "SMART overall-health self-assessment
    // test result: PASSED" (ATA) / "status: PASSED" style on some NVMe.
    for line in text.lines() {
        if let Some((_, rest)) =
            line.split_once("SMART overall-health self-assessment test result:")
        {
            let verdict = rest.trim();
            if !verdict.is_empty() {
                disk.health = first_word(verdict).to_string();
                disk.supported = true;
            }
            break;
        }
    }

    // Devices without SMART report it as unavailable rather than an error.
    if text.contains("SMART support is: Unavailable")
        || text.contains("Device does not support SMART")
        || text.contains("SMART support is: Not Available")
    {
        disk.supported = false;
        disk.health = "UNKNOWN".to_string();
    } else if disk.health != "UNKNOWN" {
        disk.supported = true;
    }

    // NVMe wear indicator: "Percentage Used Endurance Indicator: 3%".
    for line in text.lines() {
        if line.contains("Percentage Used Endurance Indicator") {
            if let Some((_, value)) = line.split_once(':') {
                let cleaned = value.trim().trim_end_matches('%').trim();
                if let Ok(v) = cleaned.parse::<f32>() {
                    disk.percentage_used = Some(v);
                }
            }
            break;
        }
    }

    // ATA attribute table (and vendor SSD extras).
    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 10 {
            continue;
        }
        // Attr lines start with a numeric ID then an underscored name.
        if !tokens[0].bytes().all(|b| b.is_ascii_digit()) || !tokens[1].bytes().any(|b| b == b'_') {
            continue;
        }
        let name = tokens[1];
        // Raw value is the 10th token; strip any trailing "(...)" detail.
        let Some(raw) = leading_number(tokens[9]) else {
            continue;
        };

        match name {
            "Temperature_Celsius" | "Airflow_Temperature_Cel" => {
                if disk.temperature_c.is_none() {
                    disk.temperature_c = Some(raw as f32);
                }
            }
            "Power_On_Hours" => {
                if disk.power_on_hours.is_none() {
                    disk.power_on_hours = Some(raw as u64);
                }
            }
            "Reallocated_Sector_Ct" => {
                disk.reallocated_sectors = Some(raw as u64);
            }
            "Reallocated_Event_Count" => {
                if disk.reallocated_sectors.is_none() {
                    disk.reallocated_sectors = Some(raw as u64);
                }
            }
            "SSD_Life_Left" => {
                disk.percentage_used = Some((100.0 - raw).clamp(0.0, 100.0) as f32);
            }
            _ => {}
        }
    }

    // No verdict and no attributes: surface *why* (permission is common).
    if !disk.supported && disk.health == "UNKNOWN" {
        let lower = text.to_lowercase();
        if lower.contains("permission denied") || lower.contains("operation not permitted") {
            disk.error = Some(format!("permission denied reading {dev}"));
        } else if out.status.success() {
            disk.error = Some("smartctl reported no SMART data".to_string());
        } else {
            disk.error = Some(
                first_non_empty_line(&stderr)
                    .unwrap_or("smartctl returned no SMART data")
                    .to_string(),
            );
        }
    }

    disk
}

/// First whitespace-delimited token of a trimmed string.
#[cfg(target_os = "linux")]
fn first_word(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or(s)
}

/// First non-empty line of `text`, if any.
#[cfg(target_os = "linux")]
fn first_non_empty_line(text: &str) -> Option<&str> {
    text.lines().find(|l| !l.trim().is_empty())
}

/// Parse the leading number of an attribute raw-value token, e.g. `38` from
/// `38 (38 Min/Max 15/45)`, `0` from `0`, or hex like `0x0000000A`.
#[cfg(target_os = "linux")]
fn leading_number(tok: &str) -> Option<f64> {
    let t = tok.trim_start_matches('(');
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        let digits: String = hex.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        if !digits.is_empty() {
            return i64::from_str_radix(&digits, 16).ok().map(|v| v as f64);
        }
        return None;
    }
    let digits: String = t
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<f64>().ok()
}
