//! wifi collector — reports the connected state of each wireless interface.
//!
//! A full scan needs root and takes seconds, so this only reads the kernel's
//! own view (`/proc/net/wireless`) and enriches it with `iw dev <iface> link`
//! for SSID/BSSID/frequency when `iw` is available. An interface listed by
//! the kernel that `iw` reports as not associated is emitted with
//! `connected = false` — a powered radio that isn't joined to a network.

use crate::Options;
use crate::model::WifiNetwork;
use crate::section::Collected;

#[cfg(target_os = "linux")]
use std::process::Command;

/// Wi-Fi via `/proc/net/wireless` + `iw` is a Linux interface.
#[cfg(not(target_os = "linux"))]
pub(super) fn collect(_opts: &Options) -> Collected<Vec<WifiNetwork>> {
    Collected::unsupported("wifi via /proc + iw is Linux-only")
}

#[cfg(target_os = "linux")]
pub(super) fn collect(opts: &Options) -> Collected<Vec<WifiNetwork>> {
    let raw = match std::fs::read_to_string("/proc/net/wireless") {
        Ok(s) => s,
        // No /proc entry at all → this machine has no wireless hardware.
        Err(_) => return Collected::ok(Vec::new()),
    };

    let mut networks = Vec::new();
    // First two lines are headers (`Inter-|` / `face |`); skip them.
    for line in raw.lines().skip(2) {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        // `wlan0: 0000   70.  -40.  0  0 0 ...` — split at the first ':'.
        let Some((iface_part, rest)) = trimmed.split_once(':') else {
            continue;
        };
        let iface = iface_part.trim();
        if iface.is_empty() {
            continue;
        }
        // tokens: status, link quality (`70.`), level in dBm (`-40.`), ...
        let fields: Vec<&str> = rest.split_whitespace().collect();
        let signal_dbm = fields
            .get(2)
            .and_then(|tok| tok.trim_end_matches('.').parse::<i32>().ok());

        // Enrich with `iw` (SSID, BSSID, frequency → band/channel, bitrate).
        let (ssid, bssid, band, channel, link_mbps, connected) = iw_link(opts, iface);
        networks.push(WifiNetwork {
            interface: iface.to_string(),
            ssid,
            signal_dbm,
            link_mbps,
            band,
            channel,
            bssid,
            connected,
        });
    }

    Collected::ok(networks)
}

/// Everything `iw link` reports about one interface's association.
/// `(ssid, bssid, band, channel, link_mbps, connected)`.
#[cfg(target_os = "linux")]
type IwLink = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<u32>,
    Option<f64>,
    bool,
);

/// Ask `iw dev <iface> link` about association state.
///
/// Returns `(ssid, bssid, band, channel, link_mbps, connected)`; everything is
/// `None`/`false` when `iw` is absent, fails, or the radio isn't associated.
#[cfg(target_os = "linux")]
fn iw_link(opts: &Options, iface: &str) -> IwLink {
    let not_connected: IwLink = (None, None, None, None, None, false);
    let Ok(out) = Command::new("iw").args(["dev", iface, "link"]).output() else {
        return not_connected;
    };
    if !out.status.success() {
        return not_connected;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if !text.contains("Connected to") {
        return not_connected;
    }

    let mut ssid: Option<String> = None;
    let mut bssid: Option<String> = None;
    let mut band: Option<String> = None;
    let mut channel: Option<u32> = None;
    let mut link_mbps: Option<f64> = None;

    for line in text.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("Connected to ") {
            // `Connected to aa:bb:cc:dd:ee:ff (on wlan0)`
            if let Some(tok) = rest.split_whitespace().next() {
                bssid = Some(opts.privacy.mac(tok));
            }
        } else if let Some(rest) = line.strip_prefix("SSID: ") {
            ssid = Some(opts.privacy.hostname(rest.trim()));
        } else if let Some(rest) = line.strip_prefix("freq: ") {
            // `freq: 2412.0` (some kernels print a decimal); round to whole MHz.
            let freq = rest
                .split_whitespace()
                .next()
                .and_then(|tok| tok.parse::<f64>().ok())
                .map(|hz| hz.round() as u32);
            if let Some(freq) = freq {
                let (b, ch) = freq_to_band_channel(freq);
                band = b;
                channel = ch;
            }
        } else if let Some(rest) = line.strip_prefix("tx bitrate: ") {
            // `tx bitrate: 144.4 MBit/s`
            if let Some(tok) = rest.split_whitespace().next() {
                link_mbps = tok.parse::<f64>().ok();
            }
        }
    }

    (ssid, bssid, band, channel, link_mbps, true)
}

/// Map a MHz frequency to a band label and channel number.
#[cfg(target_os = "linux")]
fn freq_to_band_channel(freq: u32) -> (Option<String>, Option<u32>) {
    match freq {
        2400..=2500 => {
            let channel = if freq == 2484 {
                14
            } else {
                freq.saturating_sub(2407) / 5
            };
            (Some("2.4GHz".to_string()), Some(channel))
        }
        4900..=5900 => (
            Some("5GHz".to_string()),
            Some(freq.saturating_sub(5000) / 5),
        ),
        5925..=7125 => (
            Some("6GHz".to_string()),
            Some(freq.saturating_sub(5950) / 5),
        ),
        _ => (None, None),
    }
}
