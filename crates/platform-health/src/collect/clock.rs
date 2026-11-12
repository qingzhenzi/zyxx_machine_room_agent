//! clock collector — synchronisation state and offset from the time source.
//!
//! Sources are tried in order of increasing detail and the first that runs
//! wins: `chronyc tracking` (offset + source + sync), then systemd's
//! `timedatectl` (sync flag only), then Windows `w32tm`.

use crate::Options;
use crate::model::ClockInfo;
use crate::section::Collected;

#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::process::Command;

pub(super) fn collect(_opts: &Options) -> Collected<ClockInfo> {
    platform_collect()
}

#[cfg(target_os = "linux")]
fn platform_collect() -> Collected<ClockInfo> {
    if let Some(info) = from_chronyc() {
        return Collected::ok(info);
    }
    if let Some(info) = from_timedatectl() {
        return Collected::ok(info);
    }
    Collected::missing_tool("no chrony/timedatectl/w32tm available")
}

#[cfg(target_os = "windows")]
fn platform_collect() -> Collected<ClockInfo> {
    if let Some(info) = from_w32tm() {
        return Collected::ok(info);
    }
    Collected::missing_tool("no chrony/timedatectl/w32tm available")
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn platform_collect() -> Collected<ClockInfo> {
    Collected::missing_tool("no chrony/timedatectl/w32tm available")
}

/// `chronyc tracking` — offset, reference source and leap/sync status.
#[cfg(target_os = "linux")]
fn from_chronyc() -> Option<ClockInfo> {
    let out = Command::new("chronyc").arg("tracking").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut offset_ms: Option<f64> = None;
    let mut synchronised: Option<bool> = None;
    let mut source: Option<String> = None;

    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("System time") {
            // `System time : 0.000123456 seconds fast of NTP time`
            // "fast" = our clock is ahead (positive offset), "slow" = behind.
            let value_part = rest.split(':').nth(1).unwrap_or("");
            if let Some(tok) = value_part.split_whitespace().next()
                && let Ok(secs) = tok.parse::<f64>()
            {
                let sign = if rest.contains("slow") { -1.0 } else { 1.0 };
                offset_ms = Some(secs * 1000.0 * sign);
            }
        } else if let Some(rest) = line.strip_prefix("Reference ID") {
            // `Reference ID    : ABCD (time.cloudflare.com)`
            let value = rest.trim().trim_start_matches(':').trim();
            source = Some(if let Some(open) = value.find('(') {
                value[open + 1..].trim().trim_end_matches(')').to_string()
            } else {
                value.split_whitespace().next().unwrap_or("").to_string()
            });
        } else if let Some(rest) = line.strip_prefix("Leap status") {
            // `Leap status : Normal` vs `Leap status : Not synchronised`
            let value = rest.trim().trim_start_matches(':').trim();
            let lower = value.to_ascii_lowercase();
            if lower.contains("not synchron") {
                synchronised = Some(false);
            } else if lower.contains("normal") {
                synchronised = Some(true);
            }
        }
    }

    Some(ClockInfo {
        offset_ms,
        synchronised,
        source,
        method: "chrony".to_string(),
    })
}

/// systemd's `timedatectl` — sync flag only, no offset or source.
#[cfg(target_os = "linux")]
fn from_timedatectl() -> Option<ClockInfo> {
    // Preferred: the precise one-value form.
    if let Ok(out) = Command::new("timedatectl")
        .args(["show", "-p", "NTPSynchronized", "--value"])
        .output()
        && out.status.success()
    {
        let value = String::from_utf8_lossy(&out.stdout);
        return Some(ClockInfo {
            offset_ms: None,
            synchronised: Some(value.trim().eq_ignore_ascii_case("yes")),
            source: None,
            method: "timedatectl".to_string(),
        });
    }
    // Fallback: parse `NTP synchronized: yes` from the human-readable form.
    let out = Command::new("timedatectl").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("NTP synchronized") {
            let value = rest.trim().trim_start_matches(':').trim();
            return Some(ClockInfo {
                offset_ms: None,
                synchronised: Some(value.eq_ignore_ascii_case("yes")),
                source: None,
                method: "timedatectl".to_string(),
            });
        }
    }
    None
}

/// Windows `w32tm /query /status` — source and leap indicator.
#[cfg(target_os = "windows")]
fn from_w32tm() -> Option<ClockInfo> {
    let out = Command::new("w32tm")
        .args(["/query", "/status"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut source: Option<String> = None;
    let mut synchronised: Option<bool> = None;

    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Source:") {
            let value = rest.trim();
            if !value.is_empty() {
                source = Some(value.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("Leap Indicator:") {
            // `Leap Indicator: 0(NoWarning)` — anything else means unsynced.
            let value = rest.trim();
            synchronised = Some(value.starts_with('0') || value.contains("NoWarning"));
        }
    }

    Some(ClockInfo {
        offset_ms: None,
        synchronised,
        source,
        method: "w32tm".to_string(),
    })
}
